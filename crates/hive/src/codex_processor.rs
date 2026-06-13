use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use crate::agent_record::{AgentRecord, AgentSource, AgentStatus};

/// Parse all Codex rollout sessions found under the given base directory.
///
/// This recursively walks for files named "rollout-*.jsonl" (Codex's typical session format).
/// It produces one AgentRecord per valid rollout file.
///
/// Sessions are parsed from the JSONL events to extract:
/// - cwd from task_started or similar payloads (fallback to file parent or current dir)
/// - summary from first user_message
/// - id from filename
/// - last_generated_msg from file mtime
/// - status inferred from last speaker (user/agent) and pending tool calls (function_call without matching output)
///
/// Records are returned sorted by last_generated_msg descending.
pub fn parse_codex_sessions(base_dir: &Path) -> Result<Vec<AgentRecord>> {
    let mut records: Vec<AgentRecord> = Vec::new();

    if !base_dir.exists() {
        return Ok(records);
    }

    collect_rollout_files(base_dir, &mut records)?;

    records.sort_by(|a, b| b.last_generated_msg.cmp(&a.last_generated_msg));
    Ok(records)
}

fn collect_rollout_files(dir: &Path, records: &mut Vec<AgentRecord>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading codex dir {:?}", dir))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_rollout_files(&path, records)?;
        } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                match parse_rollout_file(&path) {
                    Ok(record) => records.push(record),
                    Err(err) => {
                        eprintln!("Warning: skipping Codex rollout {:?}: {}", path, err);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Try to parse a timestamp from a Codex rollout filename like
/// rollout-2026-04-01T20-21-23-019d4c35-....jsonl
/// This is often more reliable than file mtime.
fn parse_rollout_timestamp_from_filename(path: &Path) -> Option<DateTime<Utc>> {
    let name = path.file_stem()?.to_str()?;
    // After "rollout-" comes YYYY-MM-DDTHH-MM-SS-...
    let rest = name.strip_prefix("rollout-")?;
    if rest.len() < 19 {
        return None;
    }
    let time_part = &rest[..19]; // "2026-04-01T20-21-23"
                                 // Replace the two '-' in the time portion with ':'
    if let Some(t_pos) = time_part.find('T') {
        let date = &time_part[..=t_pos];
        let time = &time_part[t_pos + 1..];
        let time_fixed = time.replace('-', ":");
        let iso = format!("{}Z", date.to_string() + &time_fixed);
        if let Ok(dt) = DateTime::parse_from_rfc3339(&iso) {
            return Some(dt.with_timezone(&Utc));
        }
        if let Ok(dt) = DateTime::parse_from_str(&iso, "%Y-%m-%dT%H:%M:%SZ") {
            return Some(dt.with_timezone(&Utc));
        }
    }
    None
}

fn parse_rollout_file(path: &Path) -> Result<AgentRecord> {
    let content =
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;

    let mut max_ts: Option<DateTime<Utc>> = parse_rollout_timestamp_from_filename(path);
    if let Ok(meta) = fs::metadata(path) {
        if let Ok(mtime) = meta.modified() {
            let mtime_dt = DateTime::<Utc>::from(mtime);
            if max_ts.map_or(true, |t| mtime_dt > t) {
                max_ts = Some(mtime_dt);
            }
        }
    }

    let mut id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown-rollout")
        .to_string();

    let mut cwd: Option<String> = None;
    let mut summary: Option<String> = None;
    let mut last_was_user = false;
    let mut pending_tools = 0i32;
    let mut active_turns: HashSet<String> = HashSet::new();
    let mut anonymous_active_turns = 0i32;

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
            // Collect max timestamp from every line for better last_generated_msg
            if let Some(ts_str) = val.get("timestamp").and_then(|v| v.as_str()) {
                if let Ok(dt) = DateTime::parse_from_rfc3339(ts_str) {
                    let dt_utc = dt.with_timezone(&Utc);
                    if max_ts.map_or(true, |t| dt_utc > t) {
                        max_ts = Some(dt_utc);
                    }
                }
            }

            if let Some(typ) = val.get("type").and_then(|v| v.as_str()) {
                match typ {
                    "session_meta" => {
                        // Best source for id, cwd, start time (from the real example)
                        if let Some(payload) = val.get("payload") {
                            if let Some(meta_id) = payload.get("id").and_then(|v| v.as_str()) {
                                id = meta_id.to_string();
                            }
                            if cwd.is_none() {
                                if let Some(c) = payload.get("cwd").and_then(|v| v.as_str()) {
                                    cwd = Some(c.to_string());
                                }
                            }
                        }
                    }
                    "event_msg" => {
                        if let Some(payload) = val.get("payload") {
                            if let Some(pt) = payload.get("type").and_then(|v| v.as_str()) {
                                match pt {
                                    "user_message" => {
                                        last_was_user = true;
                                        if summary.is_none() {
                                            if let Some(msg) =
                                                payload.get("message").and_then(|v| v.as_str())
                                            {
                                                summary =
                                                    Some(msg.chars().take(120).collect::<String>());
                                            }
                                        }
                                    }
                                    "agent_message" => {
                                        last_was_user = false;
                                    }
                                    "task_started" => {
                                        if let Some(turn_id) =
                                            payload.get("turn_id").and_then(|v| v.as_str())
                                        {
                                            active_turns.insert(turn_id.to_string());
                                        } else {
                                            anonymous_active_turns += 1;
                                        }

                                        if cwd.is_none() {
                                            if let Some(c) = payload
                                                .get("cwd")
                                                .and_then(|v| v.as_str())
                                                .or_else(|| {
                                                    payload
                                                        .get("working_dir")
                                                        .and_then(|v| v.as_str())
                                                })
                                            {
                                                cwd = Some(c.to_string());
                                            }
                                        }
                                    }
                                    "task_complete" | "turn_aborted" => {
                                        if let Some(turn_id) =
                                            payload.get("turn_id").and_then(|v| v.as_str())
                                        {
                                            active_turns.remove(turn_id);
                                        } else if anonymous_active_turns > 0 {
                                            anonymous_active_turns -= 1;
                                        }
                                        last_was_user = false;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    "response_item" => {
                        if let Some(payload) = val.get("payload") {
                            if let Some(pt) = payload.get("type").and_then(|v| v.as_str()) {
                                if pt == "function_call" || pt == "custom_tool_call" {
                                    pending_tools += 1;
                                } else if pt == "function_call_output"
                                    || pt == "custom_tool_call_output"
                                {
                                    if pending_tools > 0 {
                                        pending_tools -= 1;
                                    }
                                    last_was_user = false;
                                } else if pt == "message" {
                                    match payload.get("role").and_then(|v| v.as_str()) {
                                        Some("user") => last_was_user = true,
                                        Some("assistant") => last_was_user = false,
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let last_generated_msg = max_ts.unwrap_or_else(Utc::now);

    let working_dir = cwd
        .map(PathBuf::from)
        .or_else(|| path.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    let mut summary_text = summary.unwrap_or_else(|| "Untitled Codex session".to_string());

    // Optional LLM refinement (same as Grok) when HIVE_LLM_SUMMARIES=1 and native
    // summary is weak. For a fuller transcript we would accumulate more content
    // from the rollout events while scanning.
    if std::env::var("HIVE_LLM_SUMMARIES").is_ok() && summary_text.len() < 80 {
        if let Ok(better) = crate::summarizer_client::summarize_via_external(&summary_text) {
            if !better.trim().is_empty() {
                summary_text = better;
            }
        }
    }

    // Prefer clean id from session_meta if we saw one; for now we keep filename-based id
    // (the meta id is the same as the uuid in the filename in the example).
    // If we want, we can parse the first session_meta for id, but filename is unique and fine.

    let has_active_turn = !active_turns.is_empty() || anonymous_active_turns > 0;
    let status = if has_active_turn || last_was_user || pending_tools > 0 {
        AgentStatus::Thinking
    } else {
        AgentStatus::Waiting
    };

    Ok(AgentRecord::new(
        id,
        summary_text,
        status,
        last_generated_msg,
        working_dir,
        AgentSource::Codex,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_mock_codex_rollout(
        base: &Path,
        filename: &str,
        cwd: &str,
        has_user_last: bool,
        has_pending_tool: bool,
    ) -> PathBuf {
        let file = base.join(filename);
        let mut content = String::new();

        // task started with cwd
        content.push_str(&format!(
            r#"{{"type":"event_msg","payload":{{"type":"task_started","turn_id":"turn-{}","cwd":"{}"}}}}"#,
            filename,
            cwd
        ));
        content.push('\n');

        // initial user
        content.push_str(r#"{"type":"event_msg","payload":{"type":"user_message","message":"Fix the foo function"}}"#);
        content.push('\n');

        if has_pending_tool {
            content.push_str(r#"{"type":"response_item","payload":{"type":"function_call","name":"read_file","arguments":"{}"}}"#);
            content.push('\n');
            // no output yet
        } else {
            content.push_str(r#"{"type":"response_item","payload":{"type":"message","content":[{"type":"text","text":"Fixed it."}]}}}"#);
            content.push('\n');
        }

        if has_user_last {
            content.push_str(
                r#"{"type":"event_msg","payload":{"type":"user_message","message":"Now do more"}}"#,
            );
            content.push('\n');
        } else if !has_pending_tool {
            content.push_str(
                r#"{"type":"event_msg","payload":{"type":"agent_message","message":"Done."}}"#,
            );
            content.push('\n');
            content.push_str(&format!(
                r#"{{"type":"event_msg","payload":{{"type":"task_complete","turn_id":"turn-{}"}}}}"#,
                filename
            ));
            content.push('\n');
        }

        fs::write(&file, content).expect("write mock rollout");
        file
    }

    #[test]
    fn parses_codex_rollouts_and_infers_status() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();

        create_mock_codex_rollout(
            base,
            "rollout-1.jsonl",
            "/Users/misko/work/project1",
            true,
            false,
        ); // user last → Thinking
        create_mock_codex_rollout(
            base,
            "rollout-2.jsonl",
            "/Users/misko/work/project2",
            false,
            true,
        ); // pending tool → Thinking
        create_mock_codex_rollout(
            base,
            "rollout-3.jsonl",
            "/Users/misko/work/project3",
            false,
            false,
        ); // completed → Waiting

        let records = parse_codex_sessions(base).unwrap();
        assert_eq!(records.len(), 3);

        // sorted newest first by mtime, but since created in order, and mtime may be same, we don't assert order strictly but status
        let has_thinking = records
            .iter()
            .any(|r| r.status == AgentStatus::Thinking && r.source == AgentSource::Codex);
        let has_waiting = records
            .iter()
            .any(|r| r.status == AgentStatus::Waiting && r.source == AgentSource::Codex);
        assert!(has_thinking);
        assert!(has_waiting);
    }

    #[test]
    fn open_codex_turn_is_thinking_without_pending_tools() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();
        let file = base.join("rollout-running.jsonl");
        fs::write(
            &file,
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"running-turn","cwd":"/Users/misko/work/hive"}}"#
                .to_string()
                + "\n"
                + r#"{"type":"event_msg","payload":{"type":"user_message","message":"Investigate status"}}"#
                + "\n"
                + r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"I am looking now."}]}}"#
                + "\n",
        )
        .expect("write running rollout");

        let records = parse_codex_sessions(base).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, AgentStatus::Thinking);
    }

    #[test]
    fn handles_nonexistent_codex_dir() {
        let records = parse_codex_sessions(Path::new("/no/such/codex/sessions")).unwrap();
        assert!(records.is_empty());
    }
}
