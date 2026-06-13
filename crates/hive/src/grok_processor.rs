use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

use crate::agent_record::{AgentRecord, AgentSource, AgentStatus};

/// Deserializable subset of Grok's summary.json.
/// We only pull the fields we need for AgentRecord construction.
#[derive(Debug, Deserialize)]
struct SummaryInfo {
    id: String,
    cwd: String,
}

#[derive(Debug, Deserialize)]
struct GrokSummary {
    info: SummaryInfo,
    #[serde(default)]
    session_summary: Option<String>,
    #[serde(default)]
    generated_title: Option<String>,
    #[serde(default)]
    last_active_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
}

/// Minimal record for inferring activity from chat_history.jsonl.
/// We only care about the top-level type and whether an assistant had tool calls
/// or reasoning (signals of active generation / tool use).
#[derive(Debug, Deserialize, Default)]
struct ChatRecord {
    #[serde(rename = "type")]
    r#type: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    reasoning: Option<serde_json::Value>,
    #[serde(default)]
    tool_calls: Option<Vec<serde_json::Value>>,
    // content and other fields are ignored for status inference
}

/// Parse all Grok sessions found under the given base directory
/// (normally `~/.grok/sessions`).
///
/// This walks the two-level layout used by Grok:
///   base/<encoded-cwd>/<session-uuid>/summary.json
///
/// It produces one AgentRecord per valid session directory.
/// Sessions without a readable summary.json are skipped (with a warning to stderr).
///
/// Records are returned sorted by `last_generated_msg` descending (newest first).
///
/// The "currently active" aspect (AgentStatus) is derived from the structure of
/// chat_history.jsonl when available:
/// - Recent user message with no assistant reply yet, or
/// - Assistant record with pending/unresolved tool_calls
///   → Thinking (actively generating or tool-using).
/// Otherwise (last turn was a completed assistant response) → Waiting.
/// Falls back to a time-based heuristic only when chat_history.jsonl is absent
/// or unreadable.
pub fn parse_grok_sessions(base_dir: &Path) -> Result<Vec<AgentRecord>> {
    parse_grok_sessions_with_filter(base_dir, None)
}

fn parse_grok_sessions_with_filter(
    base_dir: &Path,
    cwd_filter: Option<&Path>,
) -> Result<Vec<AgentRecord>> {
    let mut records: Vec<AgentRecord> = Vec::new();

    if !base_dir.exists() {
        // Common case when Grok has never run on this machine, or in tests.
        return Ok(records);
    }

    let read_dir_err = |p: &Path| format!("reading directory {:?}", p);

    for top_entry in fs::read_dir(base_dir).context("reading Grok sessions root")? {
        let top_entry = top_entry?;
        let top_path = top_entry.path();
        if !top_path.is_dir() {
            continue;
        }

        for sess_entry in fs::read_dir(&top_path).with_context(|| read_dir_err(&top_path))? {
            let sess_entry = sess_entry?;
            let session_dir = sess_entry.path();
            if !session_dir.is_dir() {
                continue;
            }

            let summary_path = session_dir.join("summary.json");
            if !summary_path.exists() {
                continue;
            }

            match parse_single_session(&session_dir, cwd_filter) {
                Ok(Some(record)) => records.push(record),
                Ok(None) => {}
                Err(err) => {
                    // Non-fatal: one bad session should not kill the whole scan.
                    eprintln!("Warning: skipping Grok session {:?}: {}", session_dir, err);
                }
            }
        }
    }

    // Most recent activity first. This makes the "list of currently active" natural
    // when the caller displays the first N or filters further by time.
    records.sort_by(|a, b| b.last_generated_msg.cmp(&a.last_generated_msg));

    Ok(records)
}

/// Variant that returns only sessions whose working_dir matches (or is under) the given cwd.
/// Useful for project-scoped queries (e.g. `hive list` while inside a repo).
#[allow(dead_code)]
pub fn parse_grok_sessions_for_cwd(base_dir: &Path, cwd: &Path) -> Result<Vec<AgentRecord>> {
    parse_grok_sessions_with_filter(base_dir, Some(cwd))
}

