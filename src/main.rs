use clap::Parser;

/// Hive - Multi-Agent Manager
#[derive(Parser, Debug)]
#[command(name = "hive", version, about = "Hive - Dashboard and manager for AI coding agents", long_about = None)]
struct Args {
    /// Optional name to greet
    #[arg(short, long)]
    name: Option<String>,
}

fn main() {
    let args = Args::parse();
    println!("🐝 Hive - Agent Manager");
    if let Some(name) = args.name {
        println!("Hello, {}!", name);
    } else {
        println!("Run 'hive --help' to get started.");
    }
}
