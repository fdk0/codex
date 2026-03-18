use super::*;

use serde_json::Value;
use tempfile::TempDir;
use tokio::process::Command;

fn test_git_env() -> [(&'static str, &'static str); 2] {
    [
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
    ]
}

#[tokio::test]
async fn build_turn_metadata_header_includes_has_changes_for_clean_repo() {
    let temp_dir = TempDir::new().expect("temp dir");
    let repo_path = temp_dir.path().join("repo");
    std::fs::create_dir_all(&repo_path).expect("create repo");

    let envs = test_git_env();

    let init = Command::new("git")
        .envs(envs)
        .args(["init"])
        .current_dir(&repo_path)
        .output()
        .await
        .expect("git init");
    assert!(init.status.success(), "git init should succeed");

    let config_user_name = Command::new("git")
        .envs(envs)
        .args(["config", "user.name", "Test User"])
        .current_dir(&repo_path)
        .output()
        .await
        .expect("git config user.name");
    assert!(
        config_user_name.status.success(),
        "git config user.name should succeed"
    );

    let config_user_email = Command::new("git")
        .envs(envs)
        .args(["config", "user.email", "test@example.com"])
        .current_dir(&repo_path)
        .output()
        .await
        .expect("git config user.email");
    assert!(
        config_user_email.status.success(),
        "git config user.email should succeed"
    );

    std::fs::write(repo_path.join("README.md"), "hello").expect("write file");
    let add = Command::new("git")
        .envs(envs)
        .args(["add", "."])
        .current_dir(&repo_path)
        .output()
        .await
        .expect("git add");
    assert!(add.status.success(), "git add should succeed");

    let commit = Command::new("git")
        .envs(envs)
        .args(["commit", "-m", "initial"])
        .current_dir(&repo_path)
        .output()
        .await
        .expect("git commit");
    assert!(commit.status.success(), "git commit should succeed");

    let header = build_turn_metadata_header(&repo_path, Some("none"))
        .await
        .expect("header");
    let parsed: Value = serde_json::from_str(&header).expect("valid json");
    let workspace = parsed
        .get("workspaces")
        .and_then(Value::as_object)
        .and_then(|workspaces| workspaces.values().next())
        .cloned()
        .expect("workspace");

    assert_eq!(
        workspace.get("has_changes").and_then(Value::as_bool),
        Some(false)
    );
}

#[test]
fn turn_metadata_state_uses_platform_sandbox_tag() {
    let temp_dir = TempDir::new().expect("temp dir");
    let cwd = temp_dir.path().to_path_buf();
    let sandbox_policy = SandboxPolicy::new_read_only_policy();

    let state = TurnMetadataState::new(
        "session-a".to_string(),
        "turn-a".to_string(),
        cwd,
        &sandbox_policy,
        WindowsSandboxLevel::Disabled,
    );

    let header = state.current_header_value().expect("header");
    let json: Value = serde_json::from_str(&header).expect("json");
    let sandbox_name = json.get("sandbox").and_then(Value::as_str);
    let session_id = json.get("session_id").and_then(Value::as_str);

    let expected_sandbox = sandbox_tag(&sandbox_policy, WindowsSandboxLevel::Disabled);
    assert_eq!(sandbox_name, Some(expected_sandbox));
    assert_eq!(session_id, Some("session-a"));
}
