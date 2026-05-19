use std::path::Path;

use futures::StreamExt;
use futures::stream::FuturesUnordered;

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
    pub completion_order: usize,
}

pub(crate) fn select_handlers(
    handlers: &[ConfiguredHandler],
    selection: HookSelectionContext<'_>,
) -> Vec<ConfiguredHandler> {
    let matcher_inputs = selection.matcher_input.into_iter().collect::<Vec<_>>();
    select_handlers_for_matcher_inputs(handlers, selection, &matcher_inputs)
}

pub(crate) fn select_handlers_for_matcher_inputs(
    handlers: &[ConfiguredHandler],
    selection: HookSelectionContext<'_>,
    matcher_inputs: &[&str],
) -> Vec<ConfiguredHandler> {
    // Check each configured handler once, even when several compatibility names
    // match the same regex. A hook like `apply_patch|Write|Edit` should run a
    // single time for one tool call, not once per matching alias.
    handlers
        .iter()
        .filter(|handler| handler.event_name == selection.event_name)
        .filter(|handler| matches_conditions(handler, selection))
        .filter(|handler| match selection.event_name {
            HookEventName::PreToolUse
            | HookEventName::PermissionRequest
            | HookEventName::PostToolUse
            | HookEventName::SessionStart
            | HookEventName::PreCompact
            | HookEventName::PostCompact
            | HookEventName::AfterCompaction => {
                if matcher_inputs.is_empty() {
                    matches_matcher(handler.matcher.as_deref(), /*input*/ None)
                } else {
                    matcher_inputs
                        .iter()
                        .any(|input| matches_matcher(handler.matcher.as_deref(), Some(input)))
                }
            }
            HookEventName::UserPromptSubmit | HookEventName::Stop => true,
        })
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

pub(crate) fn running_summary(handler: &ConfiguredHandler) -> HookRunSummary {
    HookRunSummary {
        id: handler.run_id(),
        event_name: handler.event_name,
        handler_type: HookHandlerType::Command,
        execution_mode: HookExecutionMode::Sync,
        scope: scope_for_event(handler.event_name),
        source_path: handler.source_path.clone(),
        source: handler.source,
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
    let mut pending = FuturesUnordered::new();
    for (configured_order, handler) in handlers.into_iter().enumerate() {
        let input_json = input_json.clone();
        let turn_id = turn_id.clone();
        pending.push(async move {
            let result = run_command(shell, &handler, &input_json, cwd).await;
            (configured_order, parse(&handler, result, turn_id))
        });
    }

    let mut completed = Vec::new();
    let mut completion_order = 0;
    while let Some((configured_order, mut parsed)) = pending.next().await {
        parsed.completion_order = completion_order;
        completion_order += 1;
        completed.push((configured_order, parsed));
    }
    completed.sort_by_key(|(configured_order, _)| *configured_order);
    completed.into_iter().map(|(_, parsed)| parsed).collect()
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
        source: handler.source,
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
        | HookEventName::PermissionRequest
        | HookEventName::PostToolUse
        | HookEventName::PreCompact
        | HookEventName::PostCompact
        | HookEventName::AfterCompaction
        | HookEventName::UserPromptSubmit
        | HookEventName::Stop => HookScope::Turn,
    }
}

#[cfg(test)]
mod tests {
    use codex_config::HookConditions;
    use codex_protocol::protocol::HookEventName;
    use codex_protocol::protocol::HookSource;
    use codex_utils_absolute_path::test_support::PathBufExt;
    use codex_utils_absolute_path::test_support::test_path_buf;

    use super::ConfiguredHandler;
    use super::HookSelectionContext;
    use super::select_handlers;
    use super::select_handlers_for_matcher_inputs;

    fn make_handler(
        event_name: HookEventName,
        matcher: Option<&str>,
        command: &str,
        display_order: i64,
    ) -> ConfiguredHandler {
        ConfiguredHandler {
            event_name,
            matcher: matcher.map(str::to_owned),
            conditions: HookConditions::default(),
            command: command.to_string(),
            timeout_sec: 5,
            status_message: None,
            source_path: test_path_buf("/tmp/hooks.json").abs(),
            source: HookSource::User,
            display_order,
            env: std::collections::HashMap::new(),
        }
    }

    fn make_handler_with_conditions(
        event_name: HookEventName,
        command: &str,
        display_order: i64,
        conditions: HookConditions,
    ) -> ConfiguredHandler {
        ConfiguredHandler {
            event_name,
            matcher: None,
            conditions,
            command: command.to_string(),
            timeout_sec: 5,
            status_message: None,
            source_path: test_path_buf("/tmp/hooks.json").abs(),
            source: HookSource::User,
            display_order,
            env: std::collections::HashMap::new(),
        }
    }

    fn selection(
        event_name: HookEventName,
        matcher_input: Option<&str>,
    ) -> HookSelectionContext<'_> {
        HookSelectionContext {
            event_name,
            matcher_input,
            active_profile: None,
            model: None,
            permission_mode: None,
        }
    }

    fn selection_with_conditions<'a>(
        event_name: HookEventName,
        active_profile: Option<&'a str>,
        model: Option<&'a str>,
        permission_mode: Option<&'a str>,
    ) -> HookSelectionContext<'a> {
        HookSelectionContext {
            event_name,
            matcher_input: None,
            active_profile,
            model,
            permission_mode,
        }
    }

    #[test]
    fn select_handlers_keeps_duplicate_stop_handlers() {
        let handlers = vec![
            make_handler(
                HookEventName::Stop,
                /*matcher*/ None,
                "echo same",
                /*display_order*/ 0,
            ),
            make_handler(
                HookEventName::Stop,
                /*matcher*/ None,
                "echo same",
                /*display_order*/ 1,
            ),
        ];

        let selected = select_handlers(&handlers, selection(HookEventName::Stop, None));

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
                "echo same",
                /*display_order*/ 0,
            ),
            make_handler(
                HookEventName::SessionStart,
                Some("^startup$"),
                "echo same",
                /*display_order*/ 1,
            ),
        ];

        let selected = select_handlers(
            &handlers,
            selection(HookEventName::SessionStart, Some("startup")),
        );

        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].display_order, 0);
        assert_eq!(selected[1].display_order, 1);
    }

    #[test]
    fn compact_hooks_match_trigger() {
        let handlers = vec![
            make_handler(
                HookEventName::PreCompact,
                Some("manual"),
                "echo manual",
                /*display_order*/ 0,
            ),
            make_handler(
                HookEventName::PreCompact,
                Some("auto"),
                "echo auto",
                /*display_order*/ 1,
            ),
        ];

        let selected = select_handlers(
            &handlers,
            selection(HookEventName::PreCompact, Some("manual")),
        );

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].display_order, 0);
    }

    #[test]
    fn select_handlers_filters_by_active_profile_conditions() {
        let handlers = vec![
            make_handler_with_conditions(
                HookEventName::PostToolUse,
                "echo default",
                /*display_order*/ 0,
                HookConditions::default(),
            ),
            make_handler_with_conditions(
                HookEventName::PostToolUse,
                "echo assistant",
                /*display_order*/ 1,
                HookConditions {
                    profile: Some("assistant".to_string()),
                    ..HookConditions::default()
                },
            ),
            make_handler_with_conditions(
                HookEventName::PostToolUse,
                "echo bd",
                /*display_order*/ 2,
                HookConditions {
                    profiles: vec!["bd-driver".to_string(), "bd-worker".to_string()],
                    ..HookConditions::default()
                },
            ),
        ];

        let selected = select_handlers(
            &handlers,
            selection_with_conditions(
                HookEventName::PostToolUse,
                Some("bd-worker"),
                /*model*/ None,
                /*permission_mode*/ None,
            ),
        );

        assert_eq!(
            selected
                .iter()
                .map(|handler| handler.display_order)
                .collect::<Vec<_>>(),
            vec![0, 2],
        );
    }

    #[test]
    fn select_handlers_requires_all_declared_conditions_to_match() {
        let handlers = vec![make_handler_with_conditions(
            HookEventName::PermissionRequest,
            "echo matched",
            /*display_order*/ 0,
            HookConditions {
                profile: Some("bd-worker".to_string()),
                model: Some("gpt-5.5".to_string()),
                permission_mode: Some("workspace-write".to_string()),
                ..HookConditions::default()
            },
        )];

        let selected = select_handlers(
            &handlers,
            selection_with_conditions(
                HookEventName::PermissionRequest,
                Some("bd-worker"),
                Some("gpt-5.5"),
                Some("workspace-write"),
            ),
        );
        let wrong_profile = select_handlers(
            &handlers,
            selection_with_conditions(
                HookEventName::PermissionRequest,
                Some("assistant"),
                Some("gpt-5.5"),
                Some("workspace-write"),
            ),
        );

        assert_eq!(selected.len(), 1);
        assert_eq!(wrong_profile.len(), 0);
    }

    #[test]
    fn pre_tool_use_matches_tool_name() {
        let handlers = vec![
            make_handler(
                HookEventName::PreToolUse,
                Some("^Bash$"),
                "echo same",
                /*display_order*/ 0,
            ),
            make_handler(
                HookEventName::PreToolUse,
                Some("^Edit$"),
                "echo same",
                /*display_order*/ 1,
            ),
        ];

        let selected = select_handlers(
            &handlers,
            selection(HookEventName::PreToolUse, Some("Bash")),
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
                "echo same",
                /*display_order*/ 0,
            ),
            make_handler(
                HookEventName::PostToolUse,
                Some("^Edit$"),
                "echo same",
                /*display_order*/ 1,
            ),
        ];

        let selected = select_handlers(
            &handlers,
            selection(HookEventName::PostToolUse, Some("Bash")),
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
                "echo same",
                /*display_order*/ 0,
            ),
            make_handler(
                HookEventName::PreToolUse,
                Some("^Edit$"),
                "echo same",
                /*display_order*/ 1,
            ),
        ];

        let selected = select_handlers(
            &handlers,
            selection(HookEventName::PreToolUse, Some("Bash")),
        );

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].display_order, 0);
    }

    #[test]
    fn pre_tool_use_regex_alternation_matches_each_tool_name() {
        let handlers = vec![make_handler(
            HookEventName::PreToolUse,
            Some("Edit|Write"),
            "echo same",
            /*display_order*/ 0,
        )];

        let selected_edit = select_handlers(
            &handlers,
            selection(HookEventName::PreToolUse, Some("Edit")),
        );
        let selected_write = select_handlers(
            &handlers,
            selection(HookEventName::PreToolUse, Some("Write")),
        );
        let selected_bash = select_handlers(
            &handlers,
            selection(HookEventName::PreToolUse, Some("Bash")),
        );

        assert_eq!(selected_edit.len(), 1);
        assert_eq!(selected_write.len(), 1);
        assert_eq!(selected_bash.len(), 0);
    }

    #[test]
    fn pre_tool_use_aliases_match_once_per_handler() {
        let handlers = vec![
            make_handler(
                HookEventName::PreToolUse,
                Some("^apply_patch$"),
                "echo apply_patch",
                /*display_order*/ 0,
            ),
            make_handler(
                HookEventName::PreToolUse,
                Some("^Write$"),
                "echo write",
                /*display_order*/ 1,
            ),
            make_handler(
                HookEventName::PreToolUse,
                Some("^Edit$"),
                "echo edit",
                /*display_order*/ 2,
            ),
            make_handler(
                HookEventName::PreToolUse,
                Some("apply_patch|Write|Edit"),
                "echo combined",
                /*display_order*/ 3,
            ),
        ];

        let selected = select_handlers_for_matcher_inputs(
            &handlers,
            selection(HookEventName::PreToolUse, Some("apply_patch")),
            &["apply_patch", "Write", "Edit"],
        );

        assert_eq!(selected.len(), 4);
        assert_eq!(
            selected
                .iter()
                .map(|handler| handler.display_order)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3],
        );
    }

    #[test]
    fn user_prompt_submit_ignores_matcher() {
        let handlers = vec![
            make_handler(
                HookEventName::UserPromptSubmit,
                Some("^hello"),
                "echo first",
                /*display_order*/ 0,
            ),
            make_handler(
                HookEventName::UserPromptSubmit,
                Some("["),
                "echo second",
                /*display_order*/ 1,
            ),
        ];

        let selected = select_handlers(&handlers, selection(HookEventName::UserPromptSubmit, None));

        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].display_order, 0);
        assert_eq!(selected[1].display_order, 1);
    }

    #[test]
    fn select_handlers_preserves_declaration_order() {
        let handlers = vec![
            make_handler(
                HookEventName::Stop,
                /*matcher*/ None,
                "first",
                /*display_order*/ 0,
            ),
            make_handler(
                HookEventName::Stop,
                /*matcher*/ None,
                "second",
                /*display_order*/ 1,
            ),
            make_handler(
                HookEventName::Stop,
                /*matcher*/ None,
                "third",
                /*display_order*/ 2,
            ),
        ];

        let selected = select_handlers(&handlers, selection(HookEventName::Stop, None));

        assert_eq!(selected.len(), 3);
        assert_eq!(selected[0].command, "first");
        assert_eq!(selected[1].command, "second");
        assert_eq!(selected[2].command, "third");
    }
}
