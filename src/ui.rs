use anyhow::Result;
use chrono::{DateTime, Utc};
use crossterm::{
    queue,
    style::{Attribute, Color as CrosstermColor, Print, style, ResetColor, Stylize},
    terminal,
};
use std::io::Write;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
pub fn render_sessions_table(records: &[AgentRecord]) -> Result<()> {
    if records.is_empty() {
        return Ok(());
    }

    // Group sessions by working directory
    let mut groups: HashMap<PathBuf, Vec<&AgentRecord>> = HashMap::new();
    for r in records {
        groups.entry(r.working_dir.clone()).or_default().push(r);
    }

    // Sort sessions within each group by most recent first
    for group in groups.values_mut() {
        group.sort_by(|a, b| b.last_generated_msg.cmp(&a.last_generated_msg));
    }

    // Collect groups and sort them alphabetically by directory (using the displayed ~ form)
    let mut group_list: Vec<(PathBuf, Vec<&AgentRecord>)> = groups.into_iter().collect();
    group_list.sort_by(|(a, _), (b, _)| {
        format_working_dir(a).cmp(&format_working_dir(b))
    });

    // Build rows: group header rows + data rows (with blank first column for visual indent)
    let mut rows: Vec<Row> = Vec::new();
    let mut header_dirs: Vec<(usize, String)> = Vec::new(); // (index in `rows`, formatted dir for spilling)

    for (i, (dir, sessions)) in group_list.iter().enumerate() {
        // Insert a blank row before each directory header (except the first)
        // to visually separate groups.
        if i > 0 {
            rows.push(Row::new(vec![
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
            ]));
        }

        // Group header row: directory on its own (visually)
        let formatted_dir = format_working_dir(dir);
        let dir_cell = Cell::from(formatted_dir.clone())
            .style(Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan));

        let group_header = Row::new(vec![
            dir_cell,
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
        ]);

        let header_body_idx = rows.len();
        rows.push(group_header);
        header_dirs.push((header_body_idx, formatted_dir));

        // Data rows for this directory (first column left blank for indent)
        for s in sessions {
            let status_style = match s.status {
                AgentStatus::Thinking => Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                AgentStatus::Waiting => Style::default().fg(Color::DarkGray),
            };
            let status_cell = Cell::from(s.status.to_string()).style(status_style);

            let data_row = Row::new(vec![
                Cell::from("  "), // small indent under the directory header
                status_cell,
                Cell::from(s.id.clone()),
                Cell::from(s.summary.clone()),
                Cell::from(format_last_active(s.last_generated_msg)),
            ]);
            rows.push(data_row);
        }
    }

    let header = Row::new(vec!["Dir", "Status", "ID", "Summary", "Last Active"])
        .style(Style::default().add_modifier(Modifier::BOLD))
        .bottom_margin(1);

    // First column capped at 5 characters (just enough for the "Dir" header).
    // On group header rows the full directory path is spread across the row
    // during post-processing (spilling over the empty columns).
    let widths = [
        Constraint::Max(5),     // Dir - max width 5
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

    // Discover which rows in the rendered buffer are the group header rows
    // (they have a path starting with ~ or / in the left content area).
    // Then spread the corresponding full directory across the row so it
    // visually spills over the other columns (which are empty on those rows).
    let mut candidate_ys: Vec<u16> = Vec::new();
    for y in area.y..(area.y + area.height) {
        let sx = area.x + 1;
        if sx < area.x + area.width {
            let s = buf.get(sx, y).symbol();
            if s.starts_with('~') || s.starts_with('/') {
                candidate_ys.push(y);
            }
        }
    }

    for (i, y) in candidate_ys.into_iter().enumerate() {
        if let Some((_, full_dir)) = header_dirs.get(i) {
            spread_dir_over_row(&mut buf, y, area, full_dir);
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
    let start_x = area.x + 1;            // after left border
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
