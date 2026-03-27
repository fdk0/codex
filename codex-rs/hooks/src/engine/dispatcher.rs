use std::path::Path;

use futures::future::join_all;

use codex_protocol::protocol::HookCompletedEvent;
use codex_protocol::protocol::HookEventName;
use codex_protocol::protocol::HookExecutionMode;
use codex_protocol::protocol::HookHandlerType;
use codex_protocol::protocol::HookRunStatus;
use codex_protocol::protocol::HookRunSummary;
use codex_protocol::protocol::HookScope;

use super::CommandShell;
use super::ConfiguredHandler;
use super::command_runner::CommandRunResult;
use super::command_runner::run_command;
use crate::events::common::matches_matcher;

#[derive(Clone, Copy, Debug)]
pub(crate) struct HookSelectionContext<'a> {
    pub event_name: HookEventName,
    pub matcher_input: Option<&'a str>,
    pub active_profile: Option<&'a str>,
    pub model: Option<&'a str>,
    pub permission_mode: Option<&'a str>,
}

#[derive(Debug)]
pub(crate) struct ParsedHandler<T> {
    pub completed: HookCompletedEvent,
    pub data: T,
}

pub(crate) fn select_handlers(
    handlers: &[ConfiguredHandler],
    selection: HookSelectionContext<'_>,
) -> Vec<ConfiguredHandler> {
    handlers
        .iter()
        .filter(|handler| handler.event_name == selection.event_name)
        .filter(|handler| matches_conditions(handler, selection))
        .filter(|handler| matches_handler_matcher(handler, selection.matcher_input))
        .cloned()
        .collect()
}

fn matches_conditions(handler: &ConfiguredHandler, selection: HookSelectionContext<'_>) -> bool {
    matches_condition_value(
        selection.active_profile,
        handler.conditions.profile.as_deref(),
        &handler.conditions.profiles,
    ) && matches_condition_value(
        selection.model,
        handler.conditions.model.as_deref(),
        &handler.conditions.models,
    ) && matches_condition_value(
        selection.permission_mode,
        handler.conditions.permission_mode.as_deref(),
        &handler.conditions.permission_modes,
    )
}

fn matches_condition_value(actual: Option<&str>, single: Option<&str>, many: &[String]) -> bool {
    if single.is_none() && many.is_empty() {
        return true;
    }

    let Some(actual) = actual else {
        return false;
    };

    single.is_some_and(|expected| expected == actual)
        || many.iter().any(|expected| expected == actual)
}

fn matches_handler_matcher(handler: &ConfiguredHandler, matcher_input: Option<&str>) -> bool {
    matches_matcher(handler.matcher.as_deref(), matcher_input)
}

pub(crate) fn running_summary(handler: &ConfiguredHandler) -> HookRunSummary {
    HookRunSummary {
        id: handler.run_id(),
        event_name: handler.event_name,
        handler_type: HookHandlerType::Command,
        execution_mode: HookExecutionMode::Sync,
        scope: scope_for_event(handler.event_name),
        source_path: handler.source_path.clone(),
        display_order: handler.display_order,
        status: HookRunStatus::Running,
        status_message: handler.status_message.clone(),
        started_at: chrono::Utc::now().timestamp(),
        completed_at: None,
        duration_ms: None,
        entries: Vec::new(),
    }
}

pub(crate) async fn execute_handlers<T>(
    shell: &CommandShell,
    handlers: Vec<ConfiguredHandler>,
    input_json: String,
    cwd: &Path,
    turn_id: Option<String>,
    parse: fn(&ConfiguredHandler, CommandRunResult, Option<String>) -> ParsedHandler<T>,
) -> Vec<ParsedHandler<T>> {
    let results = join_all(
        handlers
            .iter()
            .map(|handler| run_command(shell, handler, &input_json, cwd)),
    )
    .await;

    handlers
        .into_iter()
        .zip(results)
        .map(|(handler, result)| parse(&handler, result, turn_id.clone()))
        .collect()
}

pub(crate) fn completed_summary(
    handler: &ConfiguredHandler,
    run_result: &CommandRunResult,
    status: HookRunStatus,
    entries: Vec<codex_protocol::protocol::HookOutputEntry>,
) -> HookRunSummary {
    HookRunSummary {
        id: handler.run_id(),
        event_name: handler.event_name,
        handler_type: HookHandlerType::Command,
        execution_mode: HookExecutionMode::Sync,
        scope: scope_for_event(handler.event_name),
        source_path: handler.source_path.clone(),
        display_order: handler.display_order,
        status,
        status_message: handler.status_message.clone(),
        started_at: run_result.started_at,
        completed_at: Some(run_result.completed_at),
        duration_ms: Some(run_result.duration_ms),
        entries,
    }
}

