# MEMORY.md — Hive Project Context

This file serves as a portable handoff / memory for continuing work on the Hive project in a fresh chat or session. It captures the current state, key decisions, implementation details, and next steps.

## Project Overview

**Hive** is a Rust CLI tool (designed as a weekend learning project) for observing and managing multiple AI coding agents.

**Core Problem it solves:**
> "It is hard to get an overview which agent is working on what in which directory what goal that directory is in."

Instead of intercepting PTY output or acting as a proxy, Hive reads the on-disk session data that agents already write:
- Grok Build: `~/.grok/sessions/<encoded-cwd>/<session-id>/` (summary.json, chat_history.jsonl, events.jsonl, etc.)
- Codex: `~/.codex/sessions/` (rollout-*.jsonl files)
- Future targets: Claude, Aider, Cursor, etc.

**Current focus:** CLI-first with a clean TUI for `list` and `list --watch`. The heavy optional functionality (local LLM summarization) has been deliberately isolated.

**Tech stack:**
- Rust (edition 2021)
- `clap` (derive) for CLI
- `chrono`, `serde`/`serde_json`, `anyhow`
- `ratatui` + `crossterm` + `notify` for the watch TUI
- Testing: `tempfile` + simulated directory fixtures
- Cargo workspace for clean separation of concerns

## Current Implementation Status (as of latest work)

### Project Structure (Cargo Workspace)
The project is organized as a workspace to keep the core binary lightweight:

- Root `Cargo.toml` — pure workspace manifest (`members = ["crates/hive", "crates/hive-summarizer"]`, `default-members = ["crates/hive"]`)
- `crates/hive/` — the main lightweight observer binary
- `crates/hive-summarizer/` — independent heavy binary containing all Candle + ML dependencies

This design was chosen so that `cargo build` / `cargo run` for the main tool never pulls the large ML dependency graph (Candle, hf-hub, tokenizers, etc.).

### Core Data Model
- `crates/hive/src/agent_record.rs`
  - `AgentRecord` struct (uniform across agents):
    - `id`, `summary`, `status`, `last_generated_msg`, `working_dir`, `source`
  - `AgentStatus` enum: `Thinking` / `Waiting`
  - `AgentSource` enum: `Grok` / `Codex`
  - The `summary` field explicitly supports being generated/refined by a local LLM from the full transcript.

### Processors
- **Grok** (`crates/hive/src/grok_processor.rs`)
  - `parse_grok_sessions`, `parse_grok_sessions_for_cwd`
  - Reads `summary.json` + `chat_history.jsonl`
  - Structural status inference from chat history (last speaker + pending tool_calls)
  - Optional LLM refinement when `HIVE_LLM_SUMMARIES=1` and native summary is weak (uses `extract_transcript_for_llm_summary` + external summarizer)

- **Codex** (`crates/hive/src/codex_processor.rs`)
  - `parse_codex_sessions` (recursive search for `rollout-*.jsonl`)
  - Parses `session_meta`, `event_msg`, `response_item`
  - Similar status logic + basic LLM refinement hook

### Summarization Architecture (Major Recent Work)
The local LLM summarizer (Candle + Falconsai/text_summarization T5) is **not** a Cargo feature inside the main binary.

Instead:
- `crates/hive-summarizer/` is a completely separate executable.
  - Contains all heavy deps (`candle-*`, `hf-hub`, `tokenizers`, plus pins for `gemm`/`half`/`rand` to make older Candle 0.5 build reliably).
  - Re-exports `TextSummarizer` in its lib + has a standalone `main` that reads text from stdin (or args) and writes a summary to stdout.
  - Can be used directly: `cat transcript.txt | hive-summarizer`

- `crates/hive/src/summarizer_client.rs`
  - Discovery logic (in priority order):
    1. `HIVE_SUMMARIZER` env var (full path)
    2. Next to the `hive` executable
    3. `~/.hive/bin/hive-summarizer`
    4. `PATH`
    5. (Dev only) `HIVE_DEV_SUMMARIZER=1` → auto `cargo run -p hive-summarizer`
  - Spawns the process, feeds text via stdin, reads summary from stdout.
  - Graceful fallback if the binary is missing.

- `hive summarize` subcommand (always visible in `--help`, even without the companion binary installed). It delegates to the client.

- Optional deep integration: when `HIVE_LLM_SUMMARIES=1`, the processors will attempt to refine weak native summaries using the full transcript via the external summarizer process.

This separate-process approach was chosen over:
- A Cargo feature (would pull heavy deps into every build)
- A cdylib + `libloading` (ABI pain, harder distribution, single-process crash risk)

Benefits: complete compile-time isolation, crash isolation, easy to swap backends later (Ollama, different model, etc.), and the summarizer binary remains useful on its own.

### CLI
- `hive list [--watch | -w]`
- `hive summarize [TEXT]...` (or pipe via stdin)
- The `summarize` subcommand is intentionally always listed so users discover the capability even if they haven't built/installed the companion binary yet.

