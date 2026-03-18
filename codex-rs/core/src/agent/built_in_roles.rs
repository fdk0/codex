use crate::config::AgentRoleConfig;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;

/// Returns the cached built-in role declarations defined in this module.
pub(crate) fn configs() -> &'static BTreeMap<String, AgentRoleConfig> {
    static CONFIG: LazyLock<BTreeMap<String, AgentRoleConfig>> = LazyLock::new(|| {
        BTreeMap::from([
            (
                "default".to_string(),
                AgentRoleConfig {
                    description: Some("Default agent.".to_string()),
                    config_file: None,
                    model: None,
                    spawn_mode: None,
                    nickname_candidates: None,
                },
            ),
            (
                "explorer".to_string(),
                AgentRoleConfig {
                    description: Some(r#"Use `explorer` for specific codebase questions.
Explorers are fast and authoritative.
They must be used to ask specific, well-scoped questions on the codebase.
Rules:
- In order to avoid redundant work, you should avoid exploring the same problem that explorers have already covered. Typically, you should trust the explorer results without additional verification. You are still allowed to inspect the code yourself to gain the needed context!
- You are encouraged to spawn up multiple explorers in parallel when you have multiple distinct questions to ask about the codebase that can be answered independently. This allows you to get more information faster without waiting for one question to finish before asking the next. While waiting for the explorer results, you can continue working on other local tasks that do not depend on those results. This parallelism is a key advantage of delegation, so use it whenever you have multiple questions to ask.
- Reuse existing explorers for related questions."#
                        .to_string()),
                    model: None,
                    config_file: Some("explorer.toml".to_string().parse().unwrap_or_default()),
                    spawn_mode: None,
                    nickname_candidates: None,
                },
            ),
            (
                "fast-worker".to_string(),
                AgentRoleConfig {
                    description: Some(r#"Use `fast-worker` for tightly constrained problems.
Typical tasks:
- Make a small, localized code change
- Execute a narrowly scoped command sequence
- Handle an isolated fix from a self-contained prompt
Rules:
- Keep scope tight and avoid broad repo exploration.
- Treat the prompt as self-contained and do not assume shared context.
- Prefer direct execution over extended analysis."#
                        .to_string()),
                    config_file: None,
                    model: None,
                    spawn_mode: None,
                    nickname_candidates: None,
                },
            ),
            (
                "worker".to_string(),
                AgentRoleConfig {
                    description: Some(r#"Use for execution and production work.
Typical tasks:
- Implement part of a feature
- Fix tests or bugs
- Split large refactors into independent chunks
Rules:
- Explicitly assign **ownership** of the task (files / responsibility). When the subtask involves code changes, you should clearly specify which files or modules the worker is responsible for. This helps avoid merge conflicts and ensures accountability. For example, you can say "Worker 1 is responsible for updating the authentication module, while Worker 2 will handle the database layer." By defining clear ownership, you can delegate more effectively and reduce coordination overhead.
- Always tell workers they are **not alone in the codebase**, and they should not revert the edits made by others, and they should adjust their implementation to accommodate the changes made by others. This is important because there may be multiple workers making changes in parallel, and they need to be aware of each other's work to avoid conflicts and ensure a cohesive final product."#
                        .to_string()),
                    config_file: None,
                    model: None,
                    spawn_mode: None,
                    nickname_candidates: None,
                },
            ),
            (
                "awaiter".to_string(),
                AgentRoleConfig {
                    description: Some(r#"Use an `awaiter` agent EVERY TIME you must run a command that might take some time.
This includes, but is not limited to:
* testing
* monitoring of a long-running process
* explicit ask to wait for something

Rules:
- When you wait for the `awaiter` to be done, use the largest possible timeout.
- Only use `awaiter` for commands that may take time."#
                        .to_string()),
                    config_file: Some("awaiter.toml".to_string().parse().unwrap_or_default()),
                    model: None,
                    spawn_mode: None,
                    nickname_candidates: None,
                },
            ),
        ])
    });
    &CONFIG
}

/// Resolves a built-in role `config_file` path to embedded content.
pub(crate) fn config_file_contents(path: &Path) -> Option<&'static str> {
    const EXPLORER: &str = include_str!("builtins/explorer.toml");
    const AWAITER: &str = include_str!("builtins/awaiter.toml");
    match path.to_str()? {
        "explorer.toml" => Some(EXPLORER),
        "awaiter.toml" => Some(AWAITER),
        _ => None,
    }
}
