use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

/// Locate the `hive-summarizer` binary.
///
/// Lookup order (first hit wins):
/// 1. HIVE_SUMMARIZER env var (full path) — best for development & CI.
/// 2. Next to the current executable (common when the two binaries are co-installed).
/// 3. In a conventional user plugin location (~/.hive/bin/hive-summarizer).
/// 4. Via PATH (plain `hive-summarizer` command name).
///
/// In a cargo workspace development context we also try to fall back to
/// `cargo run -p hive-summarizer --` so that `cargo run -p hive -- summarize "..."`
/// "just works" without the user having to manually build the sibling first.
fn find_summarizer_binary() -> Result<PathBuf> {
    // 1. Explicit override (highest priority, great for `cargo run -p hive` dev workflows).
    if let Ok(p) = std::env::var("HIVE_SUMMARIZER") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Ok(p);
        }
        // If the env var points at something that doesn't exist yet, we still
        // return it — the spawn will give a nice OS error.
        return Ok(p);
    }

    // 2. Next to the hive binary itself.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // Common names
            for name in ["hive-summarizer", "hive-summarizer.exe"] {
                let candidate = dir.join(name);
                if candidate.exists() {
                    return Ok(candidate);
                }
            }
        }
    }

    // 3. User-local plugin dir (simple HOME/USERPROFILE lookup to avoid pulling
    // a dirs/home crate that can have high MSRV requirements).
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        let home = PathBuf::from(home);
        for name in ["hive-summarizer", "hive-summarizer.exe"] {
            let candidate = home
                .join(".hive")
                .join("bin")
                .join(name);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    // 4. Fall back to PATH lookup (manual, no extra crate, to keep MSRV low).
    let path_candidate = PathBuf::from(if cfg!(windows) { "hive-summarizer.exe" } else { "hive-summarizer" });
    if is_executable_on_path(&path_candidate) {
        return Ok(path_candidate);
    }

    // Dev convenience (opt-in): if HIVE_DEV_SUMMARIZER=1 and we look like a cargo
    // workspace build, use `cargo run -p hive-summarizer --` so that
    // `cargo run -p hive -- summarize "..."` works without a pre-built binary.
    // This avoids accidentally kicking off heavy ML compiles on every `cargo run`.
    if std::env::var("HIVE_DEV_SUMMARIZER").ok().as_deref() == Some("1")
        && (std::env::var("CARGO").is_ok() || std::env::var("CARGO_MANIFEST_DIR").is_ok())
    {
        return Ok(PathBuf::from("__CARGO_RUN_HIVE_SUMMARIZER__"));
    }

    // Final fallback — the spawn will fail with a nice message.
    Ok(path_candidate)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_mode_returns_marked_output() {
        // This exercises the client without needing any real binary or heavy deps.
        std::env::set_var("HIVE_SUMMARIZER", "passthrough");
        let out = summarize_via_external("hello world from the test").unwrap();
        assert!(out.contains("[passthrough]"));
        assert!(out.contains("hello world from the test"));
        std::env::remove_var("HIVE_SUMMARIZER");
    }
}

/// Run the external summarizer by spawning the helper binary and streaming
/// the input text through stdin. Returns the summary from stdout.
///
/// This is the "only loads if needed" mechanism: the heavy Candle code and the
/// model are only brought into memory when this function (or equivalent) is
/// actually called.
pub fn run_external_summarizer(text: &str) -> Result<String> {
    let bin = find_summarizer_binary()?;

    let mut cmd = if bin == PathBuf::from("__CARGO_RUN_HIVE_SUMMARIZER__") {
        // Dev convenience inside the workspace.
        let mut c = Command::new("cargo");
        c.arg("run")
            .arg("-p")
            .arg("hive-summarizer")
            .arg("--");
        c
    } else {
        Command::new(&bin)
    };

    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit()) // Progress / model loading messages go to the user's terminal.
        .spawn()
        .with_context(|| format!("failed to spawn summarizer (tried {:?})", bin))?;

    {
        let stdin = child.stdin.as_mut().context("failed to open stdin of summarizer")?;
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

    let summary = String::from_utf8(output.stdout)
        .context("summarizer produced non-UTF8 output")?;

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