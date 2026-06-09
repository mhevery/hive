use clap::Parser;
use std::path::PathBuf;

mod agent_record;
mod grok_processor;

use agent_record::AgentRecord;

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
            Ok(recs) => {
                if recs.is_empty() {
                    println!("(No Grok sessions found under {})", root.display());
                } else {
                    println!("Discovered {} Grok session(s) from {}", recs.len(), root.display());
                }
                recs
            }
            Err(e) => {
                eprintln!("Warning: failed to parse Grok sessions under {:?}: {}", root, e);
                eprintln!("Falling back to demo data.");
                demo_records()
            }
        },
        None => {
            println!("(Could not determine Grok sessions directory. Using demo data.)");
            demo_records()
        }
    };

    if !records.is_empty() {
        println!();
        println!("Sessions (newest activity first):");
        for record in &records {
            println!(
                "  [{}] {} | {} | Last: {} | Dir: {}",
                record.status,
                record.id,
                record.summary,
                record.last_generated_msg.format("%Y-%m-%d %H:%M:%S UTC"),
                record.working_dir.display()
            );
        }
    }

    println!();
    println!("Run 'hive --help' to see available options.");
    println!("(The grok_processor reads ~/.grok/sessions/.../summary.json + events to build AgentRecords.)");
}

/// Hard-coded demo records used only when real parsing is unavailable.
fn demo_records() -> Vec<AgentRecord> {
    use chrono::Utc;
    use agent_record::AgentStatus;

    vec![
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
    ]
}
