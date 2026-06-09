use anyhow::Result;
use crossterm::terminal;

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Rect},
    prelude::Widget,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Row, Table},
};

use crate::agent_record::{AgentRecord, AgentStatus};

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
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
                AgentStatus::Waiting => Style::default().fg(Color::DarkGray),
            };
            let status_cell = Cell::from(format!("[{}]", r.status)).style(status_style);

            // Show a short prefix of the ID for table density. The full ID remains
            // available via other commands / future detail views.
            let short_id = if r.id.len() > 12 {
                format!("{}…", &r.id[..12])
            } else {
                r.id.clone()
            };

            Row::new(vec![
                status_cell,
                Cell::from(short_id),
                Cell::from(r.summary.clone()),
                Cell::from(
                    r.last_generated_msg
                        .format("%Y-%m-%d %H:%M")
                        .to_string(),
                ),
                Cell::from(r.working_dir.display().to_string()),
            ])
        })
        .collect();

    let header = Row::new(vec!["Status", "ID", "Summary", "Last", "Working Dir"])
        .style(Style::default().add_modifier(Modifier::BOLD))
        .bottom_margin(1);

    // Tuned for typical terminal widths (100-160 cols). Status and short-ID are fixed;
    // timestamp is now shorter; summary + dir get the flexible space.
    let widths = [
        Constraint::Length(11), // [Thinking]
        Constraint::Length(13), // short ID
        Constraint::Min(22),
        Constraint::Length(16), // "YYYY-MM-DD HH:MM"
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
    // columns (especially the session ID) don't get crushed on smaller terminals.
    let (term_width, _) = terminal::size().unwrap_or((120, 40));
    let width = term_width.max(110);

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

    // Print each row from the buffer. This gives us the nice aligned table
    // as plain(ish) text that remains in scrollback after the process exits.
    for y in area.y..(area.y + area.height) {
        let mut line = String::new();
        for x in area.x..(area.x + area.width) {
            let cell = buf.get(x, y);
            line.push_str(cell.symbol());
        }
        let trimmed = line.trim_end();
        if !trimmed.is_empty() {
            println!("{}", trimmed);
        }
    }

    Ok(())
}
