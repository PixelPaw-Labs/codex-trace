use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::compression::read_session_file;
use super::entry::{extract_session_id, RawEntry};
use super::toolcall::ToolKind;
use super::turn::{build_turns, CodexTurn, TokenInfo, TurnStatus};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitInfo {
    pub commit_hash: Option<String>,
    pub branch: Option<String>,
    pub repository_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexSession {
    pub id: String,
    pub timestamp: String,
    pub cwd: Option<String>,
    pub originator: Option<String>,
    pub cli_version: Option<String>,
    pub model_provider: Option<String>,
    pub git: Option<GitInfo>,
    pub instructions: Option<String>,
    pub turns: Vec<CodexTurn>,
    pub is_ongoing: bool,
    pub total_tokens: Option<TokenInfo>,
    pub thread_name: Option<String>,
    pub spawned_worker_ids: Vec<String>,
    pub path: String,
    pub ai_title: Option<String>,
    /// true when the session was started via `codex remote-control` (Codex v0.130.0+, PR #21424).
    /// Detected from originator == "remote-control" or source == "remote-control" in session_meta.
    pub is_headless: bool,
    /// true when the session contains spawn_agent calls whose metadata was suppressed.
    /// Codex v0.137.0 (PR #26114) changed hide_spawn_agent_metadata to default true, causing
    /// function_call_output for spawn_agent to be empty. When this is true, multi-agent
    /// subagent lineage is absent and users should set hide_spawn_agent_metadata = false.
    pub has_missing_spawn_metadata: bool,
    /// Codex v0.146.0 (PRs #34621, #35220): thread ID this rollout's paginated history
    /// inherits from, read from session_meta.payload.history_base.thread_id. Distinct from
    /// the per-turn `forked_from_thread_id` and compaction `lineage_id` fields — this one
    /// marks the whole rollout file as a continuation of another paginated thread's history.
    /// Null for legacy-history sessions or paginated threads with no inherited prefix.
    pub history_base_thread_id: Option<String>,
    /// Thread ID this session was forked from, read from `session_meta.payload.forked_from_id`
    /// — the app-server's `SessionConfiguredEvent.forked_from_id`, persisted verbatim into the
    /// rollout's `session_meta` line by `codex-rs/rollout/src/recorder.rs`. This field on
    /// `SessionMeta` predates Codex v0.148.0, but `codex exec fork` (new subcommand) and
    /// `codex exec resume` (Codex v0.148.0+, PR #37367, issue #238) are the first `codex exec`
    /// code paths to populate it — it was previously hardcoded to `None` in
    /// `codex-rs/exec/src/lib.rs`. Session-level signal, distinct from the per-turn
    /// `forked_from_thread_id` on `CodexTurn` (sourced from `task_started`).
    /// Null for non-forked sessions.
    pub forked_from_thread_id: Option<String>,
}

/// Parse a Codex JSONL session file into a CodexSession.
pub fn parse_session(path: &Path) -> Result<CodexSession, String> {
    let mut visited = HashSet::new();
    parse_session_inner(path, &mut visited)
}

fn parse_session_inner(
    path: &Path,
    visited: &mut HashSet<PathBuf>,
) -> Result<CodexSession, String> {
    let content = read_session_file(path)?;
    let canonical_path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !visited.insert(canonical_path.clone()) {
        return Err(format!(
            "recursive session reference detected: {}",
            path.display()
        ));
    }

    let entries: Vec<RawEntry> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(RawEntry::parse)
        .collect();

    let mut session = CodexSession {
        id: String::new(),
        timestamp: String::new(),
        cwd: None,
        originator: None,
        cli_version: None,
        model_provider: None,
        git: None,
        instructions: None,
        turns: Vec::new(),
        is_ongoing: false,
        total_tokens: None,
        thread_name: None,
        spawned_worker_ids: Vec::new(),
        path: path.to_string_lossy().to_string(),
        ai_title: None,
        is_headless: false,
        has_missing_spawn_metadata: false,
        history_base_thread_id: None,
        forked_from_thread_id: None,
    };

    // Parse session_meta from first matching entry
    for entry in &entries {
        match entry.entry_type.as_str() {
            "session_meta" => {
                parse_session_meta_new(&mut session, &entry.payload, &entry.raw);
                break;
            }
            "session_meta_root" => {
                parse_session_meta_root(&mut session, &entry.raw);
                break;
            }
            _ => {}
        }
    }

    // Check for explicit session_end marker (Codex v0.128.0+).
    // When present the session is definitively closed regardless of file freshness.
    let has_session_end = entries.iter().any(|e| e.entry_type == "session_end");

    // Build turns from remaining entries
    let mut turns = build_turns(&entries);

    // Extract thread_name from last thread_name_updated
    let thread_name = turns.iter().rev().find_map(|t| t.thread_name.clone());

    // Collect spawned_worker_ids from all turns
    let spawned_worker_ids: Vec<String> = turns
        .iter()
        .flat_map(|t| t.collab_spawns.iter().map(|s| s.new_session_id.clone()))
        .collect();

    // Determine total tokens from last turn's token info
    let total_tokens = turns.iter().rev().find_map(|t| t.total_tokens.clone());

    // Determine is_ongoing: last turn must be Ongoing AND file must have been
    // modified within 60 seconds (same threshold as source repo). Sessions older
    // than that have no live CLI writing to them — task_complete was simply missed
    // (crash, kill, older CLI that never emitted the event).
    // A session_end marker (v0.128.0+) overrides both heuristics: the session
    // is definitively closed even if the file is still fresh.
    let turn_ongoing = turns
        .last()
        .map(|t| t.status == super::turn::TurnStatus::Ongoing)
        .unwrap_or(false);
    let file_fresh = fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|mt| {
            SystemTime::now()
                .duration_since(mt)
                .map(|e| e.as_secs() <= 60)
                .unwrap_or(true)
        })
        .unwrap_or(true);
    let is_ongoing = !has_session_end && turn_ongoing && file_fresh;

    // If the file is stale (or session_end present) and the last turn never got
    // a completion event, mark it as Aborted so the UI doesn't show an ongoing
    // indicator.
    if turn_ongoing && (!file_fresh || has_session_end) {
        if let Some(last) = turns.last_mut() {
            last.status = TurnStatus::Aborted;
        }
    }

    // Codex v0.137.0 (PR #26114): hide_spawn_agent_metadata defaults to true.
    // When active, spawn_agent function_call_output is empty and collab_agent_spawn_end
    // events are suppressed, so no spawn metadata is recorded. Detect this by checking
    // for spawn_agent calls whose output was empty (status "unknown").
    let has_missing_spawn_metadata = turns.iter().any(|t| {
        t.tool_calls
            .iter()
            .any(|tc| tc.kind == ToolKind::SpawnAgent && tc.status == "unknown")
    });

    embed_worker_sessions(path, &mut turns, visited);

    session.turns = turns;
    session.thread_name = thread_name;
    session.spawned_worker_ids = spawned_worker_ids;
    session.total_tokens = total_tokens;
    session.is_ongoing = is_ongoing;
    session.has_missing_spawn_metadata = has_missing_spawn_metadata;

    visited.remove(&canonical_path);
    Ok(session)
}

fn embed_worker_sessions(
    parent_path: &Path,
    turns: &mut [CodexTurn],
    visited: &mut HashSet<PathBuf>,
) {
    for turn in turns {
        for tool in &mut turn.tool_calls {
            if tool.kind != ToolKind::SpawnAgent {
                continue;
            }

            let Some(spawn) = turn
                .collab_spawns
                .iter()
                .find(|spawn| spawn.call_id == tool.call_id)
            else {
                continue;
            };

            let Some(worker_path) = find_session_file_by_id(parent_path, &spawn.new_session_id)
            else {
                continue;
            };

            let canonical_worker_path =
                fs::canonicalize(&worker_path).unwrap_or_else(|_| worker_path.clone());
            if visited.contains(&canonical_worker_path) {
                continue;
            }

            if let Ok(worker_session) = parse_session_inner(&worker_path, visited) {
                tool.worker_session = Some(Box::new(worker_session));
            }
        }
    }
}

fn find_session_file_by_id(anchor_path: &Path, session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty() {
        return None;
    }

    let dir = anchor_path.parent()?;
    let mut candidates: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains(session_id))
        })
        .collect();

    candidates.sort();
    candidates
        .iter()
        .find(|path| session_file_id(path).as_deref() == Some(session_id))
        .cloned()
        .or_else(|| candidates.into_iter().next())
}

fn session_file_id(path: &Path) -> Option<String> {
    let content = read_session_file(path).ok()?;
    content.lines().take(20).find_map(|line| {
        let entry = RawEntry::parse(line)?;
        match entry.entry_type.as_str() {
            "session_meta" => {
                let id = extract_session_id(&entry.payload);
                if id.is_empty() {
                    None
                } else {
                    Some(id)
                }
            }
            "session_meta_root" => entry
                .raw
                .get("id")
                .and_then(|id| id.as_str())
                .map(|id| id.to_string()),
            _ => None,
        }
    })
}

