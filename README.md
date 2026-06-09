# Hive

**Hive** is a lightweight Rust CLI tool for discovering, monitoring, and managing multiple AI coding agents across your projects.

Instead of acting as a PTY proxy or intercepting terminal output, Hive reads the rich on-disk session data that the agents already write.

## What it does

Hive currently focuses on **Grok Build** sessions (stored under `~/.grok/sessions/`), with the architecture ready to support other agents (Codex, Claude, Aider, Cursor, etc.) in the future.

For each session it extracts:

- Working directory
- Session ID
- Summary / title
- Last active timestamp
- Live status (`Thinking` vs `Waiting`), inferred structurally from `chat_history.jsonl` (last speaker + pending tool calls) rather than just wall-clock time

Sessions are grouped by working directory (with `~` expansion for your home directory), sorted by recency within each group, and directories are shown in a clean, aligned table.

### Key features

- Clean grouped table output
- Structural status detection (Thinking / Waiting)
- Relative time display ("5 min ago", "2 days ago", etc.)
- Live `--watch` / `-w` mode that watches the filesystem and refreshes the UI continuously
- Uses `ratatui` + `crossterm` for nice terminal rendering (with colors and proper alternate screen support in watch mode)
- Falls back to demo data when no real sessions are present

## Installation

```bash
# From source
cargo install --path .

# Or build a release binary
cargo build --release
```

## Usage

```bash
# One-shot view
hive list

# Live updating view (press q, Esc, or Ctrl-C to exit)
hive list -w
hive list --watch
```

### Help output

```text
Hive - Multi-Agent Manager

Usage: hive [COMMAND]

Commands:
  list  List active and recent agent sessions (newest first)
  help  Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

### `hive list --help`

```text
List active and recent agent sessions (newest first)

Usage: hive list [OPTIONS]

Options:
  -w, --watch  Watch the filesystem and continuously refresh the output (Ctrl-C to exit)
  -h, --help   Print help
```

## How it works (briefly)

Hive looks for sessions in:

- `$GROK_SESSIONS_DIR` (if set), otherwise
- `~/.grok/sessions/`

It reads `summary.json` for metadata and `chat_history.jsonl` to determine current status. No network access or agent instrumentation is required.

In watch mode it uses the `notify` crate to react to filesystem changes under the sessions directory and redraws the table in an alternate screen buffer.

## Development

```bash
cargo run -- list
cargo run -- list -w
cargo test
```

## License

This is currently an experimental / learning project. License TBD.