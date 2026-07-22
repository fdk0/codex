use std::collections::HashMap;

use codex_config::HookConditions;
use codex_protocol::protocol::HookEventName;
use codex_protocol::protocol::HookRunStatus;
use codex_protocol::protocol::HookSource;
use codex_utils_absolute_path::test_support::PathBufExt;
use codex_utils_absolute_path::test_support::test_path_buf;
use pretty_assertions::assert_eq;

use super::SessionEndRequest;
use super::parse_completed;
use super::preview;
use crate::engine::ConfiguredHandler;
use crate::engine::command_runner::CommandRunResult;

#[test]
fn session_end_matches_other_reason() {
    let selected = preview(
        &[
            ConfiguredHandler {
                display_order: 0,
                ..handler(Some("clear"))
            },
            ConfiguredHandler {
                display_order: 1,
                ..handler(Some("other"))
            },
            ConfiguredHandler {
                display_order: 2,
                ..handler(/*matcher*/ None)
            },
        ],
        &request(),
    );

    assert_eq!(
        selected
            .iter()
            .map(|run| run.display_order)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn session_end_ignores_successful_output() {
    let completed = parse_completed(
        &handler(/*matcher*/ None),
        CommandRunResult {
            started_at: 1,
            completed_at: 2,
            duration_ms: 1,
            exit_code: Some(0),
            stdout: r#"{"continue":false,"decision":"block","reason":"ignored"}"#.to_string(),
            stderr: String::new(),
            error: None,
        },
        /*turn_id*/ None,
    );

    assert_eq!(completed.completed.run.status, HookRunStatus::Completed);
    assert_eq!(completed.completed.run.entries, Vec::new());
}

#[test]
fn session_end_filters_on_active_turn_selection_context() {
    let mut matching = handler(/*matcher*/ None);
    matching.conditions = HookConditions {
        profile: Some("assistant".to_string()),
        model: Some("gpt-test".to_string()),
        permission_mode: Some("default".to_string()),
        ..Default::default()
    };
    let mut wrong_profile = matching.clone();
    wrong_profile.display_order = 1;
    wrong_profile.conditions.profile = Some("bd-worker".to_string());

    let selected = preview(&[matching, wrong_profile], &request());

    assert_eq!(
        selected
            .iter()
            .map(|run| run.display_order)
            .collect::<Vec<_>>(),
        vec![0]
    );
}

fn handler(matcher: Option<&str>) -> ConfiguredHandler {
    ConfiguredHandler {
        event_name: HookEventName::SessionEnd,
        matcher: matcher.map(str::to_string),
        conditions: HookConditions::default(),
        command: "echo hook".to_string(),
        timeout_sec: 2,
        status_message: None,
        additional_context_limit: Default::default(),
        source_path: test_path_buf("/tmp/hooks.json").abs(),
        source: HookSource::User,
        display_order: 0,
        env: HashMap::new(),
    }
}

fn request() -> SessionEndRequest {
    SessionEndRequest {
        session_id: codex_protocol::ThreadId::new(),
        turn_id: "turn-1".to_string(),
        cwd: test_path_buf("/tmp").abs(),
        transcript_path: None,
        active_profile: Some("assistant".to_string()),
        model: "gpt-test".to_string(),
        permission_mode: "default".to_string(),
    }
}
