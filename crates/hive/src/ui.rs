use anyhow::Result;
use chrono::{DateTime, Utc};
use crossterm::{
    queue,
    style::{style, Attribute, Color as CrosstermColor, Print, ResetColor, Stylize},
    terminal,
};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Rect},
    prelude::{Frame, Widget},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
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

/// Replace a path's home directory prefix with `~` if possible.
fn format_working_dir(path: &Path) -> String {
    let s = path.display().to_string();
    if let Ok(home) = std::env::var("HOME") {
        if let Some(rest) = s.strip_prefix(&home) {
            if rest.is_empty() {
                return "~".to_string();
            }
            return format!("~{}", rest);
        }
    }
    s
}

/// Render the list of sessions using a ratatui Table widget for clean column alignment.
/// The widget is rendered into an offscreen Buffer and then printed row-by-row.
/// This produces properly aligned columns while keeping output in the terminal scrollback.
///
/// See: https://docs.rs/ratatui-widgets/latest/ratatui_widgets/table/struct.Table.html
/// (core Table widget is also available via ratatui::widgets::Table)
pub fn group_records<'a>(records: &'a [AgentRecord]) -> Vec<(PathBuf, Vec<&'a AgentRecord>)> {
    if records.is_empty() {
        return vec![];
    }

    // Group sessions by working directory
    let mut groups: HashMap<PathBuf, Vec<&'a AgentRecord>> = HashMap::new();
    for r in records {
        groups.entry(r.working_dir.clone()).or_default().push(r);
    }

    // Sort sessions within each group by most recent first
    for group in groups.values_mut() {
        group.sort_by(|a, b| b.last_generated_msg.cmp(&a.last_generated_msg));
    }

    // Collect groups and sort them alphabetically by directory (using the displayed ~ form)
    let mut group_list: Vec<(PathBuf, Vec<&'a AgentRecord>)> = groups.into_iter().collect();
    group_list.sort_by(|(a, _), (b, _)| format_working_dir(a).cmp(&format_working_dir(b)));

    group_list
}

fn build_table_rows(records: &[AgentRecord]) -> (Vec<Row>, Vec<(usize, String)>) {
    let group_list = group_records(records);
    let mut rows: Vec<Row> = Vec::new();
    let mut header_dirs: Vec<(usize, String)> = Vec::new();

    for (i, (dir, sessions)) in group_list.iter().enumerate() {
        if i > 0 {
            rows.push(Row::new(vec![
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
            ]));
        }

        let formatted_dir = format_working_dir(dir);

        // Use empty cells for the group header row in the Table.
        // The full directory name will be overlaid (in TUI) or spread (in non-TUI buffer)
        // across the entire row width for the "spill" visual.
        // This prevents the clipped dir from appearing in the narrow "Dir" column.
        let group_header = Row::new(vec![
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
        ]);

        let header_body_idx = rows.len();
        rows.push(group_header);
        header_dirs.push((header_body_idx, formatted_dir));

        for s in sessions {
            let status_style = match s.status {
                AgentStatus::Thinking => Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                AgentStatus::Waiting => Style::default().fg(Color::DarkGray),
            };
            let status_cell = Cell::from(s.status.to_string()).style(status_style);
            let agent_cell = Cell::from(s.source.to_string());

            let data_row = Row::new(vec![
                Cell::from("  "),
                agent_cell,
                status_cell,
                Cell::from(s.id.clone()),
                Cell::from(s.summary.clone()),
                Cell::from(format_last_active(s.last_generated_msg)),
            ]);
            rows.push(data_row);
        }
    }

    (rows, header_dirs)
}

pub fn render_sessions_table(records: &[AgentRecord]) -> Result<()> {
    let (rows, header_dirs) = build_table_rows(records);
    if rows.is_empty() {
        return Ok(());
    }

    let header = Row::new(vec![
        "Dir",
        "Agent",
        "Status",
        "ID",
        "Summary",
        "Last Active",
    ])
    .style(Style::default().add_modifier(Modifier::BOLD))
    .bottom_margin(1);

    // Dir capped at 5 (as requested). Agent column added to distinguish Grok vs Codex.
    let widths = [
        Constraint::Max(5),     // Dir - max width 5
        Constraint::Length(7),  // Agent (Grok / Codex)
        Constraint::Length(9),  // Status
        Constraint::Length(37), // full session ID
        Constraint::Min(22),    // Summary
        Constraint::Min(13),    // relative time
    ];

    // Height based on actual rows we created (group headers + data + separators).
    // Add some extra for the outer border + table header.
    let height = (rows.len() as u16 + 8).min(400);

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
    let area = Rect {
        x: 0,
        y: 0,
        width,
        height,
    };

    let mut buf = Buffer::empty(area);
    table.render(area, &mut buf);

    // Spread the full directory on the exact group header rows (using the
    // collected body indices) so it spills across the row. This works even
    // though we put empty cells in the group_header rows (prevents clipped
    // dir from the narrow column).
    let table_header_height: u16 = 3; // table header (1) + bottom_margin (1) + top border offset -> body starts at +3
    for (body_idx, full_dir) in &header_dirs {
        let row_y = area.y + table_header_height + *body_idx as u16;
        if row_y < area.y + area.height {
            spread_dir_over_row(&mut buf, row_y, area, full_dir);
        }
    }

    // Print each row from the buffer, using crossterm's StyledContent so that
    // each cell carries its own foreground color + modifiers (bold).
    //
    // We use `queue!` + a single flush at the end instead of `execute!` per cell.
    // This makes the whole frame much more atomic from the terminal's point of
    // view, which dramatically reduces flicker during --watch refreshes.
    //
    // (We still avoid a full Terminal + alternate screen in normal mode so that
    // plain `hive list` output stays in the user's scrollback.)
    let mut out = std::io::stdout();
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

            queue!(out, Print(styled)).ok();
        }

        // Make sure styles are reset after each row (for borders / next content)
        queue!(out, ResetColor).ok();
        queue!(out, Print("\n")).ok();
    }

    // One flush at the end = one big atomic update instead of hundreds of tiny ones.
    out.flush().ok();

    Ok(())
}

