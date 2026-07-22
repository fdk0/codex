//! Informational, warning, update, and policy notice history cells.

use super::*;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::InterAgentCommunication;

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
                "✨\u{200A}".bold().cyan(),
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
        let lines = adaptive_wrap_lines(content.lines, RtOptions::new(inner_width));
        with_border_with_inner_width(lines, inner_width)
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

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        crate::terminal_hyperlinks::annotate_web_urls(self.display_lines(width))
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.display_hyperlink_lines(width)
    }
}
#[allow(clippy::disallowed_methods)]
pub(crate) fn new_warning_event(message: String) -> PrefixedWrappedHistoryCell {
    PrefixedWrappedHistoryCell::new(message.yellow(), "⚠ ".yellow(), "  ")
}

#[derive(Debug)]
pub(crate) struct SafetyAccessBlockCell {
    body: &'static str,
    trusted_access_url: &'static str,
}

const SAFETY_ACCESS_BLOCK_TITLE: &str = "This content can't be shown";
const SAFETY_ACCESS_BLOCK_LEARN_MORE_URL: &str = "https://help.openai.com/en/articles/20001326";

pub(crate) fn new_safety_access_block_event() -> SafetyAccessBlockCell {
    SafetyAccessBlockCell {
        body: "We take extra caution with requests involving biological research and applications that could pose safety risks. Eligible researchers can apply for Trusted Access.",
        trusted_access_url: "https://www.openai.com/form/trusted-access-for-biology-research/",
    }
}

pub(crate) fn new_cyber_policy_error_event() -> SafetyAccessBlockCell {
    SafetyAccessBlockCell {
        body: "We take extra caution with cybersecurity requests. If you’re a security professional, you may be able to apply for Trusted Access.",
        trusted_access_url: "https://openai.com/form/enterprise-trusted-access-for-cyber/",
    }
}

impl HistoryCell for SafetyAccessBlockCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.display_hyperlink_lines(width))
    }

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = vec![HyperlinkLine::new(
            vec!["ⓘ ".cyan(), SAFETY_ACCESS_BLOCK_TITLE.bold()].into(),
        )];
        let body = Line::from(vec!["  ".into(), self.body.dim()]);
        let wrap_width = width.saturating_sub(2).max(1) as usize;
        let wrapped = adaptive_wrap_line(
            &body,
            RtOptions::new(wrap_width).subsequent_indent("  ".into()),
        );
        let mut wrapped_body = Vec::new();
        push_owned_lines(&wrapped, &mut wrapped_body);
        lines.extend(plain_hyperlink_lines(wrapped_body));

        for (label, url) in [
            ("Trusted Access", self.trusted_access_url),
            ("Learn more", SAFETY_ACCESS_BLOCK_LEARN_MORE_URL),
        ] {
            let source = crate::terminal_hyperlinks::annotate_web_urls_in_line(
                vec![format!("  {label}: ").dim(), url.cyan().underlined()].into(),
            );
            let wrapped = crate::wrapping::word_wrap_line(
                &source.line,
                RtOptions::new(wrap_width).subsequent_indent("  ".into()),
            );
            let mut wrapped_links = Vec::new();
            push_owned_lines(&wrapped, &mut wrapped_links);
            lines.extend(crate::terminal_hyperlinks::remap_wrapped_line(
                &source,
                wrapped_links,
            ));
        }
        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let trusted_access_url = self.trusted_access_url;
        vec![
            Line::from(SAFETY_ACCESS_BLOCK_TITLE),
            Line::from(self.body),
            Line::from(format!("Trusted Access: {trusted_access_url}")),
            Line::from(format!("Learn more: {SAFETY_ACCESS_BLOCK_LEARN_MORE_URL}")),
        ]
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.display_hyperlink_lines(width)
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

fn find_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

fn extract_subagent_notification(message: &str) -> Option<String> {
    let trimmed = message.trim();

    if let Ok(communication) = serde_json::from_str::<InterAgentCommunication>(trimmed) {
        return extract_subagent_notification(&communication.content);
    }

    let start = find_case_insensitive(trimmed, SUBAGENT_NOTIFICATION_OPEN_TAG)?;
    let body_start = start + SUBAGENT_NOTIFICATION_OPEN_TAG.len();
    let close_offset =
        find_case_insensitive(&trimmed[body_start..], SUBAGENT_NOTIFICATION_CLOSE_TAG)?;
    let body_end = body_start + close_offset;
    Some(trimmed[body_start..body_end].trim().to_string())
}

fn parse_subagent_notification_message(message: &str) -> Option<SubagentNotificationPayload> {
    let body = extract_subagent_notification(message)?;
    serde_json::from_str(&body).ok()
}

#[derive(Debug)]
pub(crate) struct SubagentNotificationHistoryCell {
    title: &'static str,
    agent_reference: String,
    detail: Option<String>,
}

impl SubagentNotificationHistoryCell {
    fn header_line(&self) -> Line<'static> {
        vec![
            "• ".dim(),
            Span::from(self.title).bold(),
            Span::from(": ").dim(),
            self.agent_reference.clone().into(),
        ]
        .into()
    }

    fn detail_display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let Some(detail) = self.detail.as_deref() else {
            return Vec::new();
        };

        let wrap_width = width.saturating_sub(4).max(1) as usize;
        let rendered = crate::markdown::render_markdown_agent_with_links_and_cwd(
            detail.trim(),
            Some(wrap_width),
            /*cwd*/ None,
        );
        let mut body = visible_lines(rendered);
        if body.is_empty() {
            body.extend(raw_lines_from_source(detail.trim_end()));
        }
        prefix_lines(body, "  └ ".dim(), "    ".into())
    }

    fn detail_raw_lines(&self) -> Vec<Line<'static>> {
        let Some(detail) = self.detail.as_deref() else {
            return Vec::new();
        };

        let body = raw_lines_from_source(detail.trim_end());
        prefix_lines(body, "  └ ".dim(), "    ".into())
    }
}

impl HistoryCell for SubagentNotificationHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = vec![self.header_line()];
        lines.extend(self.detail_display_lines(width));
        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let mut lines = plain_lines(vec![self.header_line()]);
        lines.extend(self.detail_raw_lines());
        lines
    }
}

pub(crate) fn new_subagent_notification_event(
    message: &str,
) -> Option<SubagentNotificationHistoryCell> {
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

    Some(SubagentNotificationHistoryCell {
        title,
        agent_reference: notification.agent_reference,
        detail,
    })
}

pub(crate) fn new_error_event(message: String) -> PlainHistoryCell {
    // Use a hair space (U+200A) to create a subtle, near-invisible separation
    // before the text. VS16 is intentionally omitted to keep spacing tighter
    // in terminals like Ghostty.
    let lines: Vec<Line<'static>> = vec![vec![format!("■ {message}").red()].into()];
    PlainHistoryCell { lines }
}
