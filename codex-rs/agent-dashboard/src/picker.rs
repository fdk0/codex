use anyhow::Result;
use anyhow::bail;
use chrono::Local;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadStatus;
use crossterm::event::Event;
use crossterm::event::EventStream;
use crossterm::event::KeyCode;
use crossterm::event::KeyEventKind;
use futures::StreamExt;
use ratatui::Frame;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::style::Color;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use textwrap::wrap;

use crate::model::short_thread_id;
use crate::model::thread_label;
use crate::model::thread_project_name;
use crate::preview::normalize_preview;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ParentThreadChoice {
    pub(crate) thread_id: String,
    pub(crate) project_name: String,
    pub(crate) label: String,
    pub(crate) cwd: String,
    pub(crate) updated_at: i64,
    pub(crate) status: ThreadStatus,
    pub(crate) preview: String,
}

impl ParentThreadChoice {
    fn from_thread(thread: Thread) -> Self {
        let project_name =
            thread_project_name(&thread).unwrap_or_else(|| short_thread_id(&thread.id));
        Self {
            thread_id: thread.id.clone(),
            project_name,
            label: thread_label(&thread),
            cwd: thread.cwd.to_string_lossy().to_string(),
            updated_at: thread.updated_at,
            status: thread.status,
            preview: normalize_preview(&thread.preview),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SessionPickerState {
    choices: Vec<ParentThreadChoice>,
    selected: usize,
}

impl SessionPickerState {
    pub(crate) fn from_threads(threads: Vec<Thread>) -> Result<Self> {
        let mut choices = threads
            .into_iter()
            .filter(|thread| thread.parent_thread_id.is_none())
            .map(ParentThreadChoice::from_thread)
            .collect::<Vec<_>>();
        choices.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.thread_id.cmp(&right.thread_id))
        });
        if choices.is_empty() {
            bail!("no loaded root thread found; start or resume a remote session first");
        }
        Ok(Self {
            choices,
            selected: 0,
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.choices.len()
    }

    pub(crate) fn selected_thread_id(&self) -> &str {
        &self.choices[self.selected].thread_id
    }

    pub(crate) fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub(crate) fn move_down(&mut self) {
        if self.selected + 1 < self.choices.len() {
            self.selected += 1;
        }
    }

    fn visible_choices(&self, height: usize) -> (std::ops::Range<usize>, &[ParentThreadChoice]) {
        let row_height = 5usize;
        let visible = (height / row_height).max(1);
        let start = self
            .selected
            .saturating_sub(visible.saturating_sub(1))
            .min(self.choices.len().saturating_sub(visible));
        let end = (start + visible).min(self.choices.len());
        (start..end, &self.choices[start..end])
    }
}

pub(crate) async fn pick_parent_thread_id<Draw>(
    draw: &mut Draw,
    state: &mut SessionPickerState,
) -> Result<String>
where
    Draw: FnMut(&SessionPickerState) -> Result<()>,
{
    let mut input = EventStream::new();
    loop {
        draw(state)?;
        tokio::select! {
            maybe_input = input.next() => {
                match maybe_input.transpose()? {
                    Some(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                        match key.code {
                            KeyCode::Up | KeyCode::Char('k') => state.move_up(),
                            KeyCode::Down | KeyCode::Char('j') => state.move_down(),
                            KeyCode::Enter => return Ok(state.selected_thread_id().to_string()),
                            KeyCode::Esc | KeyCode::Char('q') => bail!("session picker aborted"),
                            _ => {}
                        }
                    }
                    Some(_) | None => {}
                }
            }
            _ = tokio::signal::ctrl_c() => bail!("session picker aborted"),
        }
    }
}

pub(crate) fn render(frame: &mut Frame<'_>, state: &SessionPickerState) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(area);

    let header = Paragraph::new(Line::from(vec![
        "Select parent session".bold(),
        " · ".dim(),
        format!("{} loaded roots", state.len()).cyan(),
    ]));
    frame.render_widget(header, layout[0]);

    let (range, visible) = state.visible_choices(layout[1].height as usize);
    let constraints = visible
        .iter()
        .map(|_| Constraint::Length(5))
        .collect::<Vec<_>>();
    let rows = Layout::vertical(constraints).split(layout[1]);

    for (index, choice) in visible.iter().enumerate() {
        let absolute_index = range.start + index;
        render_choice(frame, rows[index], choice, absolute_index == state.selected);
    }

    let hidden_above = range.start;
    let hidden_below = state.len().saturating_sub(range.end);
    let footer = match (hidden_above, hidden_below) {
        (0, 0) => "↑/↓ or j/k move · enter select · q quit".to_string(),
        _ => format!(
            "↑/↓ or j/k move · enter select · q quit · hidden: {hidden_above} above, {hidden_below} below"
        ),
    };
    frame.render_widget(Paragraph::new(footer.dim()), layout[2]);
}

fn render_choice(
    frame: &mut Frame<'_>,
    area: ratatui::layout::Rect,
    choice: &ParentThreadChoice,
    selected: bool,
) {
    let (status_label, status_color) = status_pill(&choice.status);
    let timestamp = format_timestamp(choice.updated_at);
    let short_id = short_thread_id(&choice.thread_id);
    let selector = if selected {
        "› ".cyan().bold()
    } else {
        "  ".into()
    };
    let mut title = vec![selector, choice.project_name.clone().bold()];
    if choice.label != choice.project_name {
        title.push(" · ".dim());
        title.push(choice.label.clone().into());
    }
    title.push(" · ".dim());
    title.push(short_id.dim());
    title.push(" · ".dim());
    title.push(status_label.fg(status_color));
    title.push(" · ".dim());
    title.push(timestamp.dim());
    let title = Line::from(title);

    let preview = if choice.preview.is_empty() {
        "No preview.".to_string()
    } else {
        choice.preview.clone()
    };
    let preview_line = wrap(&preview, area.width.saturating_sub(4) as usize)
        .into_iter()
        .next()
        .map(std::borrow::Cow::into_owned)
        .unwrap_or_default();
    let lines = vec![
        title,
        Line::from(vec!["  cwd: ".dim(), choice.cwd.clone().into()]),
        Line::from(vec!["  preview: ".dim(), preview_line.into()]),
    ];
    let block = if selected {
        Block::default()
            .borders(Borders::ALL)
            .border_style(Color::Cyan)
    } else {
        Block::default().borders(Borders::ALL)
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn format_timestamp(updated_at: i64) -> String {
    match chrono::DateTime::from_timestamp(updated_at, 0) {
        Some(timestamp) => timestamp
            .with_timezone(&Local)
            .format("%H:%M:%S")
            .to_string(),
        None => "unknown".to_string(),
    }
}

fn status_pill(status: &ThreadStatus) -> (&'static str, Color) {
    match status {
        ThreadStatus::Active { .. } => ("active", Color::LightGreen),
        ThreadStatus::Idle => ("idle", Color::Cyan),
        ThreadStatus::SystemError => ("error", Color::LightRed),
        ThreadStatus::NotLoaded => ("notLoaded", Color::DarkGray),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::SessionSource;
    use std::path::PathBuf;

    fn thread(id: &str, updated_at: i64, parent_thread_id: Option<&str>) -> Thread {
        Thread {
            id: id.to_string(),
            preview: format!("{id} preview"),
            ephemeral: false,
            model_provider: "mock".to_string(),
            created_at: 0,
            updated_at,
            status: ThreadStatus::Idle,
            path: None,
            cwd: PathBuf::from(format!("/tmp/{id}")),
            cli_version: "0.0.0".to_string(),
            source: SessionSource::Exec,
            parent_thread_id: parent_thread_id.map(str::to_string),
            agent_nickname: None,
            agent_role: None,
            git_info: None,
            name: Some(format!("{id}-name")),
            turns: Vec::new(),
        }
    }

    #[test]
    fn picker_filters_to_root_threads_and_sorts_by_updated_at() {
        let state = SessionPickerState::from_threads(vec![
            thread("older-root", 1, None),
            thread("child", 99, Some("parent")),
            thread("newer-root", 5, None),
        ])
        .expect("picker state");

        assert_eq!(state.len(), 2);
        assert_eq!(state.choices[0].thread_id, "newer-root");
        assert_eq!(state.choices[1].thread_id, "older-root");
    }

    #[test]
    fn picker_selection_stays_in_bounds() {
        let mut state = SessionPickerState::from_threads(vec![
            thread("root-a", 1, None),
            thread("root-b", 2, None),
        ])
        .expect("picker state");

        state.move_up();
        assert_eq!(state.selected, 0);
        state.move_down();
        state.move_down();
        assert_eq!(state.selected, 1);
    }

    #[test]
    fn picker_uses_cwd_basename_as_project_name() {
        let state = SessionPickerState::from_threads(vec![thread("root-a", 1, None)])
            .expect("picker state");

        assert_eq!(state.choices[0].project_name, "root-a");
    }
}
