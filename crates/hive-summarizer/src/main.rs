use std::io::{self, Read};

use anyhow::Result;
use hive_summarizer::TextSummarizer;

/// Standalone summarizer binary for Hive.
///
/// It can be used in two ways:
///
/// 1. As a helper invoked by the main `hive` binary (the recommended integration):
///    The caller writes the text to the process's stdin and reads the summary from stdout.
///    Progress / warnings go to stderr.
///
/// 2. Directly from the shell (very convenient for testing or ad-hoc use):
///    hive-summarizer "some long text..."
///    cat transcript.txt | hive-summarizer
///
/// The Falconsai/text_summarization assets are embedded in the binary, so runtime
/// inference does not require internet access or a populated Hugging Face cache.
fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {:#}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let input = if !args.is_empty() {
        // Direct invocation with arguments (joined, like the old `hive summarize` UX).
        args.join(" ")
    } else {
        // Streaming mode (what the hive client will use, and also great for pipes).
        // Read everything from stdin.
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        buf
    };

    if input.trim().is_empty() {
        eprintln!(
            "No text provided.\n\
             Examples:\n\
               hive-summarizer \"Long paragraph to summarize...\"\n\
               cat my-transcript.txt | hive-summarizer\n\
               echo 'text here' | hive-summarizer"
        );
        std::process::exit(2);
    }

    // Inform the user on stderr so that stdout remains a clean summary (important
    // both for piping and for the parent `hive` process).
    eprintln!("Loading embedded Falconsai/text_summarization model...");

    let mut summarizer = TextSummarizer::new()?;

    let summary = summarizer.summarize(&input)?;

    // The summary goes to stdout — this is the contract the `hive` client relies on.
    println!("{}", summary);

    Ok(())
}
