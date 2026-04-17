use std::sync::LazyLock;

/// The current Codex CLI version as embedded at compile time.
pub const CODEX_CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Stable identifier shown for this custom fork build.
///
/// Builders can override the default label with `CODEX_BUILD_LABEL=<label>`
/// at compile time when they want a more specific distribution tag.
pub const CODEX_BUILD_LABEL: &str = match option_env!("CODEX_BUILD_LABEL") {
    Some(label) => label,
    None => "custom",
};

static CODEX_CLI_DISPLAY_VERSION: LazyLock<String> =
    LazyLock::new(|| format!("{CODEX_CLI_VERSION} [{CODEX_BUILD_LABEL}]"));

pub fn codex_cli_display_version() -> &'static str {
    CODEX_CLI_DISPLAY_VERSION.as_str()
}