fn scope_for_event(event_name: HookEventName) -> HookScope {
    match event_name {
        HookEventName::SessionStart => HookScope::Thread,
        HookEventName::PreToolUse
        | HookEventName::PostToolUse
        | HookEventName::AfterCompaction
        | HookEventName::UserPromptSubmit
        | HookEventName::Stop => HookScope::Turn,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use codex_protocol::protocol::HookEventName;
    use pretty_assertions::assert_eq;

    use crate::engine::config::HookConditions;

    use super::ConfiguredHandler;
    use super::HookSelectionContext;
    use super::select_handlers;

    fn make_handler(
        event_name: HookEventName,
        matcher: Option<&str>,
        conditions: HookConditions,
        command: &str,
        display_order: i64,
    ) -> ConfiguredHandler {
        ConfiguredHandler {
            event_name,
            matcher: matcher.map(str::to_owned),
            conditions,
            command: command.to_string(),
            timeout_sec: 5,
            status_message: None,
            source_path: PathBuf::from("/tmp/hooks.json"),
            display_order,
        }
    }

    #[test]
    fn select_handlers_keeps_duplicate_stop_handlers() {
        let handlers = vec![
            make_handler(
                HookEventName::Stop,
                None,
                HookConditions::default(),
                "echo same",
                0,
            ),
            make_handler(
                HookEventName::Stop,
                None,
                HookConditions::default(),
                "echo same",
                1,
            ),
        ];

        let selected = select_handlers(
            &handlers,
            HookSelectionContext {
                event_name: HookEventName::Stop,
                matcher_input: None,
                active_profile: None,
                model: None,
                permission_mode: None,
            },
        );

        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].display_order, 0);
        assert_eq!(selected[1].display_order, 1);
    }

    #[test]
    fn select_handlers_keeps_overlapping_session_start_matchers() {
        let handlers = vec![
            make_handler(
                HookEventName::SessionStart,
                Some("start.*"),
                HookConditions::default(),
                "echo same",
                0,
            ),
            make_handler(
                HookEventName::SessionStart,
                Some("^startup$"),
                HookConditions::default(),
                "echo same",
                1,
            ),
        ];

        let selected = select_handlers(
            &handlers,
            HookSelectionContext {
                event_name: HookEventName::SessionStart,
                matcher_input: Some("startup"),
                active_profile: None,
                model: None,
                permission_mode: None,
            },
        );

        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].display_order, 0);
        assert_eq!(selected[1].display_order, 1);
    }

    #[test]
    fn pre_tool_use_matches_tool_name() {
        let handlers = vec![
            make_handler(
                HookEventName::PreToolUse,
                Some("^Bash$"),
                HookConditions::default(),
                "echo same",
                0,
            ),
            make_handler(
                HookEventName::PreToolUse,
                Some("^Edit$"),
                HookConditions::default(),
                "echo same",
                1,
            ),
        ];

        let selected = select_handlers(
            &handlers,
            HookSelectionContext {
                event_name: HookEventName::PreToolUse,
                matcher_input: Some("Bash"),
                active_profile: None,
                model: None,
                permission_mode: None,
            },
        );

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].display_order, 0);
    }

    #[test]
    fn pre_tool_use_star_matcher_matches_all_tools() {
        let handlers = vec![
            make_handler(
                HookEventName::PreToolUse,
                Some("*"),
                HookConditions::default(),
                "echo same",
                0,
            ),
            make_handler(
                HookEventName::PreToolUse,
                Some("^Edit$"),
                HookConditions::default(),
                "echo same",
                1,
            ),
        ];

        let selected = select_handlers(
            &handlers,
            HookSelectionContext {
                event_name: HookEventName::PreToolUse,
                matcher_input: Some("Bash"),
                active_profile: None,
                model: None,
                permission_mode: None,
            },
        );

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].display_order, 0);
    }

    #[test]
    fn post_tool_use_matches_tool_name() {
        let handlers = vec![
            make_handler(
                HookEventName::PostToolUse,
                Some("^Bash$"),
                HookConditions::default(),
                "echo same",
                0,
            ),
            make_handler(
                HookEventName::PostToolUse,
                Some("^Edit$"),
                HookConditions::default(),
                "echo same",
                1,
            ),
        ];

        let selected = select_handlers(
            &handlers,
            HookSelectionContext {
                event_name: HookEventName::PostToolUse,
                matcher_input: Some("Bash"),
                active_profile: None,
                model: None,
                permission_mode: None,
            },
        );

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].display_order, 0);
    }

    #[test]
    fn pre_tool_use_regex_alternation_matches_each_tool_name() {
        let handlers = vec![make_handler(
            HookEventName::PreToolUse,
            Some("Edit|Write"),
            HookConditions::default(),
            "echo same",
            0,
        )];

        let selected_edit = select_handlers(
            &handlers,
            HookSelectionContext {
                event_name: HookEventName::PreToolUse,
                matcher_input: Some("Edit"),
                active_profile: None,
                model: None,
                permission_mode: None,
            },
        );
        let selected_write = select_handlers(
            &handlers,
            HookSelectionContext {
                event_name: HookEventName::PreToolUse,
                matcher_input: Some("Write"),
                active_profile: None,
                model: None,
                permission_mode: None,
            },
        );
        let selected_bash = select_handlers(
            &handlers,
            HookSelectionContext {
                event_name: HookEventName::PreToolUse,
                matcher_input: Some("Bash"),
                active_profile: None,
                model: None,
                permission_mode: None,
            },
        );

        assert_eq!(selected_edit.len(), 1);
        assert_eq!(selected_write.len(), 1);
        assert_eq!(selected_bash.len(), 0);
    }

    #[test]
    fn user_prompt_submit_respects_matcher() {
        let handlers = vec![
            make_handler(
                HookEventName::UserPromptSubmit,
                Some("^hello"),
                HookConditions::default(),
                "echo first",
                0,
            ),
            make_handler(
                HookEventName::UserPromptSubmit,
                Some("^bye"),
                HookConditions::default(),
                "echo second",
                1,
            ),
        ];

        let selected = select_handlers(
            &handlers,
            HookSelectionContext {
                event_name: HookEventName::UserPromptSubmit,
                matcher_input: Some("hello world"),
                active_profile: None,
                model: None,
                permission_mode: None,
            },
        );

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].display_order, 0);
    }

    #[test]
    fn select_handlers_preserves_declaration_order() {
        let handlers = vec![
            make_handler(
                HookEventName::Stop,
                None,
                HookConditions::default(),
                "first",
                0,
            ),
            make_handler(
                HookEventName::Stop,
                None,
                HookConditions::default(),
                "second",
                1,
            ),
            make_handler(
                HookEventName::Stop,
                None,
                HookConditions::default(),
                "third",
                2,
            ),
        ];

        let selected = select_handlers(
            &handlers,
            HookSelectionContext {
                event_name: HookEventName::Stop,
                matcher_input: None,
                active_profile: None,
                model: None,
                permission_mode: None,
            },
        );

        assert_eq!(selected.len(), 3);
        assert_eq!(selected[0].command, "first");
        assert_eq!(selected[1].command, "second");
        assert_eq!(selected[2].command, "third");
    }

    #[test]
    fn select_handlers_allows_general_and_profile_scoped_hooks_together() {
        let handlers = vec![
            make_handler(
                HookEventName::UserPromptSubmit,
                None,
                HookConditions::default(),
                "general",
                0,
            ),
            make_handler(
                HookEventName::UserPromptSubmit,
                None,
                HookConditions {
                    profile: Some("worker-profile".to_string()),
                    ..HookConditions::default()
                },
                "scoped",
                1,
            ),
        ];

        let selected = select_handlers(
            &handlers,
            HookSelectionContext {
                event_name: HookEventName::UserPromptSubmit,
                matcher_input: Some("check this"),
                active_profile: Some("worker-profile"),
                model: None,
                permission_mode: None,
            },
        );

        assert_eq!(
            selected
                .iter()
                .map(|handler| handler.command.as_str())
                .collect::<Vec<_>>(),
            vec!["general", "scoped"],
        );
    }

    #[test]
    fn select_handlers_rejects_profile_scoped_hook_for_other_profiles() {
        let handlers = vec![make_handler(
            HookEventName::Stop,
            None,
            HookConditions {
                profiles: vec!["review".to_string(), "worker-profile".to_string()],
                ..HookConditions::default()
            },
            "scoped",
            0,
        )];

        let selected = select_handlers(
            &handlers,
            HookSelectionContext {
                event_name: HookEventName::Stop,
                matcher_input: Some("done"),
                active_profile: Some("default"),
                model: None,
                permission_mode: None,
            },
        );

        assert!(selected.is_empty());
    }

    #[test]
    fn select_handlers_supports_model_and_permission_mode_conditions() {
        let handlers = vec![
            make_handler(
                HookEventName::SessionStart,
                Some("^resume$"),
                HookConditions {
                    models: vec!["gpt-5".to_string()],
                    permission_modes: vec!["default".to_string()],
                    ..HookConditions::default()
                },
                "scoped",
                0,
            ),
            make_handler(
                HookEventName::SessionStart,
                Some("^resume$"),
                HookConditions {
                    models: vec!["gpt-4.1".to_string()],
                    permission_modes: vec!["default".to_string()],
                    ..HookConditions::default()
                },
                "other-model",
                1,
            ),
        ];

        let selected = select_handlers(
            &handlers,
            HookSelectionContext {
                event_name: HookEventName::SessionStart,
                matcher_input: Some("resume"),
                active_profile: None,
                model: Some("gpt-5"),
                permission_mode: Some("default"),
            },
        );

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].command, "scoped");
    }
}
