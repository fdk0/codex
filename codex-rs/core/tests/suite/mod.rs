// Aggregates all former standalone integration tests as modules.
use std::ffi::OsString;

use codex_arg0::Arg0PathEntryGuard;
use codex_arg0::arg0_dispatch;
use ctor::ctor;
use tempfile::TempDir;

struct TestCodexAliasesGuard {
    _codex_home: TempDir,
    _arg0: Arg0PathEntryGuard,
    _previous_codex_home: Option<OsString>,
    _previous_home: Option<OsString>,
    _previous_rust_min_stack: Option<OsString>,
    _previous_zdotdir: Option<OsString>,
}

const CODEX_HOME_ENV_VAR: &str = "CODEX_HOME";
const HOME_ENV_VAR: &str = "HOME";
const RUST_MIN_STACK_ENV_VAR: &str = "RUST_MIN_STACK";
const ZDOTDIR_ENV_VAR: &str = "ZDOTDIR";
const LARGE_TEST_STACK_SIZE_BYTES: &str = "33554432";

// This code runs before any other tests are run.
// It allows the test binary to behave like codex and dispatch to apply_patch and codex-linux-sandbox
// based on the arg0.
// NOTE: this doesn't work on ARM
#[ctor]
pub static CODEX_ALIASES_TEMP_DIR: TestCodexAliasesGuard = unsafe {
    #[allow(clippy::unwrap_used)]
    let codex_home = tempfile::Builder::new()
        .prefix("codex-core-tests")
        .tempdir()
        .unwrap();
    let previous_codex_home = std::env::var_os(CODEX_HOME_ENV_VAR);
    let previous_home = std::env::var_os(HOME_ENV_VAR);
    let previous_rust_min_stack = std::env::var_os(RUST_MIN_STACK_ENV_VAR);
    let previous_zdotdir = std::env::var_os(ZDOTDIR_ENV_VAR);
    // arg0_dispatch() creates helper links under CODEX_HOME/tmp. Point it at a
    // test-owned temp dir so startup never mutates the developer's real ~/.codex.
    //
    // Also point HOME/ZDOTDIR at the same test-owned temp dir for the lifetime
    // of the suite binary so shell snapshot/login-shell tests do not source the
    // developer's real shell startup files. That keeps the suite hermetic and
    // avoids spawning dozens of slow snapshot shells under the real ~/.zshrc.
    //
    // Safety: #[ctor] runs before tests start, so no test threads exist yet.
    unsafe {
        std::env::set_var(CODEX_HOME_ENV_VAR, codex_home.path());
        std::env::set_var(HOME_ENV_VAR, codex_home.path());
        std::env::set_var(RUST_MIN_STACK_ENV_VAR, LARGE_TEST_STACK_SIZE_BYTES);
        std::env::set_var(ZDOTDIR_ENV_VAR, codex_home.path());
    }

    #[allow(clippy::unwrap_used)]
    let arg0 = arg0_dispatch().unwrap();
    // Restore the process environment immediately so later tests observe the
    // same CODEX_HOME state they started with.
    match previous_codex_home.as_ref() {
        Some(value) => unsafe {
            std::env::set_var(CODEX_HOME_ENV_VAR, value);
        },
        None => unsafe {
            std::env::remove_var(CODEX_HOME_ENV_VAR);
        },
    }

    TestCodexAliasesGuard {
        _codex_home: codex_home,
        _arg0: arg0,
        _previous_codex_home: previous_codex_home,
        _previous_home: previous_home,
        _previous_rust_min_stack: previous_rust_min_stack,
        _previous_zdotdir: previous_zdotdir,
    }
};

#[cfg(not(target_os = "windows"))]
mod abort_tasks;
mod agent_jobs;
mod agent_websocket;
mod apply_patch_cli;
#[cfg(not(target_os = "windows"))]
mod approvals;
mod auth_refresh;
mod cli_stream;
mod client;
mod client_websockets;
mod code_mode;
mod codex_delegate;
mod collaboration_instructions;
mod compact;
mod compact_remote;
mod compact_resume_fork;
mod deprecation_notice;
mod exec;
mod exec_policy;
mod fork_thread;
mod grep_files;
mod hierarchical_agents;
#[cfg(not(target_os = "windows"))]
mod hooks;
mod image_rollout;
mod items;
mod js_repl;
mod json_result;
mod list_dir;
mod live_cli;
mod live_reload;
mod memories;
mod model_info_overrides;
mod model_overrides;
mod model_switching;
mod model_visible_layout;
mod models_cache_ttl;
mod models_etag_responses;
mod otel;
mod pending_input;
mod permissions_messages;
mod personality;
mod personality_migration;
mod plugins;
mod prompt_caching;
mod quota_exceeded;
mod read_file;
mod realtime_conversation;
mod remote_env;
mod remote_models;
mod request_compression;
#[cfg(not(target_os = "windows"))]
mod request_permissions;
#[cfg(not(target_os = "windows"))]
mod request_permissions_tool;
mod request_user_input;
mod resume;
mod resume_warning;
mod review;
mod rmcp_client;
mod rollout_list_find;
mod safety_check_downgrade;
mod search_tool;
mod seatbelt;
mod shell_command;
mod shell_serialization;
mod shell_snapshot;
mod skill_approval;
mod skills;
mod spawn_agent_description;
mod sqlite_state;
mod stream_error_allows_next_turn;
mod stream_no_completed;
mod subagent_notifications;
mod text_encoding_fix;
mod tool_harness;
mod tool_parallelism;
mod tools;
mod truncation;
mod turn_state;
mod undo;
mod unified_exec;
mod unstable_features_warning;
mod user_notification;
mod user_shell_cmd;
mod view_image;
mod web_search;
mod websocket_fallback;