/// Proper ratatui rendering for --watch mode.
/// Uses the exact same rows/header/widths/block as the non-watch path for
/// visual consistency (including the narrow "Dir" column and blank separators).
/// Then overlays full-width directory text on the group header rows to
/// replicate the "spill" effect.
pub fn render_sessions_to_frame(
    frame: &mut Frame,
    area: Rect,
    records: &[AgentRecord],
    watch: bool,
) {
    let (rows, header_dirs) = build_table_rows(records);
    if rows.is_empty() {
        let msg = Paragraph::new("No agent sessions found.");
        frame.render_widget(msg, area);
        return;
    }

    let header = Row::new(vec![
        "Dir",
        "Agent",
        "Status",
        "ID",
        "Summary",
        "Last Active",
    ])
    .style(Style::default().add_modifier(Modifier::BOLD))
    .bottom_margin(1);

    let widths = [
        Constraint::Max(5),     // Dir
        Constraint::Length(7),  // Agent (Grok / Codex)
        Constraint::Length(9),  // Status
        Constraint::Length(37), // ID
        Constraint::Min(22),    // Summary
        Constraint::Min(13),    // Last Active
    ];

    let title = if watch {
        " Sessions (newest activity first) — Watching (Ctrl-C to exit) "
    } else {
        " Sessions (newest activity first) "
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .column_spacing(1);

    frame.render_widget(table, area);

    // Overlay the full directory names (spill) on the group header rows.
    // The table's own header row + its .bottom_margin(1) takes 2 rows;
    // the first body row starts at +3 from the table content top (after top border).
    // We draw inside the borders (start after left border) so the outer
    // left/right borders of the table remain on the header rows.
    let table_header_height: u16 = 3;
    let inner_x = area.x + 1;
    let inner_w = area.width.saturating_sub(2);
    for (body_idx, full_dir) in header_dirs {
        let row_y = area.y + table_header_height + body_idx as u16;
        if row_y < area.y + area.height {
            let text = if full_dir.len() < inner_w as usize {
                format!(
                    "{}{}",
                    full_dir,
                    " ".repeat(inner_w as usize - full_dir.len())
                )
            } else {
                full_dir.clone()
            };
            let dir_para = Paragraph::new(text).style(
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::Cyan),
            );
            let full_rect = Rect {
                x: inner_x,
                y: row_y,
                width: inner_w,
                height: 1,
            };
            frame.render_widget(dir_para, full_rect);
        }
    }
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

/// Spread a directory path across the inner width of a row in the buffer.
/// Used for group headers so the path can "spill" over the other (empty) columns.
fn spread_dir_over_row(buf: &mut Buffer, y: u16, area: Rect, dir: &str) {
    let start_x = area.x + 1; // after left border
    let max_x = area.x + area.width - 1; // before right border

    let style = Style::default()
        .add_modifier(Modifier::BOLD)
        .fg(Color::Cyan);

    let mut x = start_x;
    for ch in dir.chars() {
        if x >= max_x {
            break;
        }
        let cell = buf.get_mut(x, y);
        cell.set_symbol(&ch.to_string());
        cell.set_style(style);
        x += 1;
    }

    // Fill the rest of the inner row with the styled space so it reads as one
    // continuous directory line.
    while x < max_x {
        let cell = buf.get_mut(x, y);
        cell.set_symbol(" ");
        cell.set_style(style);
        x += 1;
    }
}