fn parse_session_meta_new(session: &mut CodexSession, payload: &Value, _raw: &Value) {
    session.id = extract_session_id(payload);
    session.timestamp = str_field(payload, "timestamp");
    session.cwd = opt_str(payload, "cwd");
    session.originator = opt_str(payload, "originator");
    session.cli_version = opt_str(payload, "cli_version");
    session.model_provider = opt_str(payload, "model_provider");
    // ai-title is an optional field added in Codex v0.128.0 for external agent sessions
    session.ai_title = payload
        .get("ai-title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    // Codex v0.130.0 (PR #21424): `codex remote-control` starts headless app-server sessions.
    // Detected from originator == "remote-control" or source == "remote-control".
    session.is_headless = payload
        .get("originator")
        .and_then(|v| v.as_str())
        .map(|s| s == "remote-control")
        .unwrap_or(false)
        || payload
            .get("source")
            .and_then(|v| v.as_str())
            .map(|s| s == "remote-control")
            .unwrap_or(false);

    if let Some(git) = payload.get("git") {
        session.git = Some(GitInfo {
            commit_hash: opt_str(git, "commit_hash"),
            branch: opt_str(git, "branch"),
            repository_url: opt_str(git, "repository_url"),
        });
    }

    // Instructions: prefer base_instructions.text, fall back to instructions (flat string)
    session.instructions = payload
        .get("base_instructions")
        .and_then(|bi| bi.get("text"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| opt_str(payload, "instructions"));

    // Codex v0.146.0 (PRs #34621, #35220): a paginated rollout continuing another thread's
    // history carries session_meta.history_base.thread_id pointing at the source rollout.
    session.history_base_thread_id = payload
        .get("history_base")
        .and_then(|b| b.get("thread_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    // Codex v0.148.0 (PR #37367, issue #238): session_meta.forked_from_id is the
    // app-server's SessionConfiguredEvent.forked_from_id, persisted verbatim into the
    // rollout. Verified against codex-rs/rollout/src/recorder.rs and
    // codex-rs/protocol/src/protocol.rs's SessionMeta struct — real signal for any forked
    // or fork-descended thread, including codex exec fork/resume sessions.
    session.forked_from_thread_id = payload
        .get("forked_from_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
}

fn parse_session_meta_root(session: &mut CodexSession, raw: &Value) {
    session.id = str_field(raw, "id");
    session.timestamp = str_field(raw, "timestamp");
    // Oldest format: no cwd, originator, cli_version
    if let Some(git) = raw.get("git") {
        session.git = Some(GitInfo {
            commit_hash: opt_str(git, "commit_hash"),
            branch: opt_str(git, "branch"),
            repository_url: opt_str(git, "repository_url"),
        });
    }
    session.instructions = opt_str(raw, "instructions");
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn opt_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Returns the default Codex sessions directory: ~/.codex/sessions
pub fn default_sessions_dir() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".codex").join("sessions"))
}

/// Resolve the sessions directory from settings or default.
pub fn resolve_sessions_dir(configured: Option<&str>) -> Result<std::path::PathBuf, String> {
    if let Some(p) = configured.filter(|s| !s.is_empty()) {
        return Ok(std::path::PathBuf::from(p));
    }
    default_sessions_dir().ok_or_else(|| "cannot determine home directory".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn parse_session_reads_id_from_session_id_field() {
        // v0.129.0+ PR #20437: session_id field in session_meta payload
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-07T00-00-00-newsessid.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-07T00:00:00Z","type":"session_meta","payload":{"session_id":"new-sess-id","timestamp":"2026-05-07T00:00:00Z","cwd":"/tmp"}}"#,
                r#"{"timestamp":"2026-05-07T00:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-07T00:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746576002.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "new-sess-id");
    }

    #[test]
    fn parse_session_reads_id_from_thread_session_id() {
        // v0.129.0+ PR #21336: sessionId moved onto Thread object
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-07T00-01-00-threadsessid.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-07T00:01:00Z","type":"session_meta","payload":{"thread":{"sessionId":"thread-sess-id"},"timestamp":"2026-05-07T00:01:00Z","cwd":"/tmp"}}"#,
                r#"{"timestamp":"2026-05-07T00:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-07T00:01:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746576062.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "thread-sess-id");
    }

    #[test]
    fn default_sessions_dir_exists() {
        let dir = default_sessions_dir();
        assert!(dir.is_some());
    }

    fn find_first_jsonl(dir: &PathBuf) -> Option<PathBuf> {
        let rd = std::fs::read_dir(dir).ok()?;
        let mut children: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        children.sort();
        for child in &children {
            if child.is_file() && child.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                return Some(child.clone());
            }
            if child.is_dir() {
                if let Some(found) = find_first_jsonl(child) {
                    return Some(found);
                }
            }
        }
        None
    }

    #[test]
    fn parse_real_session_does_not_panic() {
        let home = std::env::var("HOME").expect("HOME not set");
        let sessions_root = PathBuf::from(home).join(".codex/sessions");
        if !sessions_root.exists() {
            return;
        }
        let Some(path) = find_first_jsonl(&sessions_root) else {
            return;
        };
        let result = parse_session(&path);
        assert!(result.is_ok(), "parse_session failed: {:?}", result.err());
        let session = result.unwrap();
        assert!(!session.id.is_empty(), "session id should not be empty");
    }

    #[test]
    fn parse_session_collects_sdk_spawn_agent_output_workers() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-04-27T16-50-45-parent.jsonl");
        let worker_path = tmp
            .path()
            .join("rollout-2026-04-27T16-50-46-worker-session.jsonl");
        let nested_worker_path = tmp
            .path()
            .join("rollout-2026-04-27T16-50-47-nested-worker-session.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-04-27T04:50:45Z","type":"session_meta","payload":{"id":"parent","timestamp":"2026-04-27T04:50:45Z"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:02Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"message\":\"Collect evidence\"}","call_id":"call_spawn"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_spawn","output":"{\"agent_id\":\"worker-session\",\"nickname\":\"Parfit\"}"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777279924.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::write(
            &worker_path,
            [
                r#"{"timestamp":"2026-04-27T04:50:46Z","type":"session_meta","payload":{"id":"worker-session","timestamp":"2026-04-27T04:50:46Z","cwd":"/tmp/worker"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:05Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-worker"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:06Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"echo worker\"}","call_id":"call_exec"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:07Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_exec","output":"worker output"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:08Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"message\":\"Go deeper\"}","call_id":"call_nested"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:09Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_nested","output":"{\"agent_id\":\"nested-worker-session\",\"nickname\":\"Nested\"}"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:10Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-worker","completed_at":1777279930.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::write(
            &nested_worker_path,
            [
                r#"{"timestamp":"2026-04-27T04:50:47Z","type":"session_meta","payload":{"id":"nested-worker-session","timestamp":"2026-04-27T04:50:47Z","cwd":"/tmp/nested"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:11Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-nested"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:12Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"echo nested\"}","call_id":"call_nested_exec"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:13Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_nested_exec","output":"nested output"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:14Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-nested","completed_at":1777279934.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();

        assert_eq!(session.spawned_worker_ids, vec!["worker-session"]);
        assert_eq!(
            session.turns[0].collab_spawns[0].new_session_id,
            "worker-session"
        );

        let worker_session = session.turns[0].tool_calls[0]
            .worker_session
            .as_ref()
            .expect("spawn_agent tool should embed worker session");
        assert_eq!(worker_session.id, "worker-session");
        assert_eq!(worker_session.turns[0].tool_calls[0].name, "exec_command");

        let nested_worker_session = worker_session.turns[0].tool_calls[1]
            .worker_session
            .as_ref()
            .expect("nested spawn_agent tool should embed nested worker session");
        assert_eq!(nested_worker_session.id, "nested-worker-session");
        assert_eq!(
            nested_worker_session.turns[0].tool_calls[0].output,
            Some("nested output".to_string())
        );
    }

    #[test]
    fn parse_session_reads_ai_title_from_session_meta() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-04-30T10-00-00-ext.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"ext-session","timestamp":"2026-04-30T10:00:00Z","ai-title":"Fix the login bug"}}"#,
                r#"{"timestamp":"2026-04-30T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-30T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746007202.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.ai_title.as_deref(), Some("Fix the login bug"));
        assert_eq!(session.id, "ext-session");
    }

    #[test]
    fn parse_session_end_marker_closes_session() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-04-30T10-01-00-ended.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-04-30T10:01:00Z","type":"session_meta","payload":{"id":"ended-session","timestamp":"2026-04-30T10:01:00Z"}}"#,
                r#"{"timestamp":"2026-04-30T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-30T10:01:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746007262.0}}"#,
                r#"{"timestamp":"2026-04-30T10:01:03Z","type":"session_end","payload":{}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert!(!session.is_ongoing);
    }

    #[test]
    fn parse_session_end_marker_overrides_ongoing_turn() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-04-30T10-02-00-endmarker.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-04-30T10:02:00Z","type":"session_meta","payload":{"id":"endmarker-session","timestamp":"2026-04-30T10:02:00Z"}}"#,
                r#"{"timestamp":"2026-04-30T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-30T10:02:02Z","type":"session_end","payload":{}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        // session_end overrides the ongoing turn — session must not appear live
        assert!(!session.is_ongoing);
    }

    // Codex v0.130.0 (PR #21424): `codex remote-control` starts headless app-server sessions.

    #[test]
    fn parse_session_detects_headless_via_originator() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-08T10-00-00-headless.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-08T10:00:00Z","type":"session_meta","payload":{"id":"headless-session","timestamp":"2026-05-08T10:00:00Z","originator":"remote-control","cli_version":"0.130.0"}}"#,
                r#"{"timestamp":"2026-05-08T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-08T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746698402.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let session = parse_session(&path).unwrap();
        assert!(
            session.is_headless,
            "originator:remote-control must set is_headless"
        );
        assert_eq!(session.id, "headless-session");
        assert_eq!(session.turns.len(), 1);
    }

    #[test]
    fn parse_session_detects_headless_via_source_string() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-08T10-01-00-headless2.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-08T10:01:00Z","type":"session_meta","payload":{"id":"headless-session-2","timestamp":"2026-05-08T10:01:00Z","source":"remote-control","cli_version":"0.130.0"}}"#,
                r#"{"timestamp":"2026-05-08T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-08T10:01:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746698462.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let session = parse_session(&path).unwrap();
        assert!(
            session.is_headless,
            "source:remote-control must set is_headless"
        );
    }

    // Codex v0.133.0 (PRs #23300, #23685, #23696, #23732): Goals feature enabled by default.
    // Goal lifecycle events are interleaved in the session JSONL turn stream. Verify full
    // session parse handles them gracefully and produces the correct turn structure.

    #[test]
    fn parse_session_with_goal_events_produces_correct_turns() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-05-21T10-00-00-goals.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-21T10:00:00Z","type":"session_meta","payload":{"id":"goals-session","timestamp":"2026-05-21T10:00:00Z","cwd":"/tmp","cli_version":"0.133.0"}}"#,
                r#"{"timestamp":"2026-05-21T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-21T10:00:02Z","type":"event_msg","payload":{"type":"goal_created","goal_id":"goal-abc","title":"Implement feature X","status":"active"}}"#,
                r#"{"timestamp":"2026-05-21T10:00:03Z","type":"event_msg","payload":{"type":"goal_updated","goal_id":"goal-abc","progress":0.25}}"#,
                r#"{"timestamp":"2026-05-21T10:00:04Z","type":"event_msg","payload":{"type":"goal_updated","goal_id":"goal-abc","progress":0.75}}"#,
                r#"{"timestamp":"2026-05-21T10:00:05Z","type":"event_msg","payload":{"type":"goal_completed","goal_id":"goal-abc","outcome":"success"}}"#,
                r#"{"timestamp":"2026-05-21T10:00:06Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748167206.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "goals-session");
        assert_eq!(session.cli_version.as_deref(), Some("0.133.0"));
        assert_eq!(session.turns.len(), 1);
        assert!(!session.is_ongoing);
    }

    #[test]
    fn parse_session_regular_exec_session_is_not_headless() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-05-08T10-02-00-exec.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-08T10:02:00Z","type":"session_meta","payload":{"id":"exec-session","timestamp":"2026-05-08T10:02:00Z","source":"exec","cli_version":"0.130.0"}}"#,
                r#"{"timestamp":"2026-05-08T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-08T10:02:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746698522.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let session = parse_session(&path).unwrap();
        assert!(!session.is_headless, "exec session must not be headless");
    }

    #[test]
    fn parse_session_unknown_record_types_are_skipped() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-04-30T10-03-00-unknown.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-04-30T10:03:00Z","type":"session_meta","payload":{"id":"unknown-types-session","timestamp":"2026-04-30T10:03:00Z"}}"#,
                r#"{"timestamp":"2026-04-30T10:03:01Z","type":"future_record_type_v999","payload":{"data":"some future data"}}"#,
                r#"{"timestamp":"2026-04-30T10:03:02Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-30T10:03:03Z","type":"another_unknown_type","payload":{}}"#,
                r#"{"timestamp":"2026-04-30T10:03:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746007384.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "unknown-types-session");
        assert_eq!(session.turns.len(), 1);
        assert!(!session.is_ongoing);
    }

    // Codex v0.131.0 (PRs #22594, #22647, #22724): profile-v2 layered config format.
    //
    // codex-trace reads JSONL session files, not Codex CLI TOML config files. The profile-v2
    // changes alter what Codex writes into session_meta: a `profile` field may appear naming
    // the active profile, and instructions now come via `base_instructions.text` from the
    // profile's system_prompt (the `instructions_file` config key is gone from Codex config).
    // All cases below must parse without panics and produce correct field values.
    //
    // Note: As of Codex v0.134.0 (PRs #23883, #24051, #24055, #24059), --profile-v2 was
    // renamed to --profile and all legacy profile v1 support was removed. See the v0134_*
    // tests below for the corresponding v0.134.0 verification.

    #[test]
    fn v0131_profile_v2_session_parses_correctly() {
        // session_meta from v0.131.0 with --profile-v2 active (renamed to --profile in
        // v0.134.0): carries `profile` field and instructions sourced from the profile's
        // system_prompt via base_instructions.text.
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-18T10-00-00-profilev2.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-18T10:00:00Z","type":"session_meta","payload":{"id":"v0131-profile-v2","timestamp":"2026-05-18T10:00:00Z","cwd":"/home/user","cli_version":"0.131.0","model_provider":"openai","profile":"work","base_instructions":{"text":"You are a helpful assistant."}}}"#,
                r#"{"timestamp":"2026-05-18T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-18T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747562402.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0131-profile-v2");
        assert_eq!(session.cli_version.as_deref(), Some("0.131.0"));
        // Instructions arrive from base_instructions.text (profile system_prompt).
        assert_eq!(
            session.instructions.as_deref(),
            Some("You are a helpful assistant.")
        );
        assert_eq!(session.turns.len(), 1);
        assert!(!session.is_ongoing);
    }

    #[test]
    fn v0131_session_without_instructions_file_parses_correctly() {
        // v0.131.0 removed `instructions_file` from the Codex config (PR #22724). Sessions
        // started without a profile providing instructions will have no `instructions` or
        // `base_instructions` in session_meta. The parser must return None, not panic.
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-05-18T10-01-00-noinstr.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-18T10:01:00Z","type":"session_meta","payload":{"id":"v0131-no-instructions","timestamp":"2026-05-18T10:01:00Z","cwd":"/home/user","cli_version":"0.131.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-05-18T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-18T10:01:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747562462.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0131-no-instructions");
        assert_eq!(session.cli_version.as_deref(), Some("0.131.0"));
        // instructions_file is gone — no instructions in this session.
        assert!(session.instructions.is_none());
        assert_eq!(session.turns.len(), 1);
    }

    #[test]
    fn v0131_legacy_profiles_section_absent_does_not_affect_session_parsing() {
        // PR #22647: Codex now rejects legacy [profiles] TOML when profile-v2 is active.
        // codex-trace reads only JSONL session files — it never touches Codex TOML config.
        // This test confirms standard v0.131.0 session files parse correctly regardless of
        // which config format the CLI was configured with.
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-05-18T10-02-00-v0131.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-18T10:02:00Z","type":"session_meta","payload":{"id":"v0131-standard","timestamp":"2026-05-18T10:02:00Z","cwd":"/workspace","cli_version":"0.131.0","model_provider":"openai","profile":"default"}}"#,
                r#"{"timestamp":"2026-05-18T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-18T10:02:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
                r#"{"timestamp":"2026-05-18T10:02:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/workspace"}}"#,
                r#"{"timestamp":"2026-05-18T10:02:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747562524.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0131-standard");
        assert_eq!(session.cli_version.as_deref(), Some("0.131.0"));
        assert_eq!(session.turns.len(), 1);
        assert!(!session.is_ongoing);
    }

    // Codex v0.131.0 (PR #22268): collab_agent_spawn_end uses new_session_id instead of
    // new_thread_id. Verify end-to-end: parse_session reads new_session_id, populates
    // spawned_worker_ids, and embed_worker_sessions correctly stitches the worker session.
    #[test]
    fn v0131_parse_session_stitches_worker_via_new_session_id() {
        let tmp = tempdir().unwrap();
        let parent_path = tmp
            .path()
            .join("rollout-2026-05-18T10-03-00-parent-v131.jsonl");
        let worker_path = tmp
            .path()
            .join("rollout-2026-05-18T10-03-09-worker-v131.jsonl");
        std::fs::write(
            &parent_path,
            [
                r#"{"timestamp":"2026-05-18T10:03:00Z","type":"session_meta","payload":{"id":"parent-v131","timestamp":"2026-05-18T10:03:00Z","cli_version":"0.131.0"}}"#,
                r#"{"timestamp":"2026-05-18T10:03:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-18T10:03:02Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"message\":\"Gather data\"}","call_id":"call-spawn-v131"}}"#,
                r#"{"timestamp":"2026-05-18T10:03:03Z","type":"event_msg","payload":{"type":"collab_agent_spawn_end","call_id":"call-spawn-v131","sender_session_id":"parent-v131","new_session_id":"worker-v131","new_agent_nickname":"Hypatia","new_agent_role":"worker","prompt":"Gather data","status":"pending_init"}}"#,
                r#"{"timestamp":"2026-05-18T10:03:04Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-spawn-v131","output":"{\"agent_id\":\"worker-v131\",\"nickname\":\"Hypatia\"}"}}"#,
                r#"{"timestamp":"2026-05-18T10:03:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747562585.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::write(
            &worker_path,
            [
                r#"{"timestamp":"2026-05-18T10:03:09Z","type":"session_meta","payload":{"id":"worker-v131","timestamp":"2026-05-18T10:03:09Z","cli_version":"0.131.0","cwd":"/tmp/worker"}}"#,
                r#"{"timestamp":"2026-05-18T10:03:10Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-worker"}}"#,
                r#"{"timestamp":"2026-05-18T10:03:11Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-worker","completed_at":1747562591.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&parent_path).unwrap();
        assert_eq!(session.id, "parent-v131");
        assert_eq!(session.spawned_worker_ids, vec!["worker-v131"]);
        assert_eq!(
            session.turns[0].collab_spawns[0].new_session_id,
            "worker-v131"
        );
        assert_eq!(session.turns[0].collab_spawns[0].agent_nickname, "Hypatia");
        let worker = session.turns[0].tool_calls[0]
            .worker_session
            .as_ref()
            .expect("spawn_agent tool call should embed worker session");
        assert_eq!(worker.id, "worker-v131");
    }

    // Codex v0.134.0 (PRs #23883, #24051, #24055, #24059): --profile-v2 renamed to --profile;
    // legacy profile v1 support removed entirely.
    //
    // codex-trace reads JSONL session files only — it never invokes `codex` or reads Codex
    // TOML config. Sessions from v0.134.0+ carry the same `profile` field in session_meta
    // as v0.131.0+ sessions. The parser is unaffected; these tests confirm v0.134.0
    // sessions parse correctly and produce the expected field values.

    #[test]
    fn v0134_profile_session_parses_correctly() {
        // session_meta from v0.134.0 with --profile active (flag renamed from --profile-v2).
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-05-26T10-00-00-profile.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-26T10:00:00Z","type":"session_meta","payload":{"id":"v0134-profile","timestamp":"2026-05-26T10:00:00Z","cwd":"/home/user","cli_version":"0.134.0","model_provider":"openai","profile":"work","base_instructions":{"text":"You are a helpful assistant."}}}"#,
                r#"{"timestamp":"2026-05-26T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-26T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748254802.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0134-profile");
        assert_eq!(session.cli_version.as_deref(), Some("0.134.0"));
        assert_eq!(
            session.instructions.as_deref(),
            Some("You are a helpful assistant.")
        );
        assert_eq!(session.turns.len(), 1);
        assert!(!session.is_ongoing);
    }

    #[test]
    fn v0134_session_without_profile_parses_correctly() {
        // v0.134.0 session started without --profile: no `profile` field in session_meta.
        // parse_session must return None for instructions, not panic.
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-26T10-01-00-noprofile.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-26T10:01:00Z","type":"session_meta","payload":{"id":"v0134-no-profile","timestamp":"2026-05-26T10:01:00Z","cwd":"/home/user","cli_version":"0.134.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-05-26T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-26T10:01:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748254862.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0134-no-profile");
        assert_eq!(session.cli_version.as_deref(), Some("0.134.0"));
        assert!(session.instructions.is_none());
        assert_eq!(session.turns.len(), 1);
    }

    #[test]
    fn v0134_legacy_profile_v1_absent_does_not_affect_session_parsing() {
        // v0.134.0 removed legacy profile v1 support entirely. Since codex-trace reads only
        // JSONL session files (never Codex TOML config), the removal has no effect on
        // parsing. Standard v0.134.0 sessions must parse correctly regardless.
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-05-26T10-02-00-v0134.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-26T10:02:00Z","type":"session_meta","payload":{"id":"v0134-standard","timestamp":"2026-05-26T10:02:00Z","cwd":"/workspace","cli_version":"0.134.0","model_provider":"openai","profile":"default"}}"#,
                r#"{"timestamp":"2026-05-26T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-26T10:02:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
                r#"{"timestamp":"2026-05-26T10:02:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/workspace"}}"#,
                r#"{"timestamp":"2026-05-26T10:02:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748254924.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0134-standard");
        assert_eq!(session.cli_version.as_deref(), Some("0.134.0"));
        assert_eq!(session.turns.len(), 1);
        assert!(!session.is_ongoing);
    }

    // Codex v0.133.0 (PR #22709): TurnContextItem fields trimmed.
    // turn_context payloads now carry only the model field; cwd and effort are no longer
    // emitted. Sessions from v0.133.0+ must parse correctly with the reduced payload.

    #[test]
    fn v0133_turn_context_trimmed_fields_session_parses_correctly() {
        // v0.133.0 session where turn_context has only model — cwd and effort are absent.
        // Verifies the parser extracts model from the trimmed payload and does not panic
        // on the missing fields.
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-05-21T10-00-00-v0133.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-21T10:00:00Z","type":"session_meta","payload":{"id":"v0133-turn-ctx","timestamp":"2026-05-21T10:00:00Z","cwd":"/workspace","cli_version":"0.133.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-05-21T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-21T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Done"}}"#,
                r#"{"timestamp":"2026-05-21T10:00:03Z","type":"turn_context","payload":{"model":"gpt-5"}}"#,
                r#"{"timestamp":"2026-05-21T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748167204.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0133-turn-ctx");
        assert_eq!(session.cli_version.as_deref(), Some("0.133.0"));
        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.turns[0].model.as_deref(), Some("gpt-5"));
        // cwd and effort absent in turn_context — must not panic
        assert!(session.turns[0].reasoning_effort.is_none());
        assert!(!session.is_ongoing);
    }

    // Codex v0.135.0 (PR #24591): memory state moved from file-based storage to a dedicated
    // SQLite DB. Active memories are injected into context at turn start and written into the
    // turn_context JSONL event. parse_session must expose them on each CodexTurn.

    #[test]
    fn v0135_session_with_memories_in_turn_context() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-28T10-00-00-v0135mem.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-28T10:00:00Z","type":"session_meta","payload":{"id":"v0135-mem-session","timestamp":"2026-05-28T10:00:00Z","cwd":"/project","cli_version":"0.135.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-05-28T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-28T10:00:02Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project","memories":["User prefers terse output","Project uses TypeScript strict mode"]}}"#,
                r#"{"timestamp":"2026-05-28T10:00:03Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
                r#"{"timestamp":"2026-05-28T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748426404.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0135-mem-session");
        assert_eq!(session.cli_version.as_deref(), Some("0.135.0"));
        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.turns[0].memories.len(), 2);
        assert_eq!(
            session.turns[0].memories[0].content,
            "User prefers terse output"
        );
        assert_eq!(session.turns[0].memories[0].version, None);
        assert_eq!(
            session.turns[0].memories[1].content,
            "Project uses TypeScript strict mode"
        );
        assert!(!session.is_ongoing);
    }

    // Codex v0.134.0 (PR #22882): subagent identity fields added to hook input payloads.
    //
    // Tool call end events now optionally carry `subagent_id` and `subagent_name`.
    // The full parse pipeline must propagate these fields from the JSONL events through
    // to the ToolCall structs returned in CodexSession.turns[].tool_calls[].

    #[test]
    fn v0134_parse_session_with_subagent_identity_in_exec_command_end() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-26T10-00-00-v0134sub.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-26T10:00:00Z","type":"session_meta","payload":{"id":"v0134-sub-session","timestamp":"2026-05-26T10:00:00Z","cwd":"/tmp","cli_version":"0.134.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-05-26T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-26T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"echo hello\",\"workdir\":\"/tmp\"}","call_id":"call_sub_1","subagent_id":"worker-sess-abc","subagent_name":"Parfit"}}"#,
                r#"{"timestamp":"2026-05-26T10:00:03Z","type":"event_msg","payload":{"type":"exec_command_end","call_id":"call_sub_1","aggregated_output":"hello\n","exit_code":0,"status":"completed","duration":{"secs":0,"nanos":50000000},"subagent_id":"worker-sess-abc","subagent_name":"Parfit"}}"#,
                r#"{"timestamp":"2026-05-26T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748253604.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0134-sub-session");
        assert_eq!(session.cli_version.as_deref(), Some("0.134.0"));
        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.turns[0].tool_calls.len(), 1);
        let tool = &session.turns[0].tool_calls[0];
        assert_eq!(tool.name, "exec_command");
        assert_eq!(tool.subagent_id.as_deref(), Some("worker-sess-abc"));
        assert_eq!(tool.subagent_name.as_deref(), Some("Parfit"));
        assert!(!session.is_ongoing);
    }

    #[test]
    fn v0134_parse_session_without_subagent_identity_fields_is_unaffected() {
        // Pre-v0.134.0 sessions and parent-agent tool calls must parse normally
        // with subagent_id/subagent_name defaulting to None on every ToolCall.
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-26T10-01-00-v0134nosub.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-26T10:01:00Z","type":"session_meta","payload":{"id":"v0134-no-sub","timestamp":"2026-05-26T10:01:00Z","cwd":"/tmp","cli_version":"0.134.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-05-26T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-26T10:01:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"ls\",\"workdir\":\"/tmp\"}","call_id":"call_plain"}}"#,
                r#"{"timestamp":"2026-05-26T10:01:03Z","type":"event_msg","payload":{"type":"exec_command_end","call_id":"call_plain","aggregated_output":"file.txt\n","exit_code":0,"status":"completed","duration":{"secs":0,"nanos":5000000}}}"#,
                r#"{"timestamp":"2026-05-26T10:01:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748253664.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0134-no-sub");
        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.turns[0].tool_calls.len(), 1);
        let tool = &session.turns[0].tool_calls[0];
        assert!(
            tool.subagent_id.is_none(),
            "subagent_id must be None for parent-agent calls"
        );
        assert!(
            tool.subagent_name.is_none(),
            "subagent_name must be None for parent-agent calls"
        );
    }

    // Codex v0.137.0 (PRs #25089, #25087): cold session rollout files are now stored
    // compressed with zstd. parse_session must detect the magic bytes and decompress
    // transparently before parsing.

    fn compress_zstd(data: &[u8]) -> Vec<u8> {
        zstd::encode_all(data, 3).expect("zstd compress failed")
    }

    #[test]
    fn v0137_parse_session_from_zstd_compressed_file() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-06-04T10-00-00-v0137compressed.jsonl");
        let content = [
            r#"{"timestamp":"2026-06-04T10:00:00Z","type":"session_meta","payload":{"id":"v0137-zstd-session","timestamp":"2026-06-04T10:00:00Z","cwd":"/project","cli_version":"0.137.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-04T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-04T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello from compressed session"}}"#,
            r#"{"timestamp":"2026-06-04T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748995203.0}}"#,
        ]
        .join("\n");
        std::fs::write(&path, compress_zstd(content.as_bytes())).unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0137-zstd-session");
        assert_eq!(session.cli_version.as_deref(), Some("0.137.0"));
        assert_eq!(session.turns.len(), 1);
        assert!(!session.is_ongoing);
    }

    #[test]
    fn v0137_parse_session_plain_still_works() {
        // Non-compressed files from older Codex versions must continue to parse correctly.
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-06-04T10-01-00-v0136plain.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-06-04T10:01:00Z","type":"session_meta","payload":{"id":"v0136-plain-session","timestamp":"2026-06-04T10:01:00Z","cwd":"/project","cli_version":"0.136.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-06-04T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-06-04T10:01:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748995262.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0136-plain-session");
        assert_eq!(session.cli_version.as_deref(), Some("0.136.0"));
        assert_eq!(session.turns.len(), 1);
        assert!(!session.is_ongoing);
    }

    #[test]
    fn v0135_session_without_memories_produces_empty_vec() {
        // Pre-v0.135.0 sessions must parse normally with an empty memories Vec.
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-05-28T10-01-00-nomem.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-28T10:01:00Z","type":"session_meta","payload":{"id":"v0134-no-memories","timestamp":"2026-05-28T10:01:00Z","cwd":"/project","cli_version":"0.134.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-05-28T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-28T10:01:02Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project","effort":"high"}}"#,
                r#"{"timestamp":"2026-05-28T10:01:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748426463.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0134-no-memories");
        assert!(session.turns[0].memories.is_empty());
    }

    // Codex v0.132.0 (PR #23148): memory summaries are now versioned. Memory items in
    // turn_context are objects {"content":"...","version":N} instead of plain strings.
    // The full parse pipeline must expose both content and version on each MemorySummary.

    #[test]
    fn v0132_session_with_versioned_memory_summaries_parsed_correctly() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-20T10-00-00-v0132mem.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"v0132-versioned-mem","timestamp":"2026-05-20T10:00:00Z","cwd":"/project","cli_version":"0.132.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-20T10:00:02Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project","memories":[{"content":"User prefers terse output","version":1},{"content":"Project uses TypeScript strict mode","version":2}]}}"#,
                r#"{"timestamp":"2026-05-20T10:00:03Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
                r#"{"timestamp":"2026-05-20T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748253604.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0132-versioned-mem");
        assert_eq!(session.cli_version.as_deref(), Some("0.132.0"));
        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.turns[0].memories.len(), 2);
        assert_eq!(
            session.turns[0].memories[0].content,
            "User prefers terse output"
        );
        assert_eq!(session.turns[0].memories[0].version, Some(1));
        assert_eq!(
            session.turns[0].memories[1].content,
            "Project uses TypeScript strict mode"
        );
        assert_eq!(session.turns[0].memories[1].version, Some(2));
    }

    // Codex v0.137.0 (PR #26114): hide_spawn_agent_metadata now defaults to true.
    // When active, spawn_agent function_call_output is empty (no agent_id/nickname JSON),
    // producing a tool call with status "unknown". codex-trace must detect this and set
    // has_missing_spawn_metadata = true so the UI can warn users to enable the config flag.

    #[test]
    fn v0137_spawn_agent_with_hidden_metadata_sets_flag() {
        // Simulates a Codex v0.137.0 session where hide_spawn_agent_metadata = true (default).
        // The function_call_output for spawn_agent is empty — no agent_id or nickname JSON.
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-06-04T10-00-00-hidden-meta.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-06-04T10:00:00Z","type":"session_meta","payload":{"id":"v0137-hidden-meta","timestamp":"2026-06-04T10:00:00Z","cwd":"/project","cli_version":"0.137.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-06-04T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-06-04T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"message\":\"Do subtask\"}","call_id":"call-spawn-1"}}"#,
                // Empty output — hide_spawn_agent_metadata = true suppresses the agent_id/nickname JSON
                r#"{"timestamp":"2026-06-04T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-spawn-1","output":""}}"#,
                r#"{"timestamp":"2026-06-04T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1749034804.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0137-hidden-meta");
        assert!(
            session.has_missing_spawn_metadata,
            "session with empty spawn_agent output must set has_missing_spawn_metadata"
        );
        // spawned_worker_ids must be empty — no metadata to stitch workers
        assert!(session.spawned_worker_ids.is_empty());
        // The tool call must be classified as SpawnAgent with status unknown
        let tool = &session.turns[0].tool_calls[0];
        assert_eq!(tool.name, "spawn_agent");
        assert_eq!(tool.status, "unknown");
    }

    #[test]
    fn v0137_spawn_agent_with_metadata_present_does_not_set_flag() {
        // Codex v0.137.0 session where hide_spawn_agent_metadata = false (opted back in).
        // The function_call_output carries full JSON metadata — no warning should fire.
        let tmp = tempdir().unwrap();
        let parent_path = tmp
            .path()
            .join("rollout-2026-06-04T10-01-00-with-meta.jsonl");
        let worker_path = tmp.path().join("rollout-2026-06-04T10-01-09-worker.jsonl");
        std::fs::write(
            &parent_path,
            [
                r#"{"timestamp":"2026-06-04T10:01:00Z","type":"session_meta","payload":{"id":"v0137-with-meta","timestamp":"2026-06-04T10:01:00Z","cwd":"/project","cli_version":"0.137.0"}}"#,
                r#"{"timestamp":"2026-06-04T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-06-04T10:01:02Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"message\":\"Do subtask\"}","call_id":"call-spawn-2"}}"#,
                r#"{"timestamp":"2026-06-04T10:01:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-spawn-2","output":"{\"agent_id\":\"worker-137\",\"nickname\":\"Ada\"}"}}"#,
                r#"{"timestamp":"2026-06-04T10:01:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1749034864.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::write(
            &worker_path,
            [
                r#"{"timestamp":"2026-06-04T10:01:09Z","type":"session_meta","payload":{"id":"worker-137","timestamp":"2026-06-04T10:01:09Z","cwd":"/project/worker"}}"#,
                r#"{"timestamp":"2026-06-04T10:01:10Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-w"}}"#,
                r#"{"timestamp":"2026-06-04T10:01:11Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-w","completed_at":1749034871.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&parent_path).unwrap();
        assert_eq!(session.id, "v0137-with-meta");
        assert!(
            !session.has_missing_spawn_metadata,
            "session with present spawn metadata must not set has_missing_spawn_metadata"
        );
        assert_eq!(session.spawned_worker_ids, vec!["worker-137"]);
    }

    // Codex v0.132.0 (PR #23123): `codex exec resume --output-schema` produces structured
    // JSON output items. A full parse_session must capture the structured_output response_item
    // content as the turn's final_answer so exec sessions with --output-schema display correctly.

    #[test]
    fn v0132_exec_resume_with_output_schema_captures_structured_final_answer() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-20T10-00-00-structured.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"v0132-structured-session","timestamp":"2026-05-20T10:00:00Z","cwd":"/tmp","cli_version":"0.132.0","output_schema":{"type":"object","properties":{"result":{"type":"string"}}}}}"#,
                r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-20T10:00:02Z","type":"response_item","payload":{"type":"structured_output","content":{"result":"task completed successfully"}}}"#,
                r#"{"timestamp":"2026-05-20T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748606403.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0132-structured-session");
        assert_eq!(session.cli_version.as_deref(), Some("0.132.0"));
        assert_eq!(session.turns.len(), 1);
        let turn = &session.turns[0];
        let answer = turn
            .final_answer
            .as_deref()
            .expect("final_answer must be populated from structured_output response_item");
        assert!(
            answer.contains("task completed successfully"),
            "final_answer must contain the structured JSON content"
        );
        assert!(!session.is_ongoing);
    }

    #[test]
    fn v0132_parse_session_with_structured_output_populates_final_answer() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-20T10-00-00-v0132schema.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"v0132-schema-session","timestamp":"2026-05-20T10:00:00Z","cwd":"/tmp","cli_version":"0.132.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-20T10:00:02Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/tmp"}}"#,
                // structured_output item from --output-schema exec session
                r#"{"timestamp":"2026-05-20T10:00:03Z","type":"response_item","payload":{"type":"structured_output","content":{"result":"success","count":7}}}"#,
                r#"{"timestamp":"2026-05-20T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748606404.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0132-schema-session");
        assert_eq!(session.cli_version.as_deref(), Some("0.132.0"));
        assert_eq!(session.turns.len(), 1);
        let turn = &session.turns[0];
        assert!(
            turn.final_answer.is_some(),
            "structured_output response_item must populate final_answer"
        );
        let answer = turn.final_answer.as_ref().unwrap();
        let v: serde_json::Value =
            serde_json::from_str(answer).expect("final_answer must be valid JSON");
        assert_eq!(v["result"], "success");
        assert_eq!(v["count"], 7);
        assert!(!session.is_ongoing);
    }

    #[test]
    fn v0132_exec_resume_with_output_schema_and_tool_calls_parses_fully() {
        // Full exec resume session: exec_command tool call followed by structured_output.
        // Verifies that both the tool call and the structured final answer are parsed correctly.
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-20T10-01-00-structured-full.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-20T10:01:00Z","type":"session_meta","payload":{"id":"v0132-structured-full","timestamp":"2026-05-20T10:01:00Z","cwd":"/project","cli_version":"0.132.0","output_schema":{"type":"object","properties":{"files":{"type":"array"}}}}}"#,
                r#"{"timestamp":"2026-05-20T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-20T10:01:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"ls /project\",\"workdir\":\"/project\"}","call_id":"call_ls"}}"#,
                r#"{"timestamp":"2026-05-20T10:01:03Z","type":"event_msg","payload":{"type":"exec_command_end","call_id":"call_ls","aggregated_output":"main.rs\nCargo.toml\n","exit_code":0,"status":"completed","duration":{"secs":0,"nanos":50000000}}}"#,
                r#"{"timestamp":"2026-05-20T10:01:04Z","type":"response_item","payload":{"type":"structured_output","content":{"files":["main.rs","Cargo.toml"]}}}"#,
                r#"{"timestamp":"2026-05-20T10:01:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748606465.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0132-structured-full");
        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.turns[0].tool_calls.len(), 1);
        assert_eq!(session.turns[0].tool_calls[0].name, "exec_command");
        let answer = session.turns[0]
            .final_answer
            .as_deref()
            .expect("final_answer must be captured from structured_output");
        assert!(answer.contains("main.rs"));
        assert!(!session.is_ongoing);
    }

    #[test]
    fn v0132_parse_session_with_message_json_content_populates_final_answer() {
        // Codex v0.132.0+ (PR #23123): --output-schema sessions may emit the structured
        // response as a "message" response_item where content is a JSON object.
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-20T10-01-00-v0132msgschema.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-20T10:01:00Z","type":"session_meta","payload":{"id":"v0132-msg-schema","timestamp":"2026-05-20T10:01:00Z","cwd":"/tmp","cli_version":"0.132.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-05-20T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-20T10:01:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":{"status":"ok","output":"done"}}}"#,
                r#"{"timestamp":"2026-05-20T10:01:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748606463.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0132-msg-schema");
        assert_eq!(session.turns.len(), 1);
        let turn = &session.turns[0];
        assert!(
            turn.final_answer.is_some(),
            "message response_item with JSON content must populate final_answer"
        );
        let answer = turn.final_answer.as_ref().unwrap();
        let v: serde_json::Value =
            serde_json::from_str(answer).expect("final_answer must be valid JSON");
        assert_eq!(v["status"], "ok");
        assert!(!session.is_ongoing);
    }

    #[test]
    fn v0137_session_without_spawn_agent_does_not_set_flag() {
        // Sessions with no spawn_agent calls must never set has_missing_spawn_metadata.
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-06-04T10-02-00-no-spawn.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-06-04T10:02:00Z","type":"session_meta","payload":{"id":"v0137-no-spawn","timestamp":"2026-06-04T10:02:00Z","cwd":"/project","cli_version":"0.137.0"}}"#,
                r#"{"timestamp":"2026-06-04T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-06-04T10:02:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"echo hi\"}","call_id":"call-exec"}}"#,
                r#"{"timestamp":"2026-06-04T10:02:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-exec","output":"hi\n"}}"#,
                r#"{"timestamp":"2026-06-04T10:02:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1749034924.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0137-no-spawn");
        assert!(
            !session.has_missing_spawn_metadata,
            "session without spawn_agent must not set has_missing_spawn_metadata"
        );
    }

    // Codex v0.138.0 (PRs #25944, #25947): image_generation results now carry a top-level
    // file_path field exposing the saved file path. parse_session must propagate this field
    // through to the ToolCall so the UI can link to the saved image.

    #[test]
    fn v0138_parse_session_image_generation_with_file_path() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-06-08T10-00-00-v0138img.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-06-08T10:00:00Z","type":"session_meta","payload":{"id":"v0138-img-session","timestamp":"2026-06-08T10:00:00Z","cwd":"/project","cli_version":"0.138.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-06-08T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-06-08T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"image_generation","call_id":"call_img_138","arguments":"{\"prompt\":\"a sunset over mountains\",\"size\":\"1024x1024\"}"}}"#,
                r#"{"timestamp":"2026-06-08T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_img_138","output":[{"type":"image_url","image_url":{"url":"data:image/png;base64,abc123"}}],"file_path":"/home/user/.codex/images/sunset_abc123.png"}}"#,
                r#"{"timestamp":"2026-06-08T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1749376804.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0138-img-session");
        assert_eq!(session.cli_version.as_deref(), Some("0.138.0"));
        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.turns[0].tool_calls.len(), 1);
        let tool = &session.turns[0].tool_calls[0];
        assert_eq!(tool.name, "image_generation");
        assert_eq!(
            tool.image_prompt.as_deref(),
            Some("a sunset over mountains")
        );
        assert_eq!(
            tool.image_file_path.as_deref(),
            Some("/home/user/.codex/images/sunset_abc123.png"),
            "image_file_path must be populated from the file_path field"
        );
        assert!(!session.is_ongoing);
    }

    #[test]
    fn v0138_parse_session_image_generation_without_file_path_is_backward_compatible() {
        // Pre-v0.138.0 sessions must parse normally with image_file_path as None.
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-06-08T10-01-00-v0135imgold.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-06-08T10:01:00Z","type":"session_meta","payload":{"id":"v0135-img-old","timestamp":"2026-06-08T10:01:00Z","cwd":"/project","cli_version":"0.135.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-06-08T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-06-08T10:01:02Z","type":"response_item","payload":{"type":"function_call","name":"image_generation","call_id":"call_img_old","arguments":"{\"prompt\":\"a mountain lake\"}"}}"#,
                r#"{"timestamp":"2026-06-08T10:01:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_img_old","output":[{"type":"image_url","image_url":{"url":"data:image/png;base64,def456"}}]}}"#,
                r#"{"timestamp":"2026-06-08T10:01:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1749376864.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0135-img-old");
        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.turns[0].tool_calls.len(), 1);
        let tool = &session.turns[0].tool_calls[0];
        assert_eq!(tool.name, "image_generation");
        assert_eq!(tool.image_prompt.as_deref(), Some("a mountain lake"));
        assert!(
            tool.image_file_path.is_none(),
            "image_file_path must be None when file_path is absent"
        );
    }

    // Codex v0.139.0 (PRs #24118, #27084): tool/connector input schemas preserve
    // oneOf/allOf instead of flattening. parse_session must handle session_meta entries
    // with complex input_schema arrays without panic.

    #[test]
    fn v0139_session_with_complex_tool_schemas_produces_correct_tool_calls() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-06-09T10-02-00-v0139toolcall.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-06-09T10:02:00Z","type":"session_meta","payload":{"id":"v0139-toolcall-session","timestamp":"2026-06-09T10:02:00Z","cwd":"/project","cli_version":"0.139.0","model_provider":"openai","tools":[{"name":"exec_command","input_schema":{"type":"object","properties":{"cmd":{"type":"string"},"workdir":{"oneOf":[{"type":"string"},{"type":"null"}]}},"required":["cmd"]}}]}}"#,
                r#"{"timestamp":"2026-06-09T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-06-09T10:02:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"echo hello\",\"workdir\":\"/project\"}","call_id":"call-v0139-1"}}"#,
                r#"{"timestamp":"2026-06-09T10:02:03Z","type":"event_msg","payload":{"type":"exec_command_end","call_id":"call-v0139-1","aggregated_output":"hello\n","exit_code":0,"status":"completed","duration":{"secs":0,"nanos":50000000}}}"#,
                r#"{"timestamp":"2026-06-09T10:02:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1749466924.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0139-toolcall-session");
        assert_eq!(session.cli_version.as_deref(), Some("0.139.0"));
        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.turns[0].tool_calls.len(), 1);
        let tool = &session.turns[0].tool_calls[0];
        assert_eq!(tool.name, "exec_command");
        assert_eq!(tool.output.as_deref(), Some("hello\n"));
        assert_eq!(tool.exit_code, Some(0));
        assert_eq!(tool.status, "completed");
        assert!(!session.is_ongoing);
    }

    // Codex v0.141.0 (PRs #26242, #26245): exec-server remote transport migrated to
    // authenticated Noise relay channels. codex-trace reads session data from JSONL files
    // on disk — the Noise relay is a network-layer change invisible to this parser. The
    // app-server decrypts Noise frames before logging events to ~/.codex/sessions/; the
    // on-disk JSONL format is unchanged. Verify standard v0.141.0 sessions parse correctly.

    #[test]
    fn v0141_session_parses_correctly() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-06-18T10-00-00-v0141.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-06-18T10:00:00Z","type":"session_meta","payload":{"id":"v0141-session","timestamp":"2026-06-18T10:00:00Z","cwd":"/project","cli_version":"0.141.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-06-18T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-06-18T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"echo hello\",\"workdir\":\"/project\"}","call_id":"call-v0141-1"}}"#,
                r#"{"timestamp":"2026-06-18T10:00:03Z","type":"event_msg","payload":{"type":"exec_command_end","call_id":"call-v0141-1","aggregated_output":"hello\n","exit_code":0,"status":"completed","duration":{"secs":0,"nanos":10000000}}}"#,
                r#"{"timestamp":"2026-06-18T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1750244404.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0141-session");
        assert_eq!(session.cli_version.as_deref(), Some("0.141.0"));
        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.turns[0].tool_calls.len(), 1);
        let tool = &session.turns[0].tool_calls[0];
        assert_eq!(tool.name, "exec_command");
        assert_eq!(tool.output.as_deref(), Some("hello\n"));
        assert_eq!(tool.exit_code, Some(0));
        assert_eq!(tool.status, "completed");
        assert!(!session.is_ongoing);
    }

    // Codex v0.141.0 (PR #28355): ResponseItem gains a new optional top-level `metadata` field.

    #[test]
    fn v0141_parse_session_with_response_item_metadata_field() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-06-18T10-00-00-v0141meta.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-06-18T10:00:00Z","type":"session_meta","payload":{"id":"v0141-meta-session","timestamp":"2026-06-18T10:00:00Z","cwd":"/project","cli_version":"0.141.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-06-18T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-06-18T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call-v141-meta","arguments":"{\"cmd\":\"echo hello\"}","metadata":{"priority":"normal","request_id":"req-meta-v141"}}}"#,
                r#"{"timestamp":"2026-06-18T10:00:03Z","type":"event_msg","payload":{"type":"exec_command_end","call_id":"call-v141-meta","aggregated_output":"hello\n","exit_code":0,"status":"completed","duration":{"secs":0,"nanos":20000000}}}"#,
                r#"{"timestamp":"2026-06-18T10:00:04Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Done","metadata":{"server_key":"srv-0141","usage":{"prompt_tokens":5,"completion_tokens":2}}}}"#,
                r#"{"timestamp":"2026-06-18T10:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1750240805.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0141-meta-session");
        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.turns[0].tool_calls.len(), 1);
        let tool = &session.turns[0].tool_calls[0];
        assert_eq!(tool.output.as_deref(), Some("hello\n"));
        assert_eq!(
            session.turns[0].final_answer.as_deref(),
            Some("Done"),
            "message with metadata must populate final_answer"
        );
        assert!(!session.is_ongoing);
    }

    // Codex v0.144.0 (PR #31494): paste-triggered TUI corruption fixed.
    // Sessions captured before v0.144.0 may contain JSONL lines where pasted terminal
    // control sequences were embedded unescaped in JSON strings, producing invalid JSON.
    // parse_session must skip those lines via filter_map(RawEntry::parse) and successfully
    // return the turns built from the remaining valid entries.

    #[test]
    fn v0144_session_with_corrupted_line_skips_bad_entry_and_returns_valid_turns() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-07-01T10-00-00-v0144corrupt.jsonl");
        // Embed a literal ESC byte (U+001B) inside a JSON string to simulate the
        // pre-v0.144.0 paste corruption. The surrounding valid lines must still produce
        // a complete turn.
        let esc = '\x1b';
        let corrupted_user_msg = format!(
            r#"{{"timestamp":"2026-07-01T10:00:02Z","type":"event_msg","payload":{{"type":"user_message","content":"pasted: {}[31m red text"}}}}"#,
            esc
        );
        let lines = [
            r#"{"timestamp":"2026-07-01T10:00:00Z","type":"session_meta","payload":{"id":"v0144-corrupt-session","timestamp":"2026-07-01T10:00:00Z","cwd":"/project","cli_version":"0.143.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-07-01T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            corrupted_user_msg.as_str(),
            r#"{"timestamp":"2026-07-01T10:00:03Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Done"}}"#,
            r#"{"timestamp":"2026-07-01T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1751414404.0}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0144-corrupt-session");
        // Corrupted user_message line is silently skipped; the turn is still captured
        // from task_started / task_complete boundaries.
        assert_eq!(session.turns.len(), 1);
        assert_eq!(
            session.turns[0].final_answer.as_deref(),
            Some("Done"),
            "final_answer must be populated from the valid response_item after the corrupted line"
        );
        assert!(!session.is_ongoing);
    }

    // Codex v0.142.0 (PR #28968): `metadata` on chat message response_items renamed to
    // `internal_chat_message_metadata_passthrough`. Sessions written by v0.142.0+ must parse
    // correctly; final_answer must still be populated from `content`, not the metadata field.

    #[test]
    fn v0142_parse_session_with_internal_chat_message_metadata_passthrough() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-06-22T10-00-00-v0142meta.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-06-22T10:00:00Z","type":"session_meta","payload":{"id":"v0142-meta-session","timestamp":"2026-06-22T10:00:00Z","cwd":"/project","cli_version":"0.142.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-06-22T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-06-22T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call-v142-meta","arguments":"{\"cmd\":\"echo hello\"}","internal_chat_message_metadata_passthrough":{"priority":"normal","request_id":"req-meta-v142"}}}"#,
                r#"{"timestamp":"2026-06-22T10:00:03Z","type":"event_msg","payload":{"type":"exec_command_end","call_id":"call-v142-meta","aggregated_output":"hello\n","exit_code":0,"status":"completed","duration":{"secs":0,"nanos":20000000}}}"#,
                r#"{"timestamp":"2026-06-22T10:00:04Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Done v0142","internal_chat_message_metadata_passthrough":{"server_key":"srv-0142","usage":{"prompt_tokens":5,"completion_tokens":2}}}}"#,
                r#"{"timestamp":"2026-06-22T10:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1750327205.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0142-meta-session");
        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.turns[0].tool_calls.len(), 1);
        let tool = &session.turns[0].tool_calls[0];
        assert_eq!(tool.output.as_deref(), Some("hello\n"));
        assert_eq!(
            session.turns[0].final_answer.as_deref(),
            Some("Done v0142"),
            "message with internal_chat_message_metadata_passthrough must populate final_answer"
        );
        assert!(!session.is_ongoing);
    }

    // Codex v0.143.0 (PRs #29918, #30144): trailing realtime transcript text and terminal
    // rollout events are now preserved during shutdown rather than dropped. Sessions written
    // by v0.143.0+ include trailing unknown events between task_complete and session_end, and
    // session_end is reliably written by an atexit handler even after crashes.

    #[test]
    fn v0143_trailing_transcript_and_reliable_session_end_parse_correctly() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-07-12T10-00-00-v0143trail.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-07-12T10:00:00Z","type":"session_meta","payload":{"id":"v0143-trail-session","timestamp":"2026-07-12T10:00:00Z","cwd":"/project","cli_version":"0.143.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-07-12T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-07-12T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call-v143-trail","arguments":"{\"cmd\":\"echo done\"}"}}"#,
                r#"{"timestamp":"2026-07-12T10:00:03Z","type":"event_msg","payload":{"type":"exec_command_end","call_id":"call-v143-trail","aggregated_output":"done\n","exit_code":0,"status":"completed","duration":{"secs":0,"nanos":5000000}}}"#,
                r#"{"timestamp":"2026-07-12T10:00:04Z","type":"event_msg","payload":{"type":"agent_message","message":"All done.","phase":"final_answer"}}"#,
                r#"{"timestamp":"2026-07-12T10:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1752314405.0}}"#,
                r#"{"timestamp":"2026-07-12T10:00:06Z","type":"event_msg","payload":{"type":"realtime_transcript_text","text":"trailing output preserved by v0.143.0","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-07-12T10:00:07Z","type":"session_end","payload":{}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0143-trail-session");
        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.turns[0].tool_calls.len(), 1);
        assert_eq!(
            session.turns[0].tool_calls[0].output.as_deref(),
            Some("done\n")
        );
        assert_eq!(
            session.turns[0].final_answer.as_deref(),
            Some("All done."),
            "final_answer must be populated from agent_message before task_complete"
        );
        assert!(
            !session.is_ongoing,
            "session_end marker must close the session regardless of file freshness"
        );
    }

    #[test]
    fn v0143_session_end_without_task_complete_marks_turn_aborted() {
        // v0.143.0 writes session_end via atexit handler even after a crash. If the process
        // dies between task_started and task_complete, session_end appears without a preceding
        // task_complete. The abrupt-cutoff workaround must fire and set status=Aborted.
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-07-12T10-01-00-v0143crash.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-07-12T10:01:00Z","type":"session_meta","payload":{"id":"v0143-crash-session","timestamp":"2026-07-12T10:01:00Z","cwd":"/project","cli_version":"0.143.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-07-12T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-07-12T10:01:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call-v143-crash","arguments":"{\"cmd\":\"long-running-job\"}"}}"#,
                r#"{"timestamp":"2026-07-12T10:01:03Z","type":"session_end","payload":{}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0143-crash-session");
        assert_eq!(session.turns.len(), 1);
        assert_eq!(
            session.turns[0].status,
            TurnStatus::Aborted,
            "session_end without task_complete must trigger abrupt-cutoff → Aborted"
        );
        assert!(
            !session.is_ongoing,
            "session_end marker must close the session regardless of turn state"
        );
    }

    // Codex v0.145.0: streaming realtime V3 audio conversations reintroduced (issue #201).
    // Unlike v0.143.0's trailing-only events (after task_complete), v0.145.0 emits
    // realtime_transcript_text mid-turn during live streaming voice conversations.
    // The turn model must capture audio_transcript content, not silently drop it.

    #[test]
    fn v0145_live_realtime_transcript_mid_turn_is_captured() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-07-26T10-00-00-v0145audio.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-07-26T10:00:00Z","type":"session_meta","payload":{"id":"v0145-audio-session","timestamp":"2026-07-26T10:00:00Z","cwd":"/project","cli_version":"0.145.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-07-26T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-a1"}}"#,
                r#"{"timestamp":"2026-07-26T10:00:02Z","type":"event_msg","payload":{"type":"realtime_transcript_text","text":"Hello, please run the tests","turn_id":"turn-a1"}}"#,
                r#"{"timestamp":"2026-07-26T10:00:03Z","type":"event_msg","payload":{"type":"realtime_transcript_text","text":" for the current project","turn_id":"turn-a1"}}"#,
                r#"{"timestamp":"2026-07-26T10:00:04Z","type":"event_msg","payload":{"type":"agent_message","message":"Running the tests now.","phase":"final_answer"}}"#,
                r#"{"timestamp":"2026-07-26T10:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-a1","completed_at":1753520405.0}}"#,
                r#"{"timestamp":"2026-07-26T10:00:06Z","type":"session_end","payload":{}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0145-audio-session");
        assert_eq!(session.turns.len(), 1);
        assert_eq!(
            session.turns[0].audio_transcript,
            vec![
                "Hello, please run the tests".to_string(),
                " for the current project".to_string(),
            ],
            "mid-turn realtime_transcript_text events must be extracted into audio_transcript"
        );
        assert_eq!(
            session.turns[0].final_answer.as_deref(),
            Some("Running the tests now."),
        );
        assert!(!session.is_ongoing);
    }

    // Codex v0.146.0 (PRs #34621, #35220, issue #210): a paginated thread that forks or
    // inherits history from another rollout carries session_meta.history_base.thread_id,
    // pointing at the source rollout it continues from. This is distinct from the per-turn
    // forked_from_thread_id and compaction lineage_id fields.

    #[test]
    fn v0146_parse_session_reads_history_base_thread_id() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-08-01T10-00-00-v0146fork.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-08-01T10:00:00Z","type":"session_meta","payload":{"id":"v0146-fork-child","timestamp":"2026-08-01T10:00:00Z","cwd":"/project","cli_version":"0.146.0","model_provider":"openai","history_mode":"paginated","history_base":{"thread_id":"v0146-fork-parent","end_ordinal_exclusive":5,"end_byte_offset":512}}}"#,
                r#"{"timestamp":"2026-08-01T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-08-01T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1785657602.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(
            session.history_base_thread_id.as_deref(),
            Some("v0146-fork-parent")
        );
    }

    #[test]
    fn v0146_parse_session_no_history_base_is_none() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-08-01T10-01-00-v0146nofork.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-08-01T10:01:00Z","type":"session_meta","payload":{"id":"v0146-nofork","timestamp":"2026-08-01T10:01:00Z","cwd":"/project","cli_version":"0.146.0","model_provider":"openai","history_mode":"legacy"}}"#,
                r#"{"timestamp":"2026-08-01T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-08-01T10:01:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1785657662.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert!(session.history_base_thread_id.is_none());
    }

    // Codex v0.148.0/v0.149.0 (PRs #37367, #38819, issue #238): `codex exec fork` and
    // `codex exec resume` populate the app-server's SessionConfiguredEvent.forked_from_id
    // (previously hardcoded to None in codex-rs/exec/src/lib.rs). That value is persisted
    // verbatim into the rollout's session_meta.forked_from_id field by
    // codex-rs/rollout/src/recorder.rs for any forked or fork-descended thread — a distinct,
    // session-level signal from the per-turn task_started.forked_from_thread_id field.

    #[test]
    fn v0148_parse_session_reads_forked_from_id_for_exec_fork() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-08-20T10-00-00-v0148execfork.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-08-20T10:00:00Z","type":"session_meta","payload":{"id":"v0148-exec-fork-child","timestamp":"2026-08-20T10:00:00Z","cwd":"/project","cli_version":"0.148.0","model_provider":"openai","originator":"codex_exec","forked_from_id":"v0148-exec-fork-parent"}}"#,
                r#"{"timestamp":"2026-08-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-08-20T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1787306402.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(
            session.forked_from_thread_id.as_deref(),
            Some("v0148-exec-fork-parent"),
            "session_meta.forked_from_id must be surfaced as forked_from_thread_id even with \
             no task_started.forked_from_thread_id present"
        );
    }

    #[test]
    fn v0148_parse_session_no_forked_from_id_is_none() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-08-20T10-01-00-v0148nofork.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-08-20T10:01:00Z","type":"session_meta","payload":{"id":"v0148-nofork","timestamp":"2026-08-20T10:01:00Z","cwd":"/project","cli_version":"0.148.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-08-20T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-08-20T10:01:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1787306462.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert!(session.forked_from_thread_id.is_none());
    }

    // Codex v0.146.0 (issue #211): the new skills subsystem renders the skill catalog into
    // the developer message on each turn and emits a plain `EventMsg::Warning` when the
    // catalog is truncated or skills are omitted for budget reasons
    // (`codex-rs/core/src/session/mod.rs`, `build_available_skills` call site). codex-trace
    // had no handling for the generic `warning` event_msg type at all, so these notices
    // (and any other `EventMsg::Warning`, e.g. model-reroute warnings) were silently dropped.

    #[test]
    fn v0146_skill_catalog_warning_is_captured_on_turn() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-08-01T10-00-00-v0146skills.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-08-01T10:00:00Z","type":"session_meta","payload":{"id":"v0146-skills-session","timestamp":"2026-08-01T10:00:00Z","cwd":"/project","cli_version":"0.146.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-08-01T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-08-01T10:00:02Z","type":"event_msg","payload":{"type":"warning","message":"Skill catalog exceeded its context budget; 3 additional skills omitted."}}"#,
                r#"{"timestamp":"2026-08-01T10:00:03Z","type":"event_msg","payload":{"type":"agent_message","message":"Done.","phase":"final_answer"}}"#,
                r#"{"timestamp":"2026-08-01T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1754043604.0}}"#,
                r#"{"timestamp":"2026-08-01T10:00:05Z","type":"session_end","payload":{}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0146-skills-session");
        assert_eq!(session.turns.len(), 1);
        assert_eq!(
            session.turns[0].warnings,
            vec![
                "Skill catalog exceeded its context budget; 3 additional skills omitted."
                    .to_string()
            ],
            "skill catalog budget warnings must be captured, not silently dropped"
        );
        assert_eq!(
            session.turns[0].status,
            TurnStatus::Complete,
            "a warning must not itself mark the turn as errored"
        );
        assert!(!session.is_ongoing);
    }
}