### UI / TUI
- `crates/hive/src/ui.rs` — table rendering, grouping by working dir, relative time, status colors, watch-mode support using proper ratatui + alternate screen.
- Directory headers are rendered via a post-processing "spill" technique over the table.

### CLI Client for Summarizer
See `summarizer_client.rs` above. The main `hive` binary has **zero** ML dependencies.

### Testing Strategy
- All parsing tests remain hermetic using `tempfile` + realistic mock directory trees (`create_mock_grok_session`, `create_mock_codex_rollout`).
- The heavy real-model test lives inside `crates/hive-summarizer` (feature-gated style inside that crate).
- Client logic is tested with a `passthrough` mode (`HIVE_SUMMARIZER=passthrough`) so integration tests don't require the real binary.

### Environment Variables
- `HIVE_SUMMARIZER` — path to the summarizer binary (highest priority discovery)
- `HIVE_LLM_SUMMARIES=1` — enable LLM refinement of weak summaries from full transcripts in `list` output
- `HIVE_DEV_SUMMARIZER=1` — (workspace dev only) auto-delegate `summarize` to sibling via `cargo run -p ...`

### Git / Process
- Commits follow the convention of including a short summary of the originating user prompt(s) since the last commit.
- Work is done at `/Users/misko/work/hive`
- Commands should generally be run as `cd /Users/misko/work/hive && <cmd>` (or use full paths) when using `!` / terminal tools.

## Key Technical Decisions & Insights

### Separate Executables for Heavy Optional Features
The biggest recent architectural decision: heavy optional functionality (local LLM) lives in its own binary. This keeps the primary observer tool fast to build/test and small on disk, while still providing a first-class `hive summarize` experience and the ability to improve session summaries from full transcripts.

### Agent Data Formats
- Grok and Codex parsers both feed the same `AgentRecord` shape.
- Structural signals (speaker order + pending tools) are preferred for status over wall-clock time.

### Error Handling for Optional Components
The `summarize` command and LLM refinement paths are designed to degrade gracefully. If the companion binary is missing you get a clear, actionable message telling you how to build and discover it.

## Environment / Workspace Notes

- Project root: `/Users/misko/work/hive`
- Use the workspace: `cargo run -p hive ...` or just `cargo run ...` (defaults to `hive` via `default-members`).
- To build the summarizer: `cargo build -p hive-summarizer --release`
- To run with a locally built summarizer: `HIVE_SUMMARIZER=target/release/hive-summarizer cargo run -- summarize "..."`

## Current Files (high level)

**Workspace root**
- `Cargo.toml` (workspace manifest)
- `README.md`, `MEMORY.md`

**crates/hive** (main lightweight binary)
- `Cargo.toml`
- `src/main.rs` (CLI + orchestration + `load_records`)
- `src/agent_record.rs`
- `src/grok_processor.rs`
- `src/codex_processor.rs`
- `src/ui.rs`
- `src/summarizer_client.rs`

**crates/hive-summarizer** (heavy companion)
- `Cargo.toml` (contains all Candle/hf-hub/tokenizers + pins)
- `src/lib.rs` (`TextSummarizer` + T5 loading/generation logic)
- `src/main.rs` (standalone CLI that reads from stdin/args and writes summary to stdout; also useful directly)

## Open Items / Roadmap

**Current / recently completed**
- Separate `hive-summarizer` executable + client integration (done)
- `hive summarize` subcommand + passthrough test mode (done)
- Optional LLM refinement in processors via `HIVE_LLM_SUMMARIES` (done)
- Build reliability work for the Candle stack (pins + HF_ENDPOINT sanitization) (done)

**Short term / next logical steps**
- Deeper / more automatic use of LLM summaries (better transcript extraction, chunking for long sessions, caching of refined summaries)
- More agents (Claude, Aider, Cursor, etc.)
- Polish around process lifetime in `--watch` mode (keep summarizer warm)
- Better distribution story (e.g. optional "full" install that includes the summarizer, or clear docs)

**Medium / longer term**
- Configuration (not just env vars)
- Persistence / caching of records or refined summaries
- SwiftUI macOS companion (read-only view)
- More powerful local models or backends behind the same `hive-summarizer` interface

## How to Continue

1. Start a new chat while your shell/editor is inside `/Users/misko/work/hive`.
2. Paste or reference this `MEMORY.md` at the beginning of the new conversation.
3. The real source code on disk (especially under `crates/`) is the source of truth.

When working on the summarizer path, remember:
- Changes to model loading / generation go in `crates/hive-summarizer/`.
- Changes to discovery, spawning, or CLI surface go in `crates/hive/`.
- Use `HIVE_SUMMARIZER=...` (or the dev flag) when testing the integrated flow locally.

All code changes are expected to follow the existing commit message convention (short summary of the prompts/changes since the last commit).

---

*Updated to reflect the Cargo workspace, separate `hive-summarizer` executable architecture, Codex support, `ui.rs`, `summarizer_client.rs`, LLM refinement integration, and related environment variables / build notes.*