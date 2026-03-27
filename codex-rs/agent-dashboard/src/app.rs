use std::collections::HashMap;
use std::io::stdout;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadStatus;
use crossterm::event::Event;
use crossterm::event::EventStream;
use crossterm::event::KeyCode;
use crossterm::event::KeyEventKind;
use crossterm::execute;
use crossterm::terminal::EnterAlternateScreen;
use crossterm::terminal::LeaveAlternateScreen;
use crossterm::terminal::disable_raw_mode;
use crossterm::terminal::enable_raw_mode;
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::time::MissedTickBehavior;

use crate::client::DashboardClient;
use crate::client::DashboardEvent;
use crate::model::DashboardAgentEntry;
use crate::model::DashboardParentEntry;
use crate::model::DashboardPreview;
use crate::model::DashboardSnapshot;
use crate::model::PreviewSource;
use crate::model::short_thread_id;
use crate::model::thread_label;
use crate::picker;
use crate::picker::SessionPickerState;
use crate::preview::committed_preview;
use crate::preview::normalize_preview;
use crate::render;

#[derive(Parser, Debug, Clone)]
pub struct Cli {
    /// Websocket URL for the running Codex app-server.
    #[arg(long, default_value = "ws://127.0.0.1:4222")]
    websocket_url: String,

    /// Parent thread to monitor. When omitted, the dashboard auto-selects the
    /// most recently updated loaded root thread.
    #[arg(long)]
    parent_thread_id: Option<String>,

    /// Show a startup picker even when only one loaded root thread exists.
    #[arg(long, default_value_t = false)]
    pick: bool,

    /// Poll interval used to reconcile loaded threads and committed previews.
    #[arg(long, default_value_t = 1000)]
    refresh_ms: u64,
}

pub async fn run(cli: Cli) -> Result<()> {
    let mut client = DashboardClient::connect(cli.websocket_url).await?;
    let mut terminal = None;
    let parent_thread_id = match cli.parent_thread_id {
        Some(parent_thread_id) => parent_thread_id,
        None => {
            let root_threads = client.load_root_threads().await?;
            if root_threads.len() == 1 && !cli.pick {
                root_threads[0].id.clone()
            } else {
                let terminal_ref = terminal.get_or_insert(DashboardTerminal::enter()?);
                let mut picker = SessionPickerState::from_threads(root_threads)?;
                match picker::pick_parent_thread_id(
                    &mut |state| terminal_ref.draw(|frame| picker::render(frame, state)),
                    &mut picker,
                )
                .await
                {
                    Ok(parent_thread_id) => parent_thread_id,
                    Err(err) => {
                        let _ = terminal_ref.exit();
                        client.shutdown().await;
                        return Err(err);
                    }
                }
            }
        }
    };
    let mut app = DashboardApp::new(parent_thread_id);
    app.refresh(&client).await;

    let mut terminal = match terminal {
        Some(terminal) => terminal,
        None => DashboardTerminal::enter()?,
    };
    let run_result = run_loop(
        &mut terminal,
        &mut client,
        &mut app,
        cli.refresh_ms.max(250),
    )
    .await;
    let restore_result = terminal.exit();
    client.shutdown().await;
    run_result.and(restore_result)
}

