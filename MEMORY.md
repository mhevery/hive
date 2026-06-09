# MEMORY.md — Hive Project Context

This file serves as a portable handoff / memory for continuing work on the Hive project in a fresh chat or session. It captures the current state, key decisions, implementation details, and next steps.

## Project Overview

**Hive** is a Rust CLI tool (designed as a weekend learning project, ~1 month scope) for observing and managing multiple AI coding agents.

**Core Problem it solves:**
> "It is hard to get an overview which agent is working on what in which directory what goal that directory is in."

Instead of intercepting PTY output or acting as a proxy, Hive reads the on-disk session data that agents already write:
- Grok Build: `~/.grok/sessions/<encoded-cwd>/<session-id>/` (summary.json, chat_history.jsonl, events.jsonl, etc.)
- Future targets: Codex, Claude, Aider, Cursor, etc.

**Current focus:** CLI-first. A SwiftUI macOS companion is possible later but not required.

**Tech stack:**
- Rust (edition 2021)
- `clap` (derive) for CLI
- `chrono`, `serde`/`serde_json`, `anyhow`
- Testing: `tempfile` + simulated directory fixtures

## Current Implementation Status (as of latest work)

### Core Data Model
- `src/agent_record.rs`
  - `AgentRecord` struct:
    - `id: String`
    - `summary: String`
    - `status: AgentStatus`
    - `last_generated_msg: DateTime<Utc>`
    - `working_dir: PathBuf`
  - `AgentStatus` enum:
    - `Thinking` — Agent is actively generating / tool-using
    - `Waiting` — Agent is waiting for user input or idle after last response

### Grok Processor (the main working piece)
- `src/grok_processor.rs`
- Public API:
  - `parse_grok_sessions(base_dir: &Path) -> Result<Vec<AgentRecord>>`
  - `parse_grok_sessions_for_cwd(...)`
- Walks `~/.grok/sessions/<percent-encoded-cwd>/<session-uuid>/`
- Primary data from `summary.json`:
  - `info.id`, `info.cwd` → working_dir
  - `session_summary` or `generated_title` → summary
  - `last_active_at` / `updated_at` → timestamp
- **Status inference (structural, preferred over time):**
  - Reads `chat_history.jsonl`
  - Looks at sequence of `"type": "user"` / `"type": "assistant"` records
  - Rules (derived from real session analysis):
    - Last speaker is `"user"` (recent user prompt with no assistant reply yet) → **Thinking**
    - Last `"assistant"` record has `tool_calls` where the number of subsequent `tool_result` records is lower than the number of calls → **Thinking** (agent actively tool-using)
    - Last speaker is a completed `"assistant"` (final content, no pending tools) → **Waiting**
  - Falls back to simple 10-minute recency on `last_generated_msg` only when `chat_history.jsonl` is missing or unparseable.
- Also handles:
  - Multiple encoded-cwd groups
  - Graceful skipping of bad/malformed sessions
  - Sorting by last activity (newest first)
  - Optional filtering by cwd

### CLI
- `src/main.rs` (clap)
- Currently demonstrates by scanning real `~/.grok/sessions` and printing discovered records.
- Falls back to hardcoded demo data if scanning fails.

### Testing Strategy (important)
- Heavy use of `tempfile::TempDir`
- `create_mock_grok_session()` helper in tests builds realistic trees:
  - `summary.json`
  - `chat_history.jsonl` (with controlled last speaker / tool_calls to drive status)
  - Minimal `events.jsonl`
- Tests cover:
  - Happy path parsing + field extraction
  - Structural status inference (Thinking vs Waiting)
  - Title fallback, missing files, malformed JSON (graceful)
  - Multiple sessions / encoded dirs
  - `parse_grok_sessions_for_cwd` filtering
- Goal: hermetic, deterministic tests that do not depend on the developer's real `~/.grok` data (except for optional `#[ignore]` real-data smoke tests).

### Git / Process
- Commits follow the convention of including a short summary of the originating user prompt in the message.
- Work is done at `/Users/misko/work/hive`

## Key Technical Decisions & Insights

### Agent Data Formats (from direct file analysis)
- **Grok** (`chat_history.jsonl`):
  - Top-level `"type": "user" | "assistant" | "tool_result" | "system"`
  - Assistant records contain `reasoning`, `tool_calls`, and `content` (final visible text).
  - Very rich agent trace (ReAct-style).
- **Codex** (rollout JSONL):
  - `response_item` with `payload.role: "user" | "assistant"`
  - Parallel `event_msg` with `type: "user_message" | "agent_message"`
  - Turn lifecycle via `task_started` / `task_complete`.
- These analyses directly drove the improved (non-time-only) `AgentStatus` logic.

### Status (Thinking / Waiting) Philosophy
Prioritize structural signals from the agent's own transcript over wall-clock time:
- "Agent is actively generating/tool-using" vs "waiting for the human".
- Pending tool calls or "user spoke last" are strong signals that the agent session is the currently active one.

### Why Read-Only Observation?
Early ideas (PTY proxy, hotkey overlay, tmux-like) were deprioritized once it became clear that agents already persist rich metadata and transcripts on disk. Reading those is higher-leverage and less invasive.

## Environment / Workspace Notes

- The project lives at `/Users/misko/work/hive`.
- Some previous chat sessions were started while the active workspace was `/Users/misko/work/HelloRust`. This caused `!pwd` (and the shell context for `!` commands) to default to HelloRust.
- **Correct way to run commands in this chat/project:**
  - Always do `cd /Users/misko/work/hive && <command>` in the **same** `!` line.
  - Or use full absolute paths for everything.
- File tools (read, edit, grep, etc.) have been using absolute paths to `/Users/misko/work/hive/...` successfully.

When starting a **new chat**, do so after `cd /Users/misko/work/hive` (or from within the hive folder in your editor) so the workspace root is correct from the beginning.

## Current Files (high level)

- `Cargo.toml`
- `src/main.rs`
- `src/agent_record.rs`
- `src/grok_processor.rs` (the bulk of the logic + tests)
- `MEMORY.md` (this file)

No `src/lib.rs` yet (still bin-focused).

## Open Items / Roadmap (from planning)

From the original phased plan and later discussions:

**Short term / next logical steps:**
- `hive list` and other subcommands (proper clap structure)
- Support for at least one more agent (Codex processor, reusing the analysis we did)
- Use `events.jsonl` more deeply for phase/turn information
- Small local LLM summarizer (Ollama) for weak native summaries (Qwen2.5 0.5-1.5B or Phi-4-mini class recommended)

**Medium term:**
- TUI dashboard (`ratatui` + crossterm) — `hive watch`
- Worktree correlation (accurate "last used dir")
- Better "currently active" heuristics (time window + structural + possibly process presence)
- Error handling, logging, configuration

**Later / stretch:**
- Persistence / caching of records?
- SwiftUI companion app
- Support for Aider, Cursor, Claude, etc.

## How to Continue

1. Start a new chat while your shell/editor is inside `/Users/misko/work/hive`.
2. Paste or reference this `MEMORY.md` at the beginning of the new conversation.
3. The real source code on disk is the source of truth — the chat history is secondary.

All code changes made so far are already committed on the `main` branch of the hive repo (following the prompt-summary commit message style).

---

*This file was generated as a handoff when moving the long-running conversation to a fresh chat rooted in the correct directory.*