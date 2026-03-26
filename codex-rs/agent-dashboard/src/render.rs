use chrono::Local;
use codex_app_server_protocol::ThreadStatus;
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

use crate::model::DashboardAgentEntry;
use crate::model::DashboardParentEntry;
use crate::model::DashboardSnapshot;

pub(crate) fn render(frame: &mut Frame<'_>, snapshot: &DashboardSnapshot) {
    let area = frame.area();
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(area);

    frame.render_widget(header(snapshot), layout[0]);

    let (parent_area, agents_area) = split_body(layout[1], snapshot.parent.is_some());
    if let (Some(parent), Some(parent_area)) = (&snapshot.parent, parent_area) {
        render_parent(frame, parent_area, parent);
    }

    let Some(agents_area) = agents_area else {
        frame.render_widget(footer(snapshot.agents.len()), layout[2]);
        return;
    };

    if snapshot.agents.is_empty() {
        frame.render_widget(empty_state(snapshot), agents_area);
        frame.render_widget(footer(0), layout[2]);
        return;
    }

    let preview_lines = preview_line_count(agents_area.height as usize, snapshot.agents.len());
    let entry_height = (preview_lines + 2) as u16;
    let visible_agents = (agents_area.height / entry_height).max(1) as usize;
    let shown_agents = snapshot
        .agents
        .iter()
        .take(visible_agents)
        .collect::<Vec<_>>();
    let hidden_agents = snapshot.agents.len().saturating_sub(shown_agents.len());

    let body_constraints = shown_agents
        .iter()
        .map(|_| Constraint::Length(entry_height))
        .collect::<Vec<_>>();
    let body_layout = Layout::vertical(body_constraints).split(agents_area);

    for (index, agent) in shown_agents.iter().enumerate() {
        render_agent(frame, body_layout[index], agent, preview_lines);
    }

    frame.render_widget(footer(hidden_agents), layout[2]);
}

fn header(snapshot: &DashboardSnapshot) -> Paragraph<'static> {
    let title = vec![
        "Agent dashboard".bold(),
        " · ".dim(),
        snapshot.parent_label.clone().cyan(),
        " · ".dim(),
        format!("parent {}", snapshot.parent_thread_id).dim(),
        " · ".dim(),
        format!("{} open agents", snapshot.agents.len()).green(),
    ];
    Paragraph::new(Line::from(title))
}

fn empty_state(snapshot: &DashboardSnapshot) -> Paragraph<'static> {
    let mut lines = vec![
        Line::from(vec![
            "No open child-agent threads for ".into(),
            snapshot.parent_label.clone().cyan(),
            ".".into(),
        ]),
        Line::from(""),
        Line::from("Leave this dashboard open while the parent session spawns new agents.".dim()),
    ];
    if let Some(error) = &snapshot.refresh_error {
        lines.push(Line::from(""));
        lines.push(Line::from(error.clone().red()));
    }
    Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("status"))
}

fn footer(hidden_agents: usize) -> Paragraph<'static> {
    let message = if hidden_agents == 0 {
        "q quit · r refresh".to_string()
    } else {
        format!("q quit · r refresh · {hidden_agents} more agents hidden by terminal height")
    };
    Paragraph::new(message.dim())
}

fn render_agent(
    frame: &mut Frame<'_>,
    area: ratatui::layout::Rect,
    agent: &DashboardAgentEntry,
    preview_lines: usize,
) {
    frame.render_widget(Clear, area);
    let (status_label, status_color) = status_pill(&agent.status);
    let timestamp = format_timestamp(agent.updated_at);
    let title = Line::from(vec![
        agent.title().bold(),
        " · ".dim(),
        status_label.fg(status_color),
        " · ".dim(),
        timestamp.dim(),
    ]);

    let wrapped = wrap(&agent.preview.text, area.width.saturating_sub(4) as usize)
        .into_iter()
        .take(preview_lines)
        .map(|line| Line::from(line.into_owned()))
        .collect::<Vec<_>>();

    let mut lines = Vec::with_capacity(preview_lines + 1);
    lines.push(Line::from(vec![
        format!("[{}]", agent.preview.source.label()).dim(),
        " ".into(),
        if agent.preview.live {
            "live".cyan().italic()
        } else {
            "committed".dim()
        },
    ]));
    if wrapped.is_empty() {
        lines.push(Line::from("No preview yet.".dim()));
    } else {
        lines.extend(wrapped);
    }

    let paragraph =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(paragraph, area);
}

fn render_parent(
    frame: &mut Frame<'_>,
    area: ratatui::layout::Rect,
    parent: &DashboardParentEntry,
) {
    frame.render_widget(Clear, area);
    let (status_label, status_color) = status_pill(&parent.status);
    let timestamp = format_timestamp(parent.updated_at);
    let title = Line::from(vec![
        format!("Parent: {}", parent.label).bold(),
        " · ".dim(),
        status_label.fg(status_color),
        " · ".dim(),
        timestamp.dim(),
    ]);

    let wrapped = wrap(&parent.preview.text, area.width.saturating_sub(4) as usize)
        .into_iter()
        .take(parent_preview_line_count(area.height as usize))
        .map(|line| Line::from(line.into_owned()))
        .collect::<Vec<_>>();

    let mut lines = Vec::with_capacity(wrapped.len() + 1);
    lines.push(Line::from(vec![
        format!("[{}]", parent.preview.source.label()).dim(),
        " ".into(),
        if parent.preview.live {
            "live".cyan().italic()
        } else {
            "committed".dim()
        },
    ]));
    if wrapped.is_empty() {
        lines.push(Line::from("No preview yet.".dim()));
    } else {
        lines.extend(wrapped);
    }

    let paragraph =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(paragraph, area);
}

fn split_body(
    body_area: ratatui::layout::Rect,
    has_parent: bool,
) -> (Option<ratatui::layout::Rect>, Option<ratatui::layout::Rect>) {
    if !has_parent {
        return (None, Some(body_area));
    }
    if body_area.height <= parent_panel_height(body_area.height as usize) as u16 {
        return (Some(body_area), None);
    }
    let parent_height = parent_panel_height(body_area.height as usize);
    let layout = Layout::vertical([Constraint::Length(parent_height as u16), Constraint::Min(1)])
        .split(body_area);
    (Some(layout[0]), Some(layout[1]))
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

pub(crate) fn preview_line_count(height: usize, agent_count: usize) -> usize {
    for preview_lines in [3, 2, 1] {
        let entry_height = preview_lines + 2;
        if agent_count.saturating_mul(entry_height) <= height {
            return preview_lines;
        }
    }
    1
}

fn parent_panel_height(body_height: usize) -> usize {
    parent_preview_line_count(body_height) + 2
}

fn parent_preview_line_count(body_height: usize) -> usize {
    if body_height >= 12 {
        3
    } else if body_height >= 8 {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn preview_line_count_prefers_spacious_layout_when_room_allows() {
        assert_eq!(preview_line_count(18, 3), 3);
    }

    #[test]
    fn preview_line_count_compacts_when_space_is_tight() {
        assert_eq!(preview_line_count(8, 3), 1);
    }

    #[test]
    fn parent_preview_line_count_prefers_three_lines_when_room_allows() {
        assert_eq!(parent_preview_line_count(12), 3);
    }

    #[test]
    fn parent_preview_line_count_compacts_for_short_body() {
        assert_eq!(parent_preview_line_count(7), 1);
    }
}
