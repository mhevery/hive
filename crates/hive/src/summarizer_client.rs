use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

enum SummarizerCommand {
    Binary(PathBuf),
    CargoRun { workspace_root: PathBuf },
}

/// Locate the `hive-summarizer` binary.
///
/// Lookup order (first hit wins):
/// 1. HIVE_SUMMARIZER env var (full path) — best for development & CI.
/// 2. Next to the current executable (common when the two binaries are co-installed).
/// 3. In a conventional user plugin location (~/.hive/bin/hive-summarizer).
/// 4. Via PATH (plain `hive-summarizer` command name).
///
/// In a cargo workspace development context we fall back to
/// `cargo run -p hive-summarizer --` so that `cargo run -p hive -- summarize "..."`
/// "just works" without the user having to manually build the sibling first.
fn find_summarizer_command() -> SummarizerCommand {
    // 1. Explicit override (highest priority, great for `cargo run -p hive` dev workflows).
    if let Ok(p) = std::env::var("HIVE_SUMMARIZER") {
        let p = PathBuf::from(p);
        if p.exists() {
            return SummarizerCommand::Binary(p);
        }
        // If the env var points at something that doesn't exist yet, we still
        // return it — the spawn will give a nice OS error.
        return SummarizerCommand::Binary(p);
    }

    // 2. Next to the hive binary itself.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // Common names
            for name in ["hive-summarizer", "hive-summarizer.exe"] {
                let candidate = dir.join(name);
                if candidate.exists() {
                    return SummarizerCommand::Binary(candidate);
                }
            }
        }
    }

    // 3. User-local plugin dir (simple HOME/USERPROFILE lookup to avoid pulling
    // a dirs/home crate that can have high MSRV requirements).
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        let home = PathBuf::from(home);
        for name in ["hive-summarizer", "hive-summarizer.exe"] {
            let candidate = home.join(".hive").join("bin").join(name);
            if candidate.exists() {
                return SummarizerCommand::Binary(candidate);
            }
        }
    }

    // 4. Fall back to PATH lookup (manual, no extra crate, to keep MSRV low).
    let path_candidate = PathBuf::from(if cfg!(windows) {
        "hive-summarizer.exe"
    } else {
        "hive-summarizer"
    });
    if is_executable_on_path(&path_candidate) {
        return SummarizerCommand::Binary(path_candidate);
    }

    // Dev convenience: when running from this workspace via `cargo run`, the
    // sibling binary may not exist yet in target/debug. Ask Cargo to build/run it.
    if let Some(workspace_root) = find_workspace_root() {
        return SummarizerCommand::CargoRun { workspace_root };
    }

    // Final fallback — the spawn will fail with a nice message.
    SummarizerCommand::Binary(path_candidate)
}

/// Very small PATH search so we don't need the `which` crate (helps keep the
/// MSRV of the light `hive` crate reasonable).
fn is_executable_on_path(name: &PathBuf) -> bool {
    if let Ok(path_var) = std::env::var("PATH") {
        let sep = if cfg!(windows) { ';' } else { ':' };
        for dir in path_var.split(sep) {
            let candidate = PathBuf::from(dir).join(name);
            if candidate.exists() {
                // On Unix we could also check for executable bit, but for our
                // purposes "exists in a PATH dir" is good enough.
                return true;
            }
        }
    }
    false
}

fn find_workspace_root() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| find_workspace_root_from(&exe))
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|cwd| find_workspace_root_from(&cwd))
        })
}

fn find_workspace_root_from(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let manifest = dir.join("Cargo.toml");
        let summarizer_manifest = dir
            .join("crates")
            .join("hive-summarizer")
            .join("Cargo.toml");
        if manifest.exists() && summarizer_manifest.exists() {
            return Some(dir.to_path_buf());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_workspace_root_from_target_binary_path() {
        let root = find_workspace_root_from(&std::env::current_dir().unwrap()).unwrap();
        let fake_hive_exe = root.join("target").join("debug").join("hive");
        assert_eq!(find_workspace_root_from(&fake_hive_exe), Some(root));
    }
}

/// Run the external summarizer by spawning the helper binary and streaming
/// the input text through stdin. Returns the summary from stdout.
///
/// This is the "only loads if needed" mechanism: the heavy Candle code and the
/// model are only brought into memory when this function (or equivalent) is
/// actually called.
pub fn run_external_summarizer(text: &str) -> Result<String> {
    let summarizer = find_summarizer_command();

    let mut cmd = match &summarizer {
        SummarizerCommand::CargoRun { workspace_root } => {
            let mut c = Command::new("cargo");
            c.arg("run")
                .arg("--manifest-path")
                .arg(workspace_root.join("Cargo.toml"))
                .arg("-p")
                .arg("hive-summarizer")
                .arg("--")
                .arg("-");
            c
        }
        SummarizerCommand::Binary(bin) => {
            let mut c = Command::new(bin);
            c.arg("-");
            c
        }
    };

    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit()) // Progress / model loading messages go to the user's terminal.
        .spawn()
        .with_context(|| format!("failed to spawn summarizer ({})", summarizer.description()))?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .context("failed to open stdin of summarizer")?;
        use std::io::Write;
        stdin.write_all(text.as_bytes())?;
    }

    let output = child
        .wait_with_output()
        .context("failed to read output from summarizer")?;

    if !output.status.success() {
        anyhow::bail!(
            "summarizer process exited with status {:?}",
            output.status.code()
        );
    }

    let summary =
        String::from_utf8(output.stdout).context("summarizer produced non-UTF8 output")?;

    Ok(summary.trim().to_string())
}

/// Convenience for the explicit `hive summarize` subcommand.
/// Tries the external binary first. If it cannot be found / fails, we give a
/// helpful message (the caller can decide to print it).
pub fn summarize_via_external(text: &str) -> Result<String> {
    if std::env::var("HIVE_SUMMARIZER").ok().as_deref() == Some("passthrough") {
        // Test helper: bypass the real binary for hermetic client tests.
        return Ok(format!("[passthrough] {}", text.trim()));
    }
    run_external_summarizer(text)
}

impl SummarizerCommand {
    fn description(&self) -> String {
        match self {
            SummarizerCommand::Binary(path) => format!("tried {:?}", path),
            SummarizerCommand::CargoRun { workspace_root } => {
                format!("tried cargo run -p hive-summarizer in {:?}", workspace_root)
            }
        }
    }
}
