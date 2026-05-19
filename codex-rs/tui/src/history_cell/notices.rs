//! Informational, warning, update, and policy notice history cells.

use super::*;
use codex_protocol::protocol::AgentStatus;

#[cfg_attr(debug_assertions, allow(dead_code))]
#[derive(Debug)]
pub(crate) struct UpdateAvailableHistoryCell {
    latest_version: String,
    update_action: Option<UpdateAction>,
}

#[cfg_attr(debug_assertions, allow(dead_code))]
impl UpdateAvailableHistoryCell {
    pub(crate) fn new(latest_version: String, update_action: Option<UpdateAction>) -> Self {
        Self {
            latest_version,
            update_action,
        }
    }
}

impl HistoryCell for UpdateAvailableHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        use ratatui_macros::line;
        use ratatui_macros::text;
        let update_instruction = if let Some(update_action) = self.update_action {
            line!["Run ", update_action.command_str().cyan(), " to update."]
        } else {
            line![
                "See ",
                "https://github.com/openai/codex".cyan().underlined(),
                " for installation options."
            ]
        };

        let content = text![
            line![
                padded_emoji("✨").bold().cyan(),
                "Update available!".bold().cyan(),
                " ",
                format!("{CODEX_CLI_VERSION} -> {}", self.latest_version).bold(),
            ],
            update_instruction,
            "",
            "See full release notes:",
            "https://github.com/openai/codex/releases/latest"
                .cyan()
                .underlined(),
        ];

        let inner_width = content
            .width()
            .min(usize::from(width.saturating_sub(4)))
            .max(1);
        with_border_with_inner_width(content.lines, inner_width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let update_instruction = if let Some(update_action) = self.update_action {
            format!("Run {} to update.", update_action.command_str())
        } else {
            "See https://github.com/openai/codex for installation options.".to_string()
        };
        vec![
            Line::from("Update available!"),
            Line::from(format!("{CODEX_CLI_VERSION} -> {}", self.latest_version)),
            Line::from(update_instruction),
            Line::from(""),
            Line::from("See full release notes:"),
            Line::from("https://github.com/openai/codex/releases/latest"),
        ]
    }
}
#[allow(clippy::disallowed_methods)]
pub(crate) fn new_warning_event(message: String) -> PrefixedWrappedHistoryCell {
    PrefixedWrappedHistoryCell::new(message.yellow(), "⚠ ".yellow(), "  ")
}

const TRUSTED_ACCESS_FOR_CYBER_URL: &str = "https://chatgpt.com/cyber";

#[derive(Debug)]
pub(crate) struct CyberPolicyNoticeCell;

pub(crate) fn new_cyber_policy_error_event() -> CyberPolicyNoticeCell {
    CyberPolicyNoticeCell
}

impl HistoryCell for CyberPolicyNoticeCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(
            vec![
                "ⓘ ".cyan(),
                "This chat was flagged for possible cybersecurity risk".bold(),
            ]
            .into(),
        );

        let wrap_width = width.saturating_sub(2).max(1) as usize;
        let body = Line::from(vec![
            "  If this seems wrong, try rephrasing your request. To get authorized for security work, join the "
                .dim(),
            "Trusted Access for Cyber".cyan().underlined(),
            " program.".dim(),
        ]);
        let wrapped = adaptive_wrap_line(
            &body,
            RtOptions::new(wrap_width).subsequent_indent("  ".into()),
        );
        push_owned_lines(&wrapped, &mut lines);
        lines.push(
            vec![
                "  ".into(),
                TRUSTED_ACCESS_FOR_CYBER_URL.cyan().underlined(),
            ]
            .into(),
        );

        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        vec![
            Line::from("This chat was flagged for possible cybersecurity risk"),
            Line::from(
                "If this seems wrong, try rephrasing your request. To get authorized for security work, join the Trusted Access for Cyber program.",
            ),
            Line::from(TRUSTED_ACCESS_FOR_CYBER_URL),
        ]
    }
}

