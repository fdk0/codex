use std::fs;
use std::path::Path;

use codex_config::ConfigLayerStack;
use codex_config::ConfigLayerStackOrdering;
use regex::Regex;

use super::ConfiguredHandler;
use super::config::HookConditions;
use super::config::HookHandlerConfig;
use super::config::HooksFile;

pub(crate) struct DiscoveryResult {
    pub handlers: Vec<ConfiguredHandler>,
    pub warnings: Vec<String>,
}

struct AppendGroupSpec<'a> {
    source_path: &'a Path,
    event_name: codex_protocol::protocol::HookEventName,
    matcher: Option<&'a str>,
    conditions: HookConditions,
}

pub(crate) fn discover_handlers(config_layer_stack: Option<&ConfigLayerStack>) -> DiscoveryResult {
    let Some(config_layer_stack) = config_layer_stack else {
        return DiscoveryResult {
            handlers: Vec::new(),
            warnings: Vec::new(),
        };
    };

    let mut handlers = Vec::new();
    let mut warnings = Vec::new();
    let mut display_order = 0_i64;

    for layer in config_layer_stack.get_layers(
        ConfigLayerStackOrdering::LowestPrecedenceFirst,
        /*include_disabled*/ false,
    ) {
        let Some(folder) = layer.config_folder() else {
            continue;
        };
        let source_path = match folder.join("hooks.json") {
            Ok(source_path) => source_path,
            Err(err) => {
                warnings.push(format!(
                    "failed to resolve hooks config path from {}: {err}",
                    folder.display()
                ));
                continue;
            }
        };
        if !source_path.as_path().is_file() {
            continue;
        }

        let contents = match fs::read_to_string(source_path.as_path()) {
            Ok(contents) => contents,
            Err(err) => {
                warnings.push(format!(
                    "failed to read hooks config {}: {err}",
                    source_path.display()
                ));
                continue;
            }
        };

        let parsed: HooksFile = match serde_json::from_str(&contents) {
            Ok(parsed) => parsed,
            Err(err) => {
                warnings.push(format!(
                    "failed to parse hooks config {}: {err}",
                    source_path.display()
                ));
                continue;
            }
        };

        for group in parsed.hooks.session_start {
            append_group_handlers(
                &mut handlers,
                &mut warnings,
                &mut display_order,
                AppendGroupSpec {
                    source_path: source_path.as_path(),
                    event_name: codex_protocol::protocol::HookEventName::SessionStart,
                    matcher: group.matcher.as_deref(),
                    conditions: group.conditions,
                },
                group.hooks,
            );
        }

        for group in parsed.hooks.user_prompt_submit {
            append_group_handlers(
                &mut handlers,
                &mut warnings,
                &mut display_order,
                AppendGroupSpec {
                    source_path: source_path.as_path(),
                    event_name: codex_protocol::protocol::HookEventName::UserPromptSubmit,
                    matcher: group.matcher.as_deref(),
                    conditions: group.conditions,
                },
                group.hooks,
            );
        }

        for group in parsed.hooks.stop {
            append_group_handlers(
                &mut handlers,
                &mut warnings,
                &mut display_order,
                AppendGroupSpec {
                    source_path: source_path.as_path(),
                    event_name: codex_protocol::protocol::HookEventName::Stop,
                    matcher: group.matcher.as_deref(),
                    conditions: group.conditions,
                },
                group.hooks,
            );
        }
    }

    DiscoveryResult { handlers, warnings }
}

fn append_group_handlers(
    handlers: &mut Vec<ConfiguredHandler>,
    warnings: &mut Vec<String>,
    display_order: &mut i64,
    spec: AppendGroupSpec<'_>,
    group_handlers: Vec<HookHandlerConfig>,
) {
    if let Some(matcher) = spec.matcher
        && let Err(err) = Regex::new(matcher)
    {
        warnings.push(format!(
            "invalid matcher {matcher:?} in {}: {err}",
            spec.source_path.display()
        ));
        return;
    }

    for handler in group_handlers {
        match handler {
            HookHandlerConfig::Command {
                command,
                timeout_sec,
                r#async,
                status_message,
            } => {
                if r#async {
                    warnings.push(format!(
                        "skipping async hook in {}: async hooks are not supported yet",
                        spec.source_path.display()
                    ));
                    continue;
                }
                if command.trim().is_empty() {
                    warnings.push(format!(
                        "skipping empty hook command in {}",
                        spec.source_path.display()
                    ));
                    continue;
                }
                let timeout_sec = timeout_sec.unwrap_or(600).max(1);
                handlers.push(ConfiguredHandler {
                    event_name: spec.event_name,
                    matcher: spec.matcher.map(ToOwned::to_owned),
                    conditions: spec.conditions.clone(),
                    command,
                    timeout_sec,
                    status_message,
                    source_path: spec.source_path.to_path_buf(),
                    display_order: *display_order,
                });
                *display_order += 1;
            }
            HookHandlerConfig::Prompt {} => warnings.push(format!(
                "skipping prompt hook in {}: prompt hooks are not supported yet",
                spec.source_path.display()
            )),
            HookHandlerConfig::Agent {} => warnings.push(format!(
                "skipping agent hook in {}: agent hooks are not supported yet",
                spec.source_path.display()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::path::PathBuf;

    use codex_protocol::protocol::HookEventName;
    use pretty_assertions::assert_eq;

    use crate::engine::config::HookConditions;

    use super::AppendGroupSpec;
    use super::ConfiguredHandler;
    use super::HookHandlerConfig;
    use super::append_group_handlers;

    #[test]
    fn user_prompt_submit_invalid_matcher_warns_and_skips_handler() {
        let mut handlers = Vec::new();
        let mut warnings = Vec::new();
        let mut display_order = 0;

        append_group_handlers(
            &mut handlers,
            &mut warnings,
            &mut display_order,
            AppendGroupSpec {
                source_path: Path::new("/tmp/hooks.json"),
                event_name: HookEventName::UserPromptSubmit,
                matcher: Some("["),
                conditions: HookConditions::default(),
            },
            vec![HookHandlerConfig::Command {
                command: "echo hello".to_string(),
                timeout_sec: None,
                r#async: false,
                status_message: None,
            }],
        );

        assert!(handlers.is_empty());
        assert_eq!(display_order, 0);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("invalid matcher"));
    }

    #[test]
    fn append_group_handlers_preserves_conditions() {
        let mut handlers = Vec::new();
        let mut warnings = Vec::new();
        let mut display_order = 0;
        let conditions = HookConditions {
            profile: Some("bd-worker".to_string()),
            profiles: vec!["review".to_string()],
            model: Some("gpt-5".to_string()),
            models: Vec::new(),
            permission_mode: Some("default".to_string()),
            permission_modes: vec!["bypassPermissions".to_string()],
        };

        append_group_handlers(
            &mut handlers,
            &mut warnings,
            &mut display_order,
            AppendGroupSpec {
                source_path: Path::new("/tmp/hooks.json"),
                event_name: HookEventName::Stop,
                matcher: Some("^done$"),
                conditions: conditions.clone(),
            },
            vec![HookHandlerConfig::Command {
                command: "echo hello".to_string(),
                timeout_sec: None,
                r#async: false,
                status_message: None,
            }],
        );

        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(
            handlers,
            vec![ConfiguredHandler {
                event_name: HookEventName::Stop,
                matcher: Some("^done$".to_string()),
                conditions,
                command: "echo hello".to_string(),
                timeout_sec: 600,
                status_message: None,
                source_path: PathBuf::from("/tmp/hooks.json"),
                display_order: 0,
            }]
        );
    }
}
