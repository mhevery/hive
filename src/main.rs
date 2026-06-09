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
    List,
}

fn main() {
    let cli = Cli::parse();

    let cmd = cli.command.unwrap_or(Commands::List);
    if let Err(e) = run_command(cmd) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn run_command(cmd: Commands) -> Result<()> {
    match cmd {
        Commands::List => run_list(),
    }
}

fn run_list() -> Result<()> {
    // Discover sessions by running the grok processor against the real Grok sessions dir
    // (or a directory supplied via GROK_SESSIONS_DIR for testing / alternate locations).
    let sessions_root: Option<PathBuf> = std::env::var_os("GROK_SESSIONS_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".grok").join("sessions"))
        });

    let records: Vec<AgentRecord> = match sessions_root {
        Some(root) => match grok_processor::parse_grok_sessions(&root) {
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
    };

    if records.is_empty() {
        println!("No agent sessions found.");
        return Ok(());
    }

    ui::render_sessions_table(&records)
}
