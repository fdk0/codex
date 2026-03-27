use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PreviewSource {
    AgentMessage,
    Plan,
    ReasoningSummary,
    ReasoningText,
    ThreadPreview,
}

impl PreviewSource {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::AgentMessage => "message",
            Self::Plan => "plan",
            Self::ReasoningSummary => "summary",
            Self::ReasoningText => "reasoning",
            Self::ThreadPreview => "preview",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DashboardPreview {
    pub(crate) text: String,
    pub(crate) source: PreviewSource,
    pub(crate) live: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DashboardAgentEntry {
    pub(crate) thread_id: String,
    pub(crate) nickname: Option<String>,
    pub(crate) role: Option<String>,
    pub(crate) status: ThreadStatus,
    pub(crate) updated_at: i64,
    pub(crate) preview: DashboardPreview,
}

impl DashboardAgentEntry {
    pub(crate) fn from_thread(thread: &Thread, preview: DashboardPreview) -> Self {
        Self {
            thread_id: thread.id.clone(),
            nickname: thread.agent_nickname.clone(),
            role: thread.agent_role.clone(),
            status: thread.status.clone(),
            updated_at: thread.updated_at,
            preview,
        }
    }

    pub(crate) fn status_rank(&self) -> u8 {
        match self.status {
            ThreadStatus::Active { .. } => 0,
            ThreadStatus::Idle => 1,
            ThreadStatus::SystemError => 2,
            ThreadStatus::NotLoaded => 3,
        }
    }

    pub(crate) fn title(&self) -> String {
        let base = self
            .nickname
            .clone()
            .unwrap_or_else(|| short_thread_id(&self.thread_id));
        match &self.role {
            Some(role) => format!("{base} [{role}]"),
            None => base,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DashboardParentEntry {
    pub(crate) thread_id: String,
    pub(crate) label: String,
    pub(crate) status: ThreadStatus,
    pub(crate) updated_at: i64,
    pub(crate) preview: DashboardPreview,
}

impl DashboardParentEntry {
    pub(crate) fn from_thread(thread: &Thread, preview: DashboardPreview) -> Self {
        Self {
            thread_id: thread.id.clone(),
            label: thread_label(thread),
            status: thread.status.clone(),
            updated_at: thread.updated_at,
            preview,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DashboardSnapshot {
    pub(crate) parent_thread_id: String,
    pub(crate) parent_label: String,
    pub(crate) parent: Option<DashboardParentEntry>,
    pub(crate) agents: Vec<DashboardAgentEntry>,
    pub(crate) refresh_error: Option<String>,
}

pub(crate) fn thread_label(thread: &Thread) -> String {
    if let Some(name) = &thread.name
        && !name.trim().is_empty()
    {
        return name.clone();
    }
    if let Some(project_name) = thread_project_name(thread) {
        return project_name;
    }
    if !thread.preview.trim().is_empty() {
        return thread.preview.clone();
    }
    format!("thread {}", short_thread_id(&thread.id))
}

pub(crate) fn thread_project_name(thread: &Thread) -> Option<String> {
    thread
        .cwd
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn short_thread_id(thread_id: &str) -> String {
    thread_id.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::SessionSource;
    use std::path::PathBuf;

    #[test]
    fn thread_label_uses_cwd_basename_when_name_is_missing() {
        let thread = Thread {
            id: "thread-id".to_string(),
            preview: "preview text".to_string(),
            ephemeral: false,
            model_provider: "mock".to_string(),
            created_at: 0,
            updated_at: 0,
            status: ThreadStatus::Idle,
            path: None,
            cwd: PathBuf::from("/tmp/polytick"),
            cli_version: "0.0.0".to_string(),
            source: SessionSource::Exec,
            parent_thread_id: None,
            agent_nickname: None,
            agent_role: None,
            git_info: None,
            name: None,
            turns: Vec::new(),
        };

        assert_eq!(thread_label(&thread), "polytick");
    }
}
