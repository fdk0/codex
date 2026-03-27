use std::path::PathBuf;

use codex_protocol::ThreadId;
use codex_protocol::protocol::HookCompletedEvent;
use codex_protocol::protocol::HookEventName;
use codex_protocol::protocol::HookOutputEntry;
use codex_protocol::protocol::HookOutputEntryKind;
use codex_protocol::protocol::HookRunStatus;
use codex_protocol::protocol::HookRunSummary;

use super::common;
use crate::engine::CommandShell;
use crate::engine::ConfiguredHandler;
use crate::engine::command_runner::CommandRunResult;
use crate::engine::dispatcher;
use crate::engine::output_parser;
use crate::schema::AfterCompactionCommandInput;
use crate::schema::NullableString;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterCompactionSource {
    Manual,
    Auto,
    ModelSwitch,
}

impl AfterCompactionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Auto => "auto",
            Self::ModelSwitch => "modelSwitch",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AfterCompactionRequest {
    pub session_id: ThreadId,
    pub turn_id: String,
    pub cwd: PathBuf,
    pub transcript_path: Option<PathBuf>,
    pub active_profile: Option<String>,
    pub model: String,
    pub permission_mode: String,
    pub source: AfterCompactionSource,
}

#[derive(Debug)]
pub struct AfterCompactionOutcome {
    pub hook_events: Vec<HookCompletedEvent>,
    pub additional_contexts: Vec<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct AfterCompactionHandlerData {
    additional_contexts_for_model: Vec<String>,
}

pub(crate) fn preview(
    handlers: &[ConfiguredHandler],
    request: &AfterCompactionRequest,
) -> Vec<HookRunSummary> {
    dispatcher::select_handlers(
        handlers,
        HookEventName::AfterCompaction,
        Some(request.source.as_str()),
    )
    .into_iter()
    .map(|handler| dispatcher::running_summary(&handler))
    .collect()
}

pub(crate) async fn run(
    handlers: &[ConfiguredHandler],
    shell: &CommandShell,
    request: AfterCompactionRequest,
) -> AfterCompactionOutcome {
    let matched = dispatcher::select_handlers(
        handlers,
        HookEventName::AfterCompaction,
        Some(request.source.as_str()),
    );
    if matched.is_empty() {
        return AfterCompactionOutcome {
            hook_events: Vec::new(),
            additional_contexts: Vec::new(),
        };
    }

    let input_json = match serde_json::to_string(&AfterCompactionCommandInput {
        session_id: request.session_id.to_string(),
        turn_id: request.turn_id.clone(),
        transcript_path: NullableString::from_path(request.transcript_path.clone()),
        cwd: request.cwd.display().to_string(),
        hook_event_name: "AfterCompaction".to_string(),
        active_profile: NullableString::from_string(request.active_profile.clone()),
        model: request.model.clone(),
        permission_mode: request.permission_mode.clone(),
        source: request.source.as_str().to_string(),
    }) {
        Ok(input_json) => input_json,
        Err(error) => {
            return serialization_failure_outcome(common::serialization_failure_hook_events(
                matched,
                Some(request.turn_id),
                format!("failed to serialize after compaction hook input: {error}"),
            ));
        }
    };

    let results = dispatcher::execute_handlers(
        shell,
        matched,
        input_json,
        request.cwd.as_path(),
        Some(request.turn_id),
        parse_completed,
    )
    .await;

    let additional_contexts = common::flatten_additional_contexts(
        results
            .iter()
            .map(|result| result.data.additional_contexts_for_model.as_slice()),
    );

    AfterCompactionOutcome {
        hook_events: results.into_iter().map(|result| result.completed).collect(),
        additional_contexts,
    }
}

fn parse_completed(
    handler: &ConfiguredHandler,
    run_result: CommandRunResult,
    turn_id: Option<String>,
) -> dispatcher::ParsedHandler<AfterCompactionHandlerData> {
    let mut entries = Vec::new();
    let mut status = HookRunStatus::Completed;
    let mut additional_contexts_for_model = Vec::new();

    match run_result.error.as_deref() {
        Some(error) => {
            status = HookRunStatus::Failed;
            entries.push(HookOutputEntry {
                kind: HookOutputEntryKind::Error,
                text: error.to_string(),
            });
        }
        None => match run_result.exit_code {
            Some(0) => {
                let trimmed_stdout = run_result.stdout.trim();
                if trimmed_stdout.is_empty() {
                } else if let Some(parsed) =
                    output_parser::parse_after_compaction(&run_result.stdout)
                {
                    if let Some(system_message) = parsed.universal.system_message {
                        entries.push(HookOutputEntry {
                            kind: HookOutputEntryKind::Warning,
                            text: system_message,
                        });
                    }
                    if let Some(additional_context) = parsed.additional_context {
                        common::append_additional_context(
                            &mut entries,
                            &mut additional_contexts_for_model,
                            additional_context,
                        );
                    }
                    let _ = parsed.universal.continue_processing;
                    let _ = parsed.universal.stop_reason;
                    let _ = parsed.universal.suppress_output;
                } else if trimmed_stdout.starts_with('{') || trimmed_stdout.starts_with('[') {
                    status = HookRunStatus::Failed;
                    entries.push(HookOutputEntry {
                        kind: HookOutputEntryKind::Error,
                        text: "hook returned invalid after compaction JSON output".to_string(),
                    });
                } else {
                    let additional_context = trimmed_stdout.to_string();
                    common::append_additional_context(
                        &mut entries,
                        &mut additional_contexts_for_model,
                        additional_context,
                    );
                }
            }
            Some(exit_code) => {
                status = HookRunStatus::Failed;
                entries.push(HookOutputEntry {
                    kind: HookOutputEntryKind::Error,
                    text: format!("hook exited with code {exit_code}"),
                });
                if let Some(stderr) = common::trimmed_non_empty(&run_result.stderr) {
                    entries.push(HookOutputEntry {
                        kind: HookOutputEntryKind::Error,
                        text: stderr,
                    });
                }
            }
            None => {
                status = HookRunStatus::Failed;
                entries.push(HookOutputEntry {
                    kind: HookOutputEntryKind::Error,
                    text: "hook terminated without an exit code".to_string(),
                });
            }
        },
    }

    let completed = HookCompletedEvent {
        turn_id,
        run: dispatcher::completed_summary(handler, &run_result, status, entries),
    };

    dispatcher::ParsedHandler {
        completed,
        data: AfterCompactionHandlerData {
            additional_contexts_for_model,
        },
    }
}

fn serialization_failure_outcome(hook_events: Vec<HookCompletedEvent>) -> AfterCompactionOutcome {
    AfterCompactionOutcome {
        hook_events,
        additional_contexts: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use codex_protocol::protocol::HookEventName;
    use pretty_assertions::assert_eq;

    use crate::engine::ConfiguredHandler;

    use super::AfterCompactionRequest;
    use super::AfterCompactionSource;
    use super::preview;

    fn handler(matcher: Option<&str>) -> ConfiguredHandler {
        ConfiguredHandler {
            event_name: HookEventName::AfterCompaction,
            matcher: matcher.map(str::to_owned),
            command: "echo ok".to_string(),
            timeout_sec: 5,
            status_message: None,
            source_path: PathBuf::from("/tmp/hooks.json"),
            display_order: 0,
        }
    }

    fn request(source: AfterCompactionSource) -> AfterCompactionRequest {
        AfterCompactionRequest {
            session_id: codex_protocol::ThreadId::new(),
            turn_id: "turn-1".to_string(),
            cwd: PathBuf::from("/tmp"),
            transcript_path: None,
            active_profile: Some("bd-worker".to_string()),
            model: "gpt-test".to_string(),
            permission_mode: "default".to_string(),
            source,
        }
    }

    #[test]
    fn preview_filters_after_compaction_by_source() {
        let handlers = vec![handler(Some("^auto$")), handler(Some("^manual$"))];

        let runs = preview(&handlers, &request(AfterCompactionSource::Auto));

        assert_eq!(runs.len(), 1);
    }
}