async fn run_loop(
    terminal: &mut DashboardTerminal,
    client: &mut DashboardClient,
    app: &mut DashboardApp,
    refresh_ms: u64,
) -> Result<()> {
    let mut input = EventStream::new();
    let mut refresh = tokio::time::interval(Duration::from_millis(refresh_ms));
    refresh.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut connection_open = true;

    loop {
        let snapshot = app.snapshot();
        terminal.draw(|frame| render::render(frame, &snapshot))?;

        tokio::select! {
            _ = refresh.tick(), if connection_open => {
                app.refresh(client).await;
            }
            maybe_event = client.next_event(), if connection_open => {
                let Some(event) = maybe_event else {
                    app.disconnect("app-server event stream closed".to_string());
                    connection_open = false;
                    continue;
                };
                match event {
                    DashboardEvent::Disconnected(message) => {
                        app.disconnect(message);
                        connection_open = false;
                    }
                    event => {
                        if app.handle_event(event) {
                            app.refresh(client).await;
                        }
                    }
                }
            }
            maybe_input = input.next() => {
                match maybe_input.transpose()? {
                    Some(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => break,
                            KeyCode::Char('r') => app.refresh(client).await,
                            _ => {}
                        }
                    }
                    Some(_) | None => {}
                }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }

    Ok(())
}

struct DashboardApp {
    parent_thread_id: String,
    parent_label: String,
    parent: Option<DashboardParentEntry>,
    agents: HashMap<String, DashboardAgentEntry>,
    refresh_error: Option<String>,
}

impl DashboardApp {
    fn new(parent_thread_id: String) -> Self {
        Self {
            parent_thread_id,
            parent_label: "loading…".to_string(),
            parent: None,
            agents: HashMap::new(),
            refresh_error: None,
        }
    }

    async fn refresh(&mut self, client: &DashboardClient) {
        match client.load_threads(/*include_turns*/ true).await {
            Ok(threads) => {
                self.apply_refresh(threads);
            }
            Err(err) => {
                self.refresh_error = Some(err.to_string());
            }
        }
    }

    fn apply_refresh(&mut self, threads: Vec<Thread>) {
        let previous_parent = self.parent.take();
        let parent = threads
            .iter()
            .find(|thread| thread.id == self.parent_thread_id)
            .cloned();

        if let Some(parent) = parent {
            self.parent_label = thread_label(&parent);
            let preview = match (thread_is_active(&parent.status), previous_parent.as_ref()) {
                (true, Some(existing)) if existing.preview.live => existing.preview.clone(),
                _ => committed_preview(&parent),
            };
            self.parent = Some(DashboardParentEntry::from_thread(&parent, preview));
            self.refresh_error = None;
        } else {
            self.parent_label = format!("thread {}", short_thread_id(&self.parent_thread_id));
            self.parent = None;
            self.refresh_error = Some(format!(
                "parent thread {} is not loaded in the connected app-server",
                self.parent_thread_id
            ));
        }

        let previous_agents = std::mem::take(&mut self.agents);
        self.agents = threads
            .into_iter()
            .filter(|thread| {
                thread.parent_thread_id.as_deref() == Some(self.parent_thread_id.as_str())
            })
            .map(|thread| {
                let preview = match (
                    thread_is_active(&thread.status),
                    previous_agents.get(&thread.id),
                ) {
                    (true, Some(existing)) if existing.preview.live => existing.preview.clone(),
                    _ => committed_preview(&thread),
                };
                (
                    thread.id.clone(),
                    DashboardAgentEntry::from_thread(&thread, preview),
                )
            })
            .collect();
    }

    fn disconnect(&mut self, message: String) {
        self.refresh_error = Some(message);
    }

    fn handle_event(&mut self, event: DashboardEvent) -> bool {
        match event {
            DashboardEvent::Notification(notification) => self.handle_notification(*notification),
            DashboardEvent::Lagged(skipped) => {
                self.refresh_error = Some(format!(
                    "dashboard skipped {skipped} app-server notifications; refreshing"
                ));
                true
            }
            DashboardEvent::Disconnected(_) => false,
        }
    }

    fn handle_notification(&mut self, notification: ServerNotification) -> bool {
        match notification {
            ServerNotification::ThreadStarted(notification) => {
                let thread = notification.thread;
                if thread.id == self.parent_thread_id {
                    self.parent_label = thread_label(&thread);
                    self.parent = Some(DashboardParentEntry::from_thread(
                        &thread,
                        committed_preview(&thread),
                    ));
                    self.refresh_error = None;
                    return false;
                }
                if thread.parent_thread_id.as_deref() != Some(self.parent_thread_id.as_str()) {
                    return false;
                }
                let preview = committed_preview(&thread);
                self.agents.insert(
                    thread.id.clone(),
                    DashboardAgentEntry::from_thread(&thread, preview),
                );
                false
            }
            ServerNotification::ThreadStatusChanged(notification) => {
                if notification.thread_id == self.parent_thread_id {
                    let Some(parent) = self.parent.as_mut() else {
                        return true;
                    };
                    parent.status = notification.status.clone();
                    parent.updated_at = chrono::Utc::now().timestamp();
                    if !thread_is_active(&notification.status) {
                        return true;
                    }
                    return false;
                }
                let Some(agent) = self.agents.get_mut(&notification.thread_id) else {
                    return false;
                };
                agent.status = notification.status.clone();
                if !thread_is_active(&notification.status) {
                    return true;
                }
                false
            }
            ServerNotification::ThreadClosed(notification) => {
                if notification.thread_id == self.parent_thread_id {
                    self.parent = None;
                    self.agents.clear();
                    self.refresh_error = Some(format!(
                        "parent thread {} was closed",
                        short_thread_id(&notification.thread_id)
                    ));
                    return false;
                }
                self.agents.remove(&notification.thread_id);
                false
            }
            ServerNotification::AgentMessageDelta(notification) => {
                self.apply_live_delta(
                    &notification.thread_id,
                    notification.delta,
                    PreviewSource::AgentMessage,
                );
                false
            }
            ServerNotification::PlanDelta(notification) => {
                self.apply_live_delta(
                    &notification.thread_id,
                    notification.delta,
                    PreviewSource::Plan,
                );
                false
            }
            ServerNotification::ReasoningSummaryTextDelta(notification) => {
                self.apply_live_delta(
                    &notification.thread_id,
                    notification.delta,
                    PreviewSource::ReasoningSummary,
                );
                false
            }
            ServerNotification::ReasoningTextDelta(notification) => {
                self.apply_live_delta(
                    &notification.thread_id,
                    notification.delta,
                    PreviewSource::ReasoningText,
                );
                false
            }
            _ => false,
        }
    }

    fn apply_live_delta(&mut self, thread_id: &str, delta: String, source: PreviewSource) {
        if thread_id == self.parent_thread_id {
            let Some(parent) = self.parent.as_mut() else {
                return;
            };
            apply_preview_delta(&mut parent.preview, &delta, source);
            parent.updated_at = chrono::Utc::now().timestamp();
            return;
        }
        let Some(agent) = self.agents.get_mut(thread_id) else {
            return;
        };
        apply_preview_delta(&mut agent.preview, &delta, source);
        agent.updated_at = chrono::Utc::now().timestamp();
    }

    fn snapshot(&self) -> DashboardSnapshot {
        let mut agents = self.agents.values().cloned().collect::<Vec<_>>();
        agents.sort_by(|left, right| {
            left.status_rank()
                .cmp(&right.status_rank())
                .then_with(|| right.updated_at.cmp(&left.updated_at))
                .then_with(|| left.thread_id.cmp(&right.thread_id))
        });
        DashboardSnapshot {
            parent_thread_id: self.parent_thread_id.clone(),
            parent_label: self.parent_label.clone(),
            parent: self.parent.clone(),
            agents,
            refresh_error: self.refresh_error.clone(),
        }
    }
}

fn apply_preview_delta(preview: &mut DashboardPreview, delta: &str, source: PreviewSource) {
    if !preview.live || source < preview.source {
        *preview = DashboardPreview {
            text: normalize_preview(delta),
            source,
            live: true,
        };
    } else if source == preview.source {
        preview.text.push_str(delta);
        preview.text = normalize_preview(&preview.text);
        preview.live = true;
    }
}

fn thread_is_active(status: &ThreadStatus) -> bool {
    matches!(status, ThreadStatus::Active { .. })
}

struct DashboardTerminal {
    terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
}

impl DashboardTerminal {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }

    fn draw<F>(&mut self, render_fn: F) -> Result<()>
    where
        F: FnOnce(&mut ratatui::Frame<'_>),
    {
        self.terminal.draw(render_fn)?;
        Ok(())
    }

    fn exit(&mut self) -> Result<()> {
        disable_raw_mode()?;
        execute!(self.terminal.backend_mut(), LeaveAlternateScreen)?;
        self.terminal.show_cursor()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::SessionSource;
    use codex_app_server_protocol::ThreadActiveFlag;
    use codex_app_server_protocol::ThreadStatus;
    use std::path::PathBuf;

    fn thread(id: &str, parent_thread_id: Option<&str>, status: ThreadStatus) -> Thread {
        Thread {
            id: id.to_string(),
            preview: format!("{id} preview"),
            ephemeral: false,
            model_provider: "mock".to_string(),
            created_at: 0,
            updated_at: 1,
            status,
            path: None,
            cwd: PathBuf::from("/tmp"),
            cli_version: "0.0.0".to_string(),
            source: SessionSource::Exec,
            parent_thread_id: parent_thread_id.map(str::to_string),
            agent_nickname: Some(format!("{id}-nick")),
            agent_role: Some("explorer".to_string()),
            git_info: None,
            name: Some(format!("{id}-name")),
            turns: Vec::new(),
        }
    }

    #[test]
    fn apply_live_delta_updates_parent_preview() {
        let mut app = DashboardApp::new("parent".to_string());
        app.apply_refresh(vec![thread(
            "parent",
            None,
            ThreadStatus::Active {
                active_flags: vec![ThreadActiveFlag::WaitingOnUserInput],
            },
        )]);

        app.apply_live_delta("parent", "live update".to_string(), PreviewSource::Plan);

        let snapshot = app.snapshot();
        let parent = snapshot.parent.expect("parent should be present");
        assert_eq!(
            parent.preview,
            DashboardPreview {
                text: "live update".to_string(),
                source: PreviewSource::Plan,
                live: true,
            }
        );
    }

    #[test]
    fn apply_refresh_preserves_live_parent_preview_while_parent_is_active() {
        let mut app = DashboardApp::new("parent".to_string());
        app.apply_refresh(vec![thread(
            "parent",
            None,
            ThreadStatus::Active {
                active_flags: vec![ThreadActiveFlag::WaitingOnUserInput],
            },
        )]);
        app.apply_live_delta("parent", "live update".to_string(), PreviewSource::Plan);

        app.apply_refresh(vec![thread(
            "parent",
            None,
            ThreadStatus::Active {
                active_flags: vec![ThreadActiveFlag::WaitingOnUserInput],
            },
        )]);

        let snapshot = app.snapshot();
        let parent = snapshot.parent.expect("parent should be present");
        assert_eq!(parent.preview.text, "live update");
        assert_eq!(parent.preview.source, PreviewSource::Plan);
        assert!(parent.preview.live);
    }
}
