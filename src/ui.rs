use anyhow::Result;
use chrono::{DateTime, Utc};
use crossterm::{
    execute,
    style::{Attribute, Color as CrosstermColor, Print, style, ResetColor, Stylize},
    terminal,
};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Rect},
    prelude::Widget,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Row, Table},
};

use crate::agent_record::{AgentRecord, AgentStatus};

/// Format a timestamp as a relative "time ago" string (e.g. "5 min ago", "2 hours ago").
fn format_last_active(dt: DateTime<Utc>) -> String {
    let now = Utc::now();
    let dur = now.signed_duration_since(dt);
    let secs = dur.num_seconds().abs();

    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        let mins = dur.num_minutes().abs();
        if mins == 1 {
            "1 min ago".to_string()
        } else {
            format!("{} min ago", mins)
        }
    } else if secs < 86400 {
        let hours = dur.num_hours().abs();
        if hours == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{} hours ago", hours)
        }
    } else {
        let days = dur.num_days().abs();
        if days == 1 {
            "1 day ago".to_string()
        } else {
            format!("{} days ago", days)
        }
    }
}

/// Render the list of sessions using a ratatui Table widget for clean column alignment.
/// The widget is rendered into an offscreen Buffer and then printed row-by-row.
/// This produces properly aligned columns while keeping output in the terminal scrollback.
///
/// See: https://docs.rs/ratatui-widgets/latest/ratatui_widgets/table/struct.Table.html
/// (core Table widget is also available via ratatui::widgets::Table)
pub fn render_sessions_table(records: &[AgentRecord]) -> Result<()> {
    let rows: Vec<Row> = records
        .iter()
        .map(|r| {
            let status_style = match r.status {
                AgentStatus::Thinking => Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                AgentStatus::Waiting => Style::default().fg(Color::DarkGray),
            };
            let status_cell = Cell::from(r.status.to_string()).style(status_style);

            Row::new(vec![
                status_cell,
                Cell::from(r.id.clone()),
                Cell::from(r.summary.clone()),
                Cell::from(format_last_active(r.last_generated_msg)),
                Cell::from(r.working_dir.display().to_string()),
            ])
        })
        .collect();

    let header = Row::new(vec!["Status", "ID", "Summary", "Last Active", "Working Dir"])
        .style(Style::default().add_modifier(Modifier::BOLD))
        .bottom_margin(1);

    // Tuned for typical terminal widths. Full 36-char IDs require more space.
    let widths = [
        Constraint::Length(9),  // Thinking / Waiting
        Constraint::Length(37), // full session ID (36 chars)
        Constraint::Min(22),
        Constraint::Min(13),    // relative time e.g. "3 hours ago"
        Constraint::Min(18),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Sessions (newest activity first) "),
        )
        .column_spacing(1);

    // Render the widget into a buffer (no terminal backend / cursor control needed).
    // Use current terminal width when possible, but ensure a comfortable minimum so
    // the full 36-char session IDs + other columns don't get crushed on smaller terminals.
    let (term_width, _) = terminal::size().unwrap_or((120, 40));
    let width = term_width.max(140);

    // Height: 1 (top border) + 1 (header) + 1 (header bottom margin) + N (rows) + 1 (bottom border)
    // Add a couple extra to be safe; the printing loop trims empty trailing lines.
    let height = (records.len() as u16 + 5).min(300);
    let area = Rect {
        x: 0,
        y: 0,
        width,
        height,
    };

    let mut buf = Buffer::empty(area);
    table.render(area, &mut buf);

    // Print each row from the buffer, using crossterm's StyledContent so that
    // each cell carries its own foreground color + modifiers (bold).
    // We do this instead of a full Terminal backend so the table remains
    // in the user's terminal scrollback after the command exits.
    for y in area.y..(area.y + area.height) {
        for x in area.x..(area.x + area.width) {
            let cell = buf.get(x, y);
            let symbol = cell.symbol().to_string();

            let mut styled = style(symbol);

            if let Some(ct_color) = to_crossterm_color(cell.fg) {
                styled = styled.with(ct_color);
            }

            if cell.modifier.contains(Modifier::BOLD) {
                styled = styled.attribute(Attribute::Bold);
            }

            execute!(std::io::stdout(), Print(styled)).ok();
        }

        // Make sure styles are reset after each row (for borders / next content)
        execute!(std::io::stdout(), ResetColor).ok();
        println!();
    }

    Ok(())
}

fn to_crossterm_color(color: Color) -> Option<CrosstermColor> {
    match color {
        Color::Green => Some(CrosstermColor::Green),
        Color::DarkGray => Some(CrosstermColor::DarkGrey),
        Color::Yellow => Some(CrosstermColor::Yellow),
        Color::Reset => None,
        _ => None,
    }
}
