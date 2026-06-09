use clap::Parser;

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
    println!("Run 'hive --help' to see available options.");
}