#[derive(Debug)]
pub(crate) struct DeprecationNoticeCell {
    summary: String,
    details: Option<String>,
}

pub(crate) fn new_deprecation_notice(
    summary: String,
    details: Option<String>,
) -> DeprecationNoticeCell {
    DeprecationNoticeCell { summary, details }
}

impl HistoryCell for DeprecationNoticeCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(vec!["⚠ ".red().bold(), self.summary.clone().red()].into());

        let wrap_width = width.saturating_sub(4).max(1) as usize;

        if let Some(details) = &self.details {
            let detail_line = Line::from(details.clone().dim());
            let wrapped = adaptive_wrap_line(&detail_line, RtOptions::new(wrap_width));
            push_owned_lines(&wrapped, &mut lines);
        }

        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from(self.summary.clone())];
        if let Some(details) = &self.details {
            lines.extend(raw_lines_from_source(details));
        }
        lines
    }
}
pub(crate) fn new_info_event(message: String, hint: Option<String>) -> PlainHistoryCell {
    let mut line = vec!["• ".dim(), message.into()];
    if let Some(hint) = hint {
        line.push(" ".into());
        line.push(hint.dark_gray());
    }
    let lines: Vec<Line<'static>> = vec![line.into()];
    PlainHistoryCell { lines }
}

const SUBAGENT_NOTIFICATION_OPEN_TAG: &str = "<subagent_notification>";
const SUBAGENT_NOTIFICATION_CLOSE_TAG: &str = "</subagent_notification>";

#[derive(serde::Deserialize)]
struct SubagentNotificationPayload {
    #[serde(alias = "agent_id", alias = "agent_path")]
    agent_reference: String,
    status: AgentStatus,
}

fn parse_subagent_notification_message(message: &str) -> Option<SubagentNotificationPayload> {
    let trimmed = message.trim();
    let body = trimmed
        .strip_prefix(SUBAGENT_NOTIFICATION_OPEN_TAG)?
        .strip_suffix(SUBAGENT_NOTIFICATION_CLOSE_TAG)?
        .trim();
    serde_json::from_str(body).ok()
}

pub(crate) fn new_subagent_notification_event(message: &str) -> Option<PlainHistoryCell> {
    let notification = parse_subagent_notification_message(message)?;
    let title = match &notification.status {
        AgentStatus::PendingInit => "Subagent pending init",
        AgentStatus::Running => "Subagent running",
        AgentStatus::Interrupted => "Subagent interrupted",
        AgentStatus::Completed(_) => "Subagent completed",
        AgentStatus::Errored(_) => "Subagent errored",
        AgentStatus::Shutdown => "Subagent shutdown",
        AgentStatus::NotFound => "Subagent not found",
    };
    let detail = match notification.status {
        AgentStatus::PendingInit
        | AgentStatus::Running
        | AgentStatus::Interrupted
        | AgentStatus::Shutdown
        | AgentStatus::NotFound => None,
        AgentStatus::Completed(message) => message.filter(|text| !text.trim().is_empty()),
        AgentStatus::Errored(message) => Some(message),
    };

    let mut lines: Vec<Line<'static>> = vec![
        vec![
            "• ".dim(),
            Span::from(title).bold(),
            Span::from(": ").dim(),
            notification.agent_reference.into(),
        ]
        .into(),
    ];
    if let Some(detail) = detail {
        let detail_lines = detail
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| Line::from(line.to_string()))
            .collect::<Vec<_>>();
        if !detail_lines.is_empty() {
            lines.extend(prefix_lines(detail_lines, "  └ ".dim(), "    ".into()));
        }
    }
    Some(PlainHistoryCell::new(lines))
}

pub(crate) fn new_error_event(message: String) -> PlainHistoryCell {
    // Use a hair space (U+200A) to create a subtle, near-invisible separation
    // before the text. VS16 is intentionally omitted to keep spacing tighter
    // in terminals like Ghostty.
    let lines: Vec<Line<'static>> = vec![vec![format!("■ {message}").red()].into()];
    PlainHistoryCell { lines }
}
