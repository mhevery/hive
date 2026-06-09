use clap::Parser;
use chrono::Utc;
use std::path::PathBuf;

mod agent_record;
use agent_record::{AgentRecord, AgentStatus};

/// Hive - Multi-Agent Manager
///
/// A tool to discover, monitor, and manage AI coding agents (Grok Build, Codex, Claude, Aider, etc.)
/// across your projects by reading their local session and transcript data.
#[derive(Parser, Debug)]
#[command(name = "hive", version, about, long_about = None)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
    println!("🐝 Hive - Agent Manager");
    println!();

    // Demo: create example records (in real use these will come from parsing ~/.grok/sessions etc.)
    let example_records = vec![
        AgentRecord::new(
            "019ea450-f4f1-7582-a9ee-7160ed4f9e71",
            "Initialize Git Repository in Local Directory and explore worktrees for agent isolation.",
            AgentStatus::Waiting,
            Utc::now(),
            PathBuf::from("/Users/misko/work/HelloRust"),
        ),
        AgentRecord::new(
            "demo-codex-123",
            "Refactoring authentication flow in the backend service.",
            AgentStatus::Thinking,
            Utc::now(),
            PathBuf::from("/Users/misko/work/my-backend"),
        ),
    ];

    println!("Active agents (demo):");
    for record in &example_records {
        println!(
            "  [{}] {} | {} | Last msg: {} | Dir: {}",
            record.status,
            record.id,
            record.summary,
            record.last_generated_msg.format("%Y-%m-%d %H:%M:%S UTC"),
            record.working_dir.display()
        );
    }

    println!();
    println!("Run 'hive --help' to see available options.");
    println!("(Real implementation will parse agent directories and use a small local LLM for summaries.)");
}