fn parse_single_session(
    session_dir: &Path,
    cwd_filter: Option<&Path>,
) -> Result<Option<AgentRecord>> {
    let summary_path = session_dir.join("summary.json");
    let content = fs::read_to_string(&summary_path)
        .with_context(|| format!("reading {}", summary_path.display()))?;

    let summary: GrokSummary = serde_json::from_str(&content)
        .with_context(|| format!("deserializing {}", summary_path.display()))?;

    let id = summary.info.id.clone();
    let working_dir = PathBuf::from(&summary.info.cwd);
    if let Some(filter) = cwd_filter {
        if working_dir != filter && !working_dir.starts_with(filter) {
            return Ok(None);
        }
    }

    // Prefer a real session summary; fall back to the generated title (often shorter),
    // then a generic placeholder.
    let summary_text = summary
        .session_summary
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .or(summary.generated_title.as_deref())
        .unwrap_or("Untitled Grok session")
        .to_string();
    let user_text = extract_user_text(&session_dir.join("chat_history.jsonl"));

    // Timestamp priority: last_active_at is the most meaningful for "last generated msg".
    let ts_str = summary
        .last_active_at
        .or(summary.updated_at)
        .or(summary.created_at)
        .unwrap_or_else(|| Utc::now().to_rfc3339());

    let last_generated_msg = parse_grok_timestamp(&ts_str).unwrap_or_else(|_| Utc::now());

    // Prefer structural signals from chat_history.jsonl (pending tool calls,
    // last speaker being the user or an assistant mid-action).
    // This matches how real Grok (and Codex) sessions indicate active work.
    let status = infer_status_from_chat_history(&session_dir.join("chat_history.jsonl"))
        .unwrap_or_else(|| {
            // Fallback (kept for sessions without chat_history or for very old tests).
            const THINKING_WINDOW: Duration = Duration::minutes(10);
            if last_generated_msg > (Utc::now() - THINKING_WINDOW) {
                AgentStatus::Thinking
            } else {
                AgentStatus::Waiting
            }
        });

    Ok(Some(
        AgentRecord::new(
            id,
            summary_text,
            status,
            last_generated_msg,
            working_dir,
            AgentSource::Grok,
        )
        .with_user_text(user_text),
    ))
}

/// Parse the various timestamp strings Grok writes (RFC3339 with or without micros, Z, etc.).
fn parse_grok_timestamp(s: &str) -> Result<DateTime<Utc>> {
    // Primary format used in the real files we inspected.
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }

    // Common variant with fractional seconds explicitly.
    if let Ok(dt) = DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.fZ") {
        return Ok(dt.with_timezone(&Utc));
    }

    // Another variant some logs use (space instead of T).
    if let Ok(dt) = DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f +00:00") {
        return Ok(dt.with_timezone(&Utc));
    }

    anyhow::bail!("unrecognized Grok timestamp format: {}", s)
}

fn extract_user_text(chat_path: &Path) -> Option<String> {
    if !chat_path.exists() {
        return None;
    }
    let content = fs::read_to_string(chat_path).ok()?;
    let mut parts: Vec<String> = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
            if val.get("type").and_then(|v| v.as_str()) != Some("user") {
                continue;
            }
            // Common shapes in Grok chat_history:
            // - "content": "plain string"
            // - "content": [ {"type": "text", "text": "..."}, ... ]
            // - sometimes top-level "text" or "reasoning"
            if let Some(c) = val.get("content") {
                if let Some(s) = c.as_str() {
                    push_user_text_part(&mut parts, s);
                } else if let Some(arr) = c.as_array() {
                    for item in arr {
                        if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                            push_user_text_part(&mut parts, t);
                        }
                    }
                }
            }
            if let Some(t) = val.get("text").and_then(|v| v.as_str()) {
                push_user_text_part(&mut parts, t);
            }
        }
        if parts.len() > 20 {
            break; // bound the number of turns
        }
    }
    if parts.is_empty() {
        return None;
    }
    let joined = parts.join("\n\n");
    Some(joined)
}

