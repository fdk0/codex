use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::Turn;

use crate::model::DashboardPreview;
use crate::model::PreviewSource;

pub(crate) fn committed_preview(thread: &Thread) -> DashboardPreview {
    for turn in thread.turns.iter().rev() {
        if let Some(preview) = turn_preview(turn) {
            return preview;
        }
    }

    DashboardPreview {
        text: normalize_preview(&thread.preview),
        source: PreviewSource::ThreadPreview,
        live: false,
    }
}

fn turn_preview(turn: &Turn) -> Option<DashboardPreview> {
    for item in turn.items.iter().rev() {
        match item {
            ThreadItem::AgentMessage { text, .. } => {
                let text = normalize_preview(text);
                if !text.is_empty() {
                    return Some(DashboardPreview {
                        text,
                        source: PreviewSource::AgentMessage,
                        live: false,
                    });
                }
            }
            ThreadItem::Plan { text, .. } => {
                let text = normalize_preview(text);
                if !text.is_empty() {
                    return Some(DashboardPreview {
                        text,
                        source: PreviewSource::Plan,
                        live: false,
                    });
                }
            }
            ThreadItem::Reasoning {
                summary, content, ..
            } => {
                let summary_text = normalize_preview(&summary.join(" "));
                if !summary_text.is_empty() {
                    return Some(DashboardPreview {
                        text: summary_text,
                        source: PreviewSource::ReasoningSummary,
                        live: false,
                    });
                }
                let content_text = normalize_preview(&content.join(" "));
                if !content_text.is_empty() {
                    return Some(DashboardPreview {
                        text: content_text,
                        source: PreviewSource::ReasoningText,
                        live: false,
                    });
                }
            }
            _ => {}
        }
    }
    None
}

pub(crate) fn normalize_preview(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::ThreadStatus;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;

    fn thread_with_items(items: Vec<ThreadItem>) -> Thread {
        Thread {
            id: "child".to_string(),
            preview: "fallback preview".to_string(),
            ephemeral: false,
            model_provider: "mock".to_string(),
            created_at: 0,
            updated_at: 0,
            status: ThreadStatus::Idle,
            path: None,
            cwd: PathBuf::from("/tmp"),
            cli_version: "0.0.0".to_string(),
            source: codex_app_server_protocol::SessionSource::Exec,
            parent_thread_id: Some("parent".to_string()),
            agent_nickname: Some("Scout".to_string()),
            agent_role: Some("explorer".to_string()),
            git_info: None,
            name: None,
            turns: vec![Turn {
                id: "turn-1".to_string(),
                items,
                status: codex_app_server_protocol::TurnStatus::Completed,
                error: None,
            }],
        }
    }

    #[test]
    fn committed_preview_prefers_agent_message() {
        let thread = thread_with_items(vec![
            ThreadItem::Plan {
                id: "plan-1".to_string(),
                text: "draft plan".to_string(),
            },
            ThreadItem::AgentMessage {
                id: "msg-1".to_string(),
                text: "final answer".to_string(),
                phase: None,
                memory_citation: None,
            },
        ]);

        assert_eq!(
            committed_preview(&thread),
            DashboardPreview {
                text: "final answer".to_string(),
                source: PreviewSource::AgentMessage,
                live: false,
            }
        );
    }

    #[test]
    fn committed_preview_falls_back_to_thread_preview() {
        let thread = thread_with_items(vec![]);

        assert_eq!(
            committed_preview(&thread),
            DashboardPreview {
                text: "fallback preview".to_string(),
                source: PreviewSource::ThreadPreview,
                live: false,
            }
        );
    }
}
