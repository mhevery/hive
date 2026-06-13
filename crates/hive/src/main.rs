use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::stdout;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

mod agent_record;
mod codex_processor;
mod grok_processor;
mod summarizer_client;
mod ui;

use agent_record::AgentRecord;

/// Hive - Multi-Agent Manager
///
/// A tool to discover, monitor, and manage AI coding agents (Grok Build, Codex, Claude, Aider, etc.)
/// across your projects by reading their local session and transcript data.
#[derive(Parser, Debug)]
#[command(name = "hive", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug, Clone)]
enum Commands {
    /// List active and recent agent sessions (newest first)
    List {
        /// Only show sessions in this directory or one of its descendants
        directory: Option<PathBuf>,

        /// Watch the filesystem and continuously refresh the output (Ctrl-C to exit)
        #[arg(short, long)]
        watch: bool,
    },

    /// Test the local text summarizer (Falconsai/text_summarization T5 model via Candle).
    /// Provide the text to summarize as one or more arguments (will be joined),
    /// or pipe text via stdin (e.g. `cat notes.txt | hive summarize`).
    Summarize {
        /// The text to summarize. If omitted, reads from stdin.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        text: Option<Vec<String>>,
    },
}

fn main() {
    let cli = Cli::parse();

    let cmd = cli.command.unwrap_or(Commands::List {
        directory: None,
        watch: false,
    });
    if let Err(e) = run_command(cmd) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn run_command(cmd: Commands) -> Result<()> {
    match cmd {
        Commands::List { watch, directory } => run_list(watch, directory),
        Commands::Summarize { text } => run_summarize(text),
    }
}

fn run_list(watch: bool, directory: Option<PathBuf>) -> Result<()> {
    // Discover sessions by running the grok processor against the real Grok sessions dir
    // (or a directory supplied via GROK_SESSIONS_DIR for testing / alternate locations).
    let sessions_root: Option<PathBuf> = std::env::var_os("GROK_SESSIONS_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".grok").join("sessions"))
        });

    let directory_filter = directory.map(|dir| normalize_path(&dir));

    if !watch {
        let records = load_records(&sessions_root, directory_filter.as_deref());
        render_once(&records)
    } else {
        run_watch(&sessions_root, directory_filter)
    }
}

fn load_records(grok_root: &Option<PathBuf>, directory_filter: Option<&Path>) -> Vec<AgentRecord> {
    let mut records: Vec<AgentRecord> = vec![];

    // Grok
    if let Some(root) = grok_root {
        match grok_processor::parse_grok_sessions(root) {
            Ok(mut recs) => records.append(&mut recs),
            Err(e) => {
                eprintln!(
                    "Warning: failed to parse Grok sessions under {:?}: {}",
                    root, e
                );
            }
        }
    }

    // Codex - prefer ~/.codex/sessions (real location from user data), then CODEX_SESSIONS_DIR, then ~/sessions
    let codex_root: Option<PathBuf> = std::env::var_os("CODEX_SESSIONS_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex").join("sessions"))
        })
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("sessions")));
    if let Some(root) = codex_root {
        match codex_processor::parse_codex_sessions(&root) {
            Ok(mut recs) => records.append(&mut recs),
            Err(e) => {
                eprintln!(
                    "Warning: failed to parse Codex sessions under {:?}: {}",
                    root, e
                );
            }
        }
    }

    // Sort combined newest first
    records.sort_by(|a, b| b.last_generated_msg.cmp(&a.last_generated_msg));
    if let Some(filter) = directory_filter {
        records.retain(|record| is_in_or_under(&record.working_dir, filter));
    }
    records
}

fn is_in_or_under(path: &Path, directory: &Path) -> bool {
    let path = normalize_path(path);
    path == directory || path.starts_with(directory)
}

fn normalize_path(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };

    normalize_lexically(&absolute)
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn render_once(records: &[AgentRecord]) -> Result<()> {
    if records.is_empty() {
        println!("No agent sessions found.");
        return Ok(());
    }
    ui::render_sessions_table(records)
}