fn push_user_text_part(parts: &mut Vec<String>, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.starts_with("<user_info>") {
        return;
    }
    parts.push(trimmed.to_string());
}

/// Infer AgentStatus from the structure of chat_history.jsonl (preferred signal).
///
/// Based on analysis of real Grok chat histories:
/// - "user" and "assistant" records form the conversation turns.
/// - An assistant record with `tool_calls` whose results have not all appeared yet
///   (i.e. fewer "tool_result" records after it) means the agent is actively tool-using.
/// - If the most recent speaker (last "user" or "assistant" record) is the user,
///   the agent is expected to reply → Thinking.
/// - If the most recent speaker is an assistant with no pending tool activity,
///   the agent has finished its turn and is waiting for the user → Waiting.
fn infer_status_from_chat_history(chat_path: &Path) -> Option<AgentStatus> {
    if !chat_path.exists() {
        return None;
    }

    let content = fs::read_to_string(chat_path).ok()?;
    let mut records: Vec<ChatRecord> = Vec::new();
    for line in content.lines() {
        if let Ok(rec) = serde_json::from_str::<ChatRecord>(line) {
            if rec.r#type.is_some() {
                records.push(rec);
            }
        }
    }
    if records.is_empty() {
        return None;
    }

    // Find the last "speaker" (user or assistant), ignoring tool_result / system / text etc.
    // This is the primary signal.
    let last_speaker = records
        .iter()
        .rev()
        .find_map(|r| match r.r#type.as_deref() {
            Some("user") => Some("user"),
            Some("assistant") => Some("assistant"),
            _ => None,
        });

    match last_speaker {
        Some("user") => {
            // Recent user prompt with no assistant reply yet → agent should be generating / tool-using.
            Some(AgentStatus::Thinking)
        }
        Some("assistant") => {
            // Only for assistant do we look for pending tool activity after it.
            let last_assistant_idx = records
                .iter()
                .rposition(|r| r.r#type.as_deref() == Some("assistant"))
                .unwrap();
            let tool_results_after = records[last_assistant_idx + 1..]
                .iter()
                .filter(|r| r.r#type.as_deref() == Some("tool_result"))
                .count();

            let num_tool_calls = records[last_assistant_idx]
                .tool_calls
                .as_ref()
                .map(|v| v.len())
                .unwrap_or(0);

            if num_tool_calls > tool_results_after {
                // Agent emitted tool calls whose results have not all been observed yet.
                Some(AgentStatus::Thinking)
            } else {
                Some(AgentStatus::Waiting)
            }
        }
        _ => Some(AgentStatus::Waiting),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Helper that creates a realistic simulated Grok session tree under `base`.
    /// This is exactly the kind of fixture the user asked for:
    ///   tests can build any directory structure and pass the root to the parser.
    ///
    /// `last_speaker` controls the structural AgentStatus inference from chat_history:
    ///   "user"    → last speaker is user (agent expected to reply) → Thinking
    ///   "assistant" → last speaker is assistant with no pending tools → Waiting
    fn create_mock_grok_session(
        base: &Path,
        encoded_cwd: &str,
        session_id: &str,
        cwd: &str,
        session_summary: Option<&str>,
        generated_title: Option<&str>,
        last_active_at: &str,
        last_speaker: &str, // "user" or "assistant" (drives structural status)
        _source: AgentSource,
    ) -> PathBuf {
        let session_dir = base.join(encoded_cwd).join(session_id);
        fs::create_dir_all(&session_dir).expect("create session dir");

        // Build a minimal but realistic summary.json matching what we see on disk.
        let mut json = format!(
            r#"{{
    "info": {{
        "id": "{}",
        "cwd": "{}"
    }},
    "last_active_at": "{}",
    "updated_at": "{}"
"#,
            session_id, cwd, last_active_at, last_active_at
        );

        if let Some(s) = session_summary {
            json.push_str(&format!(
                r#",
    "session_summary": "{}""#,
                s
            ));
        }
        if let Some(t) = generated_title {
            json.push_str(&format!(
                r#",
    "generated_title": "{}""#,
                t
            ));
        }

        json.push_str("\n}");

        fs::write(session_dir.join("summary.json"), json).expect("write summary.json");

        // Also drop a tiny events.jsonl so future richer status logic has something to read.
        let events = format!(
            "{{\"ts\":\"{}\",\"type\":\"turn_started\",\"session_id\":\"{}\"}}\n",
            last_active_at, session_id
        );
        fs::write(session_dir.join("events.jsonl"), events).ok();

        // Write a minimal chat_history.jsonl so the *structural* status inference
        // (based on last speaker + pending tool calls) can be exercised by tests.
        let chat = if last_speaker == "user" {
            // Recent user input with no assistant reply yet → Thinking
            "{\"type\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"Please continue\"}]}\n"
                .to_string()
        } else {
            // Last action was a completed assistant turn (no pending tools) → Waiting
            "{\"type\":\"assistant\",\"content\":\"Done.\",\"tool_calls\":[]}\n".to_string()
        };
        fs::write(session_dir.join("chat_history.jsonl"), chat).ok();

        session_dir
    }

    #[test]
    fn parses_single_session_from_simulated_directory_structure() {
        let _env_guard = env_lock();

        let tmp = TempDir::new().unwrap();
        let base = tmp.path();

        let recent_ts = (Utc::now() - Duration::minutes(2)).to_rfc3339();

        create_mock_grok_session(
            base,
            "%2FUsers%2Fmisko%2Fwork%2FHelloRust",
            "019ea450-f4f1-7582-a9ee-7160ed4f9e71",
            "/Users/misko/work/HelloRust",
            Some("Initialize Git Repository in Local Directory and explore worktrees"),
            Some("Hive: Main"),
            &recent_ts,
            "user", // last speaker = user → structural Thinking
            AgentSource::Grok,
        );

        let records = parse_grok_sessions(base).unwrap();

        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.id, "019ea450-f4f1-7582-a9ee-7160ed4f9e71");
        assert_eq!(r.working_dir, PathBuf::from("/Users/misko/work/HelloRust"));
        assert_eq!(
            r.summary,
            "Initialize Git Repository in Local Directory and explore worktrees"
        );
        assert_eq!(r.user_text.as_deref(), Some("Please continue"));
        // 2 minutes ago is well inside the 10-minute Thinking window.
        assert_eq!(r.status, AgentStatus::Thinking);
    }

    #[test]
    fn returns_multiple_sessions_sorted_newest_first_and_infers_waiting() {
        let _env_guard = env_lock();

        let tmp = TempDir::new().unwrap();
        let base = tmp.path();

        let now = Utc::now();
        let very_recent = (now - Duration::seconds(30)).to_rfc3339();
        let old = (now - Duration::hours(3)).to_rfc3339();

        // Two different encoded-cwd groups (different original projects)
        create_mock_grok_session(
            base,
            "%2FUsers%2Fmisko%2Fwork%2FHelloRust",
            "sess-recent",
            "/Users/misko/work/HelloRust",
            Some("Add grok processor and simulated dir tests"),
            None,
            &very_recent,
            "user", // last speaker = user → structural Thinking
            AgentSource::Grok,
        );

        create_mock_grok_session(
            base,
            "%2FUsers%2Fmisko%2Fother-project",
            "sess-old",
            "/Users/misko/work/other-project",
            Some("Refactor the legacy module"),
            None,
            &old,
            "assistant", // last speaker = assistant, no pending tools → Waiting
            AgentSource::Grok,
        );

        let records = parse_grok_sessions(base).unwrap();

        assert_eq!(records.len(), 2);
        // Newest first
        assert_eq!(records[0].id, "sess-recent");
        assert_eq!(records[0].status, AgentStatus::Thinking);
        assert_eq!(
            records[0].working_dir,
            PathBuf::from("/Users/misko/work/HelloRust")
        );

        assert_eq!(records[1].id, "sess-old");
        assert_eq!(records[1].status, AgentStatus::Waiting);
        assert_eq!(
            records[1].working_dir,
            PathBuf::from("/Users/misko/work/other-project")
        );
    }

    #[test]
    fn falls_back_to_generated_title_and_skips_when_no_summary_file() {
        let _env_guard = env_lock();

        let tmp = TempDir::new().unwrap();
        let base = tmp.path();

        // Session that only has generated_title
        create_mock_grok_session(
            base,
            "enc1",
            "only-title",
            "/p1",
            None,
            Some("Important work happening here"),
            "2026-06-01T10:00:00Z",
            "assistant",
            AgentSource::Grok,
        );

        // A directory that looks like a session but has no summary.json (should be ignored)
        let incomplete = base.join("enc1").join("incomplete");
        fs::create_dir_all(&incomplete).unwrap();
        // no summary.json

        let records = parse_grok_sessions(base).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].summary, "Important work happening here");
    }

    #[test]
    fn skips_malformed_json_without_panicking() {
        let _env_guard = env_lock();

        let tmp = TempDir::new().unwrap();
        let base = tmp.path();

        let bad_dir = base.join("enc").join("bad");
        fs::create_dir_all(&bad_dir).unwrap();
        fs::write(
            bad_dir.join("summary.json"),
            "this is not { valid json at all",
        )
        .unwrap();

        // Also a good one so we can prove we still parse the valid ones
        let good_ts = (Utc::now() - Duration::minutes(1)).to_rfc3339();
        create_mock_grok_session(
            base,
            "enc",
            "good",
            "/good",
            Some("Good summary"),
            None,
            &good_ts,
            "assistant",
            AgentSource::Grok,
        );

        let records = parse_grok_sessions(base).unwrap();
        // Only the good one made it through
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, "good");
    }

    #[test]
    fn handles_nonexistent_base_dir_gracefully() {
        let _env_guard = env_lock();

        let records =
            parse_grok_sessions(Path::new("/definitely/not/a/real/grok/sessions/dir")).unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn parse_for_cwd_filters_correctly() {
        let _env_guard = env_lock();

        let tmp = TempDir::new().unwrap();
        let base = tmp.path();

        let ts = (Utc::now() - Duration::minutes(1)).to_rfc3339();
        create_mock_grok_session(
            base,
            "e1",
            "s1",
            "/project/a",
            Some("In a"),
            None,
            &ts,
            "assistant",
            AgentSource::Grok,
        );
        create_mock_grok_session(
            base,
            "e2",
            "s2",
            "/project/b/sub",
            Some("In b/sub"),
            None,
            &ts,
            "assistant",
            AgentSource::Grok,
        );
        create_mock_grok_session(
            base,
            "e3",
            "s3",
            "/completely/other",
            Some("Other"),
            None,
            &ts,
            "assistant",
            AgentSource::Grok,
        );

        let filtered = parse_grok_sessions_for_cwd(base, Path::new("/project")).unwrap();
        assert_eq!(filtered.len(), 2); // a and b/sub match starts_with

        let exact = parse_grok_sessions_for_cwd(base, Path::new("/project/a")).unwrap();
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].id, "s1");
    }

    #[test]
    fn extracts_user_text_without_replacing_native_summary() {
        let _env_guard = env_lock();
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();

        let ts = (Utc::now() - Duration::minutes(2)).to_rfc3339();

        create_mock_grok_session(
            base,
            "encoded-llm",
            "llm-session-123",
            "/project/llm-test",
            Some("short"),
            None,
            &ts,
            "user",
            AgentSource::Grok,
        );

        let records = parse_grok_sessions(base).unwrap();

        let rec = records
            .iter()
            .find(|r| r.id == "llm-session-123")
            .expect("session should be present");
        assert_eq!(rec.summary, "short");
        assert_eq!(rec.user_text.as_deref(), Some("Please continue"));
    }
}
