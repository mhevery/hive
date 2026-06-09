use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod agent_record;
mod grok_processor;
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
        /// Watch the filesystem and continuously refresh the output (Ctrl-C to exit)
        #[arg(short, long)]
        watch: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    let cmd = cli.command.unwrap_or(Commands::List { watch: false });
    if let Err(e) = run_command(cmd) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn run_command(cmd: Commands) -> Result<()> {
    match cmd {
        Commands::List { watch } => run_list(watch),
    }
}

fn run_list(watch: bool) -> Result<()> {
    // Discover sessions by running the grok processor against the real Grok sessions dir
    // (or a directory supplied via GROK_SESSIONS_DIR for testing / alternate locations).
    let sessions_root: Option<PathBuf> = std::env::var_os("GROK_SESSIONS_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".grok").join("sessions"))
        });

    if !watch {
        let records = load_records(&sessions_root);
        render_once(&records)
    } else {
        run_watch(&sessions_root)
    }
}

fn load_records(sessions_root: &Option<PathBuf>) -> Vec<AgentRecord> {
    match sessions_root {
        Some(root) => match grok_processor::parse_grok_sessions(root) {
            Ok(recs) => recs,
            Err(e) => {
                eprintln!("Warning: failed to parse Grok sessions under {:?}: {}", root, e);
                Vec::new()
            }
        },
        None => {
            eprintln!("Could not determine Grok sessions directory.");
            Vec::new()
        }
    }
}

fn render_once(records: &[AgentRecord]) -> Result<()> {
    if records.is_empty() {
        println!("No agent sessions found.");
        return Ok(());
    }
    ui::render_sessions_table(records)
}

fn run_watch(sessions_root: &Option<PathBuf>) -> Result<()> {
    use crossterm::{cursor, execute, terminal};
    use notify::{RecommendedWatcher, RecursiveMode, Watcher};
    use std::sync::mpsc;
    use std::time::Duration;

    let (tx, rx) = mpsc::channel();
    let mut watcher: RecommendedWatcher =
        RecommendedWatcher::new(tx, notify::Config::default()).expect("failed to create watcher");

    if let Some(root) = sessions_root {
        if root.exists() {
            watcher
                .watch(root, RecursiveMode::Recursive)
                .expect("failed to watch sessions directory");
        }
    }

    // Hide cursor during live updates to reduce flicker
    let mut stdout = std::io::stdout();
    execute!(stdout, cursor::Hide).ok();

    loop {
        execute!(stdout, cursor::MoveTo(0, 0)).ok();
        execute!(stdout, terminal::Clear(terminal::ClearType::FromCursorDown)).ok();

        println!("Watching for changes... (Ctrl-C to exit)\n");
        refresh_display(sessions_root)?;

        // Block until we get a filesystem event or a timeout (for periodic safety)
        let _ = rx.recv_timeout(Duration::from_secs(2));
    }

    // Note: cursor restore is unreachable in normal Ctrl-C exit.
    // A signal handler + alternate screen would be needed for perfect cleanup.
}

fn refresh_display(sessions_root: &Option<PathBuf>) -> Result<()> {
    let records = load_records(sessions_root);
    render_once(&records)
}