fn run_watch(sessions_root: &Option<PathBuf>, directory_filter: Option<PathBuf>) -> Result<()> {
    use notify::{RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();
    let mut watchers: Vec<RecommendedWatcher> = vec![];

    // Watch Grok root
    if let Some(root) = sessions_root {
        if root.exists() {
            let mut w: RecommendedWatcher =
                RecommendedWatcher::new(tx.clone(), notify::Config::default())
                    .expect("failed to create watcher");
            if w.watch(&root, RecursiveMode::Recursive).is_ok() {
                watchers.push(w);
            }
        }
    }

    // Watch Codex root too (if different)
    let codex_root: Option<PathBuf> = std::env::var_os("CODEX_SESSIONS_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex").join("sessions"))
        })
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("sessions")));
    if let Some(root) = codex_root {
        if root.exists() {
            let mut w: RecommendedWatcher =
                RecommendedWatcher::new(tx.clone(), notify::Config::default())
                    .expect("failed to create watcher");
            if w.watch(&root, RecursiveMode::Recursive).is_ok() {
                watchers.push(w);
            }
        }
    }

    // Proper ratatui setup for live updating (alternate screen, raw mode, etc.)
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, cursor::Hide)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // We use catch_unwind so we can reliably restore the terminal even if a panic occurs.
    // Note: On SIGINT (Ctrl-C) the process is usually killed before this cleanup runs.
    // Users can run `reset` in their shell if needed.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        loop {
            // Draw using proper ratatui (this handles efficient updates internally)
            terminal.draw(|f| {
                ui::render_sessions_to_frame(
                    f,
                    f.size(),
                    &load_records(sessions_root, directory_filter.as_deref()),
                    true,
                );
            })?;

            // Wait responsively for either:
            // - a key press ('q' or Esc or Ctrl-C to exit cleanly)
            // - a FS event from notify (to refresh)
            // - a timeout (periodic refresh safety net)
            // We poll keys frequently so 'q' exits immediately, like Ctrl-C.
            let wait_start = std::time::Instant::now();
            let max_wait = Duration::from_secs(2);
            let key_poll = Duration::from_millis(50);

            while wait_start.elapsed() < max_wait {
                if event::poll(key_poll)? {
                    if let Event::Key(key) = event::read()? {
                        let is_ctrl_c = key.code == KeyCode::Char('c')
                            && key
                                .modifiers
                                .contains(crossterm::event::KeyModifiers::CONTROL);
                        if key.code == KeyCode::Char('q')
                            || key.code == KeyCode::Char('Q')
                            || key.code == KeyCode::Esc
                            || is_ctrl_c
                        {
                            return Ok(());
                        }
                    }
                }

                // Non-blocking check for FS events
                if rx.try_recv().is_ok() {
                    break; // FS change -> refresh
                }

                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }));

    // Always restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), cursor::Show, LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(e) => std::panic::resume_unwind(e),
    }
}

/// Collect text for summarization: either from the provided CLI args (joined by space)
/// or, if none given, by reading all of stdin.
/// This helper is now always available (it has no dependency on the ML crates).
fn collect_text(text: Option<Vec<String>>) -> Result<String> {
    if let Some(parts) = text {
        if !parts.is_empty() {
            return Ok(parts.join(" "));
        }
    }
    // No args (or empty) — read from stdin. This supports pipelines:
    //   echo "long text..." | hive summarize
    //   cat document.txt | hive summarize
    use std::io::{self, Read};
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

/// Always-present command handler. When the "summarizer" feature is not enabled
/// at build time we emit a clear message telling the user how to activate it.
fn run_summarize(text: Option<Vec<String>>) -> Result<()> {
    let input = collect_text(text)?;
    let input = input.trim();
    if input.is_empty() {
        anyhow::bail!(
            "No text provided to summarize.\n\
             Pass text as arguments, e.g.:\n  hive summarize \"The quick brown fox...\"\n\
             Or pipe via stdin, e.g.:\n  cat mydoc.txt | hive summarize"
        );
    }

    // Delegate to the separate `hive-summarizer` executable.
    // This keeps the main `hive` binary free of the heavy Candle / ML dependencies.
    // The client will locate the binary (via HIVE_SUMMARIZER, next to exe, PATH, etc.)
    // and stream the text over stdin.
    match summarizer_client::summarize_via_external(input) {
        Ok(summary) => {
            println!("{}", summary);
            Ok(())
        }
        Err(e) => {
            anyhow::bail!(
                "Failed to run the summarizer component: {}\n\n\
                 The 'summarize' functionality lives in a separate binary\n\
                 (`hive-summarizer`) that contains the local LLM (Candle + T5).\n\n\
                 To build it from this workspace:\n\
                   cargo build -p hive-summarizer --release\n\n\
                 Make it discoverable by one of:\n\
                   - copy the binary next to the `hive` binary\n\
                   - add its directory to PATH\n\
                   - export HIVE_SUMMARIZER=/path/to/hive-summarizer\n\n\
                 Then use:\n\
                   hive summarize \"your text...\"\n\
                   cat transcript.txt | hive summarize",
                e
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_record::{AgentSource, AgentStatus};
    use chrono::Utc;

    fn record_in(path: &str) -> AgentRecord {
        AgentRecord::new(
            path.to_string(),
            "summary".to_string(),
            AgentStatus::Waiting,
            Utc::now(),
            PathBuf::from(path),
            AgentSource::Codex,
        )
    }

    #[test]
    fn directory_filter_matches_directory_and_descendants_only() {
        let base = normalize_path(Path::new("/tmp/hive-project"));
        let inside = base.join("child");
        let sibling = normalize_path(Path::new("/tmp/hive-project-sibling"));

        assert!(is_in_or_under(&base, &base));
        assert!(is_in_or_under(&inside, &base));
        assert!(!is_in_or_under(&sibling, &base));
    }

    #[test]
    fn dot_filter_resolves_to_current_directory() {
        let current = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
        assert_eq!(normalize_path(Path::new(".")), current);
        assert!(is_in_or_under(
            &current.join("nested"),
            &normalize_path(Path::new("."))
        ));
    }

    #[test]
    fn record_filter_keeps_only_sessions_in_current_tree() {
        let current = normalize_path(Path::new("."));
        let child = current.join("child");
        let outside = current.parent().unwrap_or(&current).join("outside");

        let mut records = vec![
            record_in(child.to_string_lossy().as_ref()),
            record_in(outside.to_string_lossy().as_ref()),
        ];
        records.retain(|record| is_in_or_under(&record.working_dir, &current));

        assert_eq!(records.len(), 1);
        assert_eq!(normalize_path(&records[0].working_dir), child);
    }
}
