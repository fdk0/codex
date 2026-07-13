use std::env;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_app_server::AppServerTransport;
use codex_app_server_daemon::DESKTOP_SHARED_APP_SERVER_CLIENT_NAME_ENV_VAR;
use codex_app_server_daemon::DESKTOP_SHARED_APP_SERVER_ENV_VAR;
use codex_app_server_daemon::DesktopAppServerOptions;
use codex_app_server_daemon::DesktopRemoteControlMode;
use codex_login::default_client::CODEX_INTERNAL_ORIGINATOR_OVERRIDE_ENV_VAR;
use codex_utils_cli::CliConfigOverrides;

pub(crate) struct DesktopSharedAppServerOptions<'a> {
    pub(crate) transport: AppServerTransport,
    pub(crate) config_overrides: &'a CliConfigOverrides,
    pub(crate) strict_config: bool,
    pub(crate) analytics_default_enabled: bool,
    pub(crate) remote_control: bool,
    pub(crate) remote_control_disabled: bool,
    pub(crate) remote_control_client_name: Option<String>,
}

pub(crate) async fn run_if_enabled(options: DesktopSharedAppServerOptions<'_>) -> Result<bool> {
    if !shared_mode_enabled()? {
        return Ok(false);
    }
    if options.transport != AppServerTransport::Stdio {
        bail!("desktop shared app-server mode requires the stdio transport");
    }
    if options.strict_config {
        bail!("desktop shared app-server mode does not support `--strict-config`");
    }
    validate_config_overrides(options.config_overrides)?;

    let codex_bin = env::current_exe().context("failed to resolve desktop Codex executable")?;
    let remote_control_client_name = options
        .remote_control_client_name
        .or_else(shared_mode_client_name);
    let output = codex_app_server_daemon::ensure_desktop_app_server(DesktopAppServerOptions {
        codex_bin,
        analytics_default_enabled: options.analytics_default_enabled,
        remote_control_mode: desktop_remote_control_mode(
            options.remote_control,
            options.remote_control_disabled,
        ),
        remote_control_client_name,
    })
    .await?;
    // The local desktop launches Codex with the JSONL stdio transport, while
    // the shared daemon exposes a WebSocket control socket. Adapt only here;
    // the SSH-facing `app-server proxy` must remain a raw byte tunnel.
    codex_app_server_daemon::proxy_app_server_json_lines(
        output.socket_path.as_path(),
        tokio::io::stdin(),
        tokio::io::stdout(),
    )
    .await?;
    Ok(true)
}

fn shared_mode_enabled() -> Result<bool> {
    let Ok(value) = env::var(DESKTOP_SHARED_APP_SERVER_ENV_VAR) else {
        return Ok(false);
    };
    parse_enabled_value(&value)
}

fn parse_enabled_value(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "" | "0" | "false" | "no" | "off" => Ok(false),
        _ => bail!(
            "invalid {DESKTOP_SHARED_APP_SERVER_ENV_VAR} value `{value}`; expected true or false"
        ),
    }
}

fn shared_mode_client_name() -> Option<String> {
    select_shared_mode_client_name(
        env::var(CODEX_INTERNAL_ORIGINATOR_OVERRIDE_ENV_VAR).ok(),
        env::var(DESKTOP_SHARED_APP_SERVER_CLIENT_NAME_ENV_VAR).ok(),
    )
}

fn select_shared_mode_client_name(
    originator_override: Option<String>,
    configured_client_name: Option<String>,
) -> Option<String> {
    // Desktop injects a process-global originator before spawning Codex. The
    // app-server rejects a different command-line client name, so preserve that
    // identity while allowing the bundle's display name to remain customized.
    originator_override
        .or(configured_client_name)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn validate_config_overrides(config_overrides: &CliConfigOverrides) -> Result<()> {
    let unsupported = config_overrides
        .raw_overrides
        .iter()
        .filter(|value| !is_desktop_compatible_override(value))
        .cloned()
        .collect::<Vec<_>>();
    if unsupported.is_empty() {
        return Ok(());
    }
    bail!(
        "desktop shared app-server mode cannot preserve these command-line config overrides: {}; move them to config.toml",
        unsupported.join(", ")
    )
}

fn is_desktop_compatible_override(value: &str) -> bool {
    let Some((key, value)) = value.split_once('=') else {
        return false;
    };
    key.trim() == "features.code_mode_host" && value.trim() == "true"
}

fn desktop_remote_control_mode(
    remote_control: bool,
    remote_control_disabled: bool,
) -> DesktopRemoteControlMode {
    if remote_control {
        DesktopRemoteControlMode::Enabled
    } else if remote_control_disabled {
        DesktopRemoteControlMode::Disabled
    } else {
        DesktopRemoteControlMode::ResolvePersisted
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::DesktopRemoteControlMode;
    use super::desktop_remote_control_mode;
    use super::is_desktop_compatible_override;
    use super::parse_enabled_value;
    use super::select_shared_mode_client_name;

    #[test]
    fn shared_mode_environment_values_are_explicit() {
        assert!(parse_enabled_value("true").expect("true"));
        assert!(!parse_enabled_value("0").expect("false"));
        assert!(parse_enabled_value("sometimes").is_err());
    }

    #[test]
    fn desktop_override_allows_only_the_known_app_default() {
        assert!(is_desktop_compatible_override(
            "features.code_mode_host=true"
        ));
        assert!(!is_desktop_compatible_override("model=gpt-5.5"));
        assert!(!is_desktop_compatible_override(
            "features.code_mode_host=false"
        ));
    }

    #[test]
    fn explicit_remote_control_flags_override_persisted_resolution() {
        assert_eq!(
            desktop_remote_control_mode(true, true),
            DesktopRemoteControlMode::Enabled
        );
        assert_eq!(
            desktop_remote_control_mode(false, true),
            DesktopRemoteControlMode::Disabled
        );
        assert_eq!(
            desktop_remote_control_mode(false, false),
            DesktopRemoteControlMode::ResolvePersisted
        );
    }

    #[test]
    fn desktop_originator_overrides_the_configured_shared_client_name() {
        assert_eq!(
            select_shared_mode_client_name(
                Some(" Codex Desktop ".to_string()),
                Some("CustomChatGPT".to_string()),
            ),
            Some("Codex Desktop".to_string())
        );
        assert_eq!(
            select_shared_mode_client_name(None, Some(" CustomChatGPT ".to_string())),
            Some("CustomChatGPT".to_string())
        );
    }
}
