use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::BufRead;
use std::path::Path;
use std::time::SystemTime;

use super::compression::open_session_reader;
use super::entry::{extract_session_id, RawEntry};
use super::spawn::parse_spawn_agent_output;

/// Lightweight session info for the picker list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexSessionInfo {
    pub id: String,
    pub path: String,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub originator: Option<String>,
    pub model: Option<String>,
    pub cli_version: Option<String>,
    pub thread_name: Option<String>,
    pub turn_count: u32,
    pub start_time: String,
    pub end_time: Option<String>,
    pub total_tokens: Option<u64>,
    pub is_ongoing: bool,
    /// true when session_meta.source.subagent is set (system-spawned: review, memory_consolidation)
    pub is_external_worker: bool,
    /// true when this session's id appears in another session's spawned_worker_ids (inline collab worker)
    pub is_inline_worker: bool,
    pub worker_nickname: Option<String>,
    pub worker_role: Option<String>,
    pub spawned_worker_ids: Vec<String>,
    /// "YYYY/MM/DD" derived from the file path
    pub date_group: String,
    /// Optional AI-generated title from external agent sessions (Codex v0.128.0+)
    pub ai_title: Option<String>,
    /// true when the session was started via `codex remote-control` (Codex v0.130.0+, PR #21424).
    /// Detected from originator == "remote-control" or source == "remote-control" in session_meta.
    pub is_headless: bool,
    /// true when the session has been archived via `codex archive` (Codex v0.136.0+).
    /// A trailing `session_archived` event sets this; a subsequent `session_unarchived` clears it.
    pub is_archived: bool,
    /// Approval mode from session_meta.ask_for_approval (Codex v0.144.0+, PR #30482).
    /// Known values: "suggest", "auto-edit", "full-auto", "writes" (new in v0.144.0).
    /// Checked against upstream (PR #36373, #37057): v0.147.0's `--approve-for-me`
    /// flag and v0.146.1's cyber-model auto-review defaults do NOT add a new value
    /// here — both route through the existing "on-request" policy plus a separate
    /// auto-review-reviewer setting, not a new ask_for_approval string. Parsing
    /// below is already a permissive passthrough, so no new value is expected.
    /// Null for sessions predating v0.144.0 or when the field is absent.
    pub approval_mode: Option<String>,
    /// Codex v0.146.0 (PRs #34621, #35220): thread ID this rollout's paginated history
    /// inherits from, read from session_meta.payload.history_base.thread_id. Distinct from
    /// the per-turn `forked_from_thread_id` and compaction `lineage_id` fields — this one
    /// marks a whole rollout file as a continuation of another paginated thread's history.
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

/// Scan a sessions directory recursively for all rollout-*.jsonl files.
/// Returns CodexSessionInfo sorted by filename descending (newest first).
///
/// Note on Codex v0.146.0 ephemeral/temporary forks (issue #210): the app-server keeps
/// ephemeral forked threads "pathless" and never materializes them to a rollout file, so
/// they cannot appear here and need no explicit filtering — this scanner only ever sees
/// files that were actually written to `sessions_dir`.
///
/// Note on Codex v0.147.0 persistent thread sections (issue #220): PRs #35722/#36007/
/// #36380 added `threadSection/*` app-server methods backed by new SQLite migrations
/// (`codex-rs/state/migrations/0045_threads_section.sql`, `0046_threads_section_order.sql`).
/// Reading the upstream source (`codex-rs/thread-store/src/local/read_thread.rs`,
/// `state_db.rs`) confirms a thread's `section`/`section_position`/`section_entered_at`
/// are always joined from local SQLite state at read time — `session_meta` itself only
/// contributes `id`/`forked_from_id`/`parent_thread_id`/`history_mode` to that join, and
/// `codex-rs/rollout/src/recorder.rs`'s former `is_pinned` parameter was renamed to
/// `section` but stayed `None` at every rollout-facing call site. No `section*` field is
/// ever written to the rollout JSONL, so there is nothing here to parse; a user's manual
/// section organization in the Codex TUI simply isn't reflected in codex-trace's own
/// `date_group` grouping. Known limitation, not a parsing gap.
///
/// Note on Codex v0.147.0 incremental transcript browsing (issue #220): PRs #36948/#36950
/// paginate transcript *rendering* inside the TUI (`codex-rs/tui/src/app/history_pagination.rs`,
/// `pager_overlay.rs`) on top of the app-server pagination protocol v0.146.0 already
/// introduced (see the `history_base_thread_id` field above, issue #210). Neither PR
/// touches `codex-rs/rollout` or adds any rollout field — confirmed by diffing both PRs'
/// changed files — so this scanner needs no changes for it.
pub fn discover_sessions(sessions_dir: &Path) -> Result<Vec<CodexSessionInfo>, String> {
    if !sessions_dir.exists() {
        return Ok(Vec::new());
    }

    let mut infos: Vec<CodexSessionInfo> = Vec::new();
    collect_jsonl_files(sessions_dir, &mut infos)?;

    // Sort newest first (ISO timestamp in filename is lexicographically sortable)
    infos.sort_by(|a, b| {
        let fa = Path::new(&a.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let fb = Path::new(&b.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        fb.cmp(fa)
    });

    // Second pass: mark inline workers — sessions whose id appears in any parent's spawned_worker_ids.
    use std::collections::HashSet;
    let inline_worker_ids: HashSet<String> = infos
        .iter()
        .flat_map(|s| s.spawned_worker_ids.iter().cloned())
        .collect();
    for info in &mut infos {
        if inline_worker_ids.contains(&info.id) {
            info.is_inline_worker = true;
        }
    }

    Ok(infos)
}

fn collect_jsonl_files(dir: &Path, infos: &mut Vec<CodexSessionInfo>) -> Result<(), String> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_files(&path, infos)?;
            continue;
        }

        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !name.starts_with("rollout-") {
            continue;
        }

        // Codex's background rollout compression worker (codex-rs/rollout/src/compression.rs,
        // present since v0.137.0 and still active as of v0.146.0's #34566) replaces a cold
        // `rollout-*.jsonl` file with a `rollout-*.jsonl.zst` sibling and removes the plain
        // file. Without recognizing the compressed suffix, discovery silently drops any
        // session Codex has compressed — it just vanishes from the list with no indication.
        if let Some(plain_name) = name
            .strip_suffix(".jsonl.zst")
            .map(|s| format!("{s}.jsonl"))
        {
            // A plain sibling briefly coexists with its compressed copy while Codex
            // materializes it back for append; skip the compressed copy in that case so
            // the session isn't counted twice.
            let plain_sibling = path.with_file_name(plain_name);
            if plain_sibling.exists() {
                continue;
            }
        } else if !name.ends_with(".jsonl") {
            continue;
        }

        if let Some(info) = scan_session_file(&path) {
            infos.push(info);
        }
    }

    Ok(())
}

/// Extract date group (YYYY/MM/DD) from the file path.
/// Path structure: .../sessions/YYYY/MM/DD/rollout-*.jsonl
fn date_group_from_path(path: &Path) -> String {
    path.parent()
        .and_then(|p| {
            let dd = p.file_name()?.to_str()?;
            let mm = p.parent()?.file_name()?.to_str()?;
            let yyyy = p.parent()?.parent()?.file_name()?.to_str()?;
            Some(format!("{yyyy}/{mm}/{dd}"))
        })
        .unwrap_or_default()
}

/// Quickly scan a JSONL file for session metadata without full parsing.
///
/// Streams the file line-by-line (decompressing zstd transparently) so peak
/// memory stays bounded to a single line — session files can be hundreds of
/// megabytes, and slurping every file into memory during discovery spiked RSS.
fn scan_session_file(path: &Path) -> Option<CodexSessionInfo> {
    let reader = open_session_reader(path).ok()?;
    let mut lines = reader
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty());

    let first_line = lines.next()?;
    let first: Value = serde_json::from_str(&first_line).ok()?;

    // Skip state placeholders
    if first.get("record_type").and_then(|t| t.as_str()) == Some("state") {
        return None;
    }

    let entry = RawEntry::parse(&first_line)?;
    let payload = &entry.payload;
    let raw = &entry.raw;

    let (
        id,
        start_time,
        cwd,
        originator,
        cli_version,
        git_branch,
        _instructions,
        is_external_worker,
        is_headless,
        worker_nickname,
        worker_role,
        ai_title,
        meta_archived,
        approval_mode,
        history_base_thread_id,
        forked_from_thread_id,
    ) = match entry.entry_type.as_str() {
        "session_meta" => {
            let id = extract_session_id(payload);
            let start_time = str_field(payload, "timestamp");
            let cwd = opt_str(payload, "cwd");
            let originator = opt_str(payload, "originator");
            let cli_version = opt_str(payload, "cli_version");
            let git_branch = payload
                .get("git")
                .and_then(|g| g.get("branch"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let instructions: Option<String> = None; // not needed for picker
                                                     // source.subagent present ⟹ this is a system-spawned external worker
            let is_external_worker = payload
                .get("source")
                .and_then(|s| s.get("subagent"))
                .is_some();
            // Codex v0.130.0 (PR #21424): `codex remote-control` starts headless app-server
            // sessions. The originator or source field is set to "remote-control" for these.
            let is_headless = payload
                .get("originator")
                .and_then(|v| v.as_str())
                .map(|s| s == "remote-control")
                .unwrap_or(false)
                || payload
                    .get("source")
                    .and_then(|v| v.as_str())
                    .map(|s| s == "remote-control")
                    .unwrap_or(false);
            let (worker_nickname, worker_role) = worker_metadata(payload);
            let ai_title = payload
                .get("ai-title")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            // Codex v0.136.0: session_meta may carry archived: true when the session
            // was archived before this scan (e.g. archived then re-opened in another run).
            let meta_archived = payload
                .get("archived")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // Codex v0.144.0 (PR #30482): approval mode captured in session_meta.
            let approval_mode = payload
                .get("ask_for_approval")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            // Codex v0.146.0 (PRs #34621, #35220): a paginated rollout continuing another
            // thread's history carries session_meta.history_base.thread_id pointing at the
            // source rollout. This is the lineage-chain marker for session-tree reconstruction
            // across paginated forks — separate from the per-turn forked_from_thread_id.
            let history_base_thread_id = payload
                .get("history_base")
                .and_then(|b| b.get("thread_id"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            // Codex v0.148.0 (PR #37367, issue #238): session_meta.forked_from_id is the
            // app-server's SessionConfiguredEvent.forked_from_id, persisted verbatim into the
            // rollout. Verified against codex-rs/rollout/src/recorder.rs and
            // codex-rs/protocol/src/protocol.rs's SessionMeta struct — real signal for any
            // forked or fork-descended thread, including codex exec fork/resume sessions.
            let forked_from_thread_id = payload
                .get("forked_from_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            (
                id,
                start_time,
                cwd,
                originator,
                cli_version,
                git_branch,
                instructions,
                is_external_worker,
                is_headless,
                worker_nickname,
                worker_role,
                ai_title,
                meta_archived,
                approval_mode,
                history_base_thread_id,
                forked_from_thread_id,
            )
        }
        "session_meta_root" => {
            let id = str_field(raw, "id");
            let start_time = str_field(raw, "timestamp");
            let git_branch = raw
                .get("git")
                .and_then(|g| g.get("branch"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            (
                id, start_time, None, None, None, git_branch, None, false, false, None, None, None,
                false, None, None, None,
            )
        }
        _ => return None,
    };

    if id.is_empty() {
        return None;
    }

    // Quick scan remaining lines for turn count, model, thread_name, tokens, end_time
    let mut turn_count: u32 = 0;
    let mut model: Option<String> = None;
    let mut thread_name: Option<String> = None;
    let mut total_tokens: Option<u64> = None;
    let mut end_time: Option<String> = None;
    let mut spawned_worker_ids: Vec<String> = Vec::new();
    let mut pending_spawn_call_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut is_ongoing = true;
    let mut has_session_end = false;
    // Codex v0.136.0: track archived state; initialised from session_meta.archived.
    let mut is_archived = meta_archived;

    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let t = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match t {
            "session_end" => {
                has_session_end = true;
                is_ongoing = false;
                if end_time.is_none() {
                    end_time = v
                        .get("timestamp")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
            }
            // Codex v0.136.0: archive/unarchive commands append these events.
            // The last event wins so a session can be toggled multiple times.
            "session_archived" => is_archived = true,
            "session_unarchived" => is_archived = false,
            "event_msg" => {
                let pt = v
                    .get("payload")
                    .and_then(|p| p.get("type"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                match pt {
                    "task_started" => {
                        turn_count += 1;
                        if !has_session_end {
                            is_ongoing = true;
                        }
                        end_time = None;
                    }
                    "user_message" if turn_count == 0 => {
                        turn_count += 1;
                        if !has_session_end {
                            is_ongoing = true;
                        }
                        end_time = None;
                    }
                    "task_complete" => {
                        is_ongoing = false;
                        let payload = v.get("payload").unwrap_or(&Value::Null);
                        end_time = payload
                            .get("completed_at")
                            .and_then(|v| v.as_f64())
                            .map(|ts| {
                                use chrono::{DateTime, Utc};
                                DateTime::<Utc>::from_timestamp(ts as i64, 0)
                                    .map(|dt| dt.to_rfc3339())
                                    .unwrap_or_default()
                            })
                            .or_else(|| {
                                v.get("timestamp")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string())
                            });
                        // Codex v0.128.0: task_complete may carry prompt_tokens/completion_tokens/total_tokens.
                        // Use as fallback when no token_count event was seen.
                        if total_tokens.is_none() {
                            total_tokens = payload
                                .get("total_tokens")
                                .and_then(|v| v.as_u64())
                                .or_else(|| {
                                    let p =
                                        payload.get("prompt_tokens").and_then(|v| v.as_u64())?;
                                    let c = payload
                                        .get("completion_tokens")
                                        .and_then(|v| v.as_u64())?;
                                    Some(p + c)
                                });
                        }
                    }
                    "turn_aborted" => {
                        is_ongoing = false;
                        end_time = v
                            .get("timestamp")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                    }
                    "token_budget_abort" | "inference_stream_cancelled" => {
                        // v0.142.0+: budget exhausted or inference cancelled — turn is done.
                        // v0.143.0 reliably preserves these on disk during shutdown.
                        is_ongoing = false;
                        end_time = v
                            .get("timestamp")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                    }
                    "token_count" => {
                        if let Some(info) = v
                            .get("payload")
                            .and_then(|p| p.get("info"))
                            .filter(|v| !v.is_null())
                        {
                            if let Some(ttu) = info.get("total_token_usage") {
                                total_tokens = ttu.get("total_tokens").and_then(|v| v.as_u64());
                            }
                        }
                    }
                    "thread_name_updated" => {
                        thread_name = v
                            .get("payload")
                            .and_then(|p| p.get("thread_name"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                    }
                    "collab_agent_spawn_end" => {
                        // v0.131.0+ (PR #22268): field renamed new_thread_id → new_session_id
                        if let Some(new_id) = v
                            .get("payload")
                            .and_then(|p| {
                                p.get("new_thread_id").or_else(|| p.get("new_session_id"))
                            })
                            .and_then(|v| v.as_str())
                        {
                            push_unique(&mut spawned_worker_ids, new_id.to_string());
                        }
                    }
                    _ => {}
                }
            }
            "response_item" => {
                let payload = v.get("payload").unwrap_or(&Value::Null);
                match payload.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                    "function_call"
                        if payload.get("name").and_then(|v| v.as_str()) == Some("spawn_agent") =>
                    {
                        if let Some(call_id) = payload.get("call_id").and_then(|v| v.as_str()) {
                            pending_spawn_call_ids.insert(call_id.to_string());
                        }
                    }
                    "function_call_output" => {
                        let Some(call_id) = payload.get("call_id").and_then(|v| v.as_str()) else {
                            continue;
                        };
                        if !pending_spawn_call_ids.contains(call_id) {
                            continue;
                        }
                        if let Some(output) = output_text(payload) {
                            if let Some(spawn) = parse_spawn_agent_output(&output) {
                                push_unique(&mut spawned_worker_ids, spawn.agent_id);
                            }
                        }
                    }
                    _ => {}
                }
            }
            "turn_context" => {
                // Always overwrite — spec says "most recent turn_context.payload.model"
                let m = v
                    .get("payload")
                    .and_then(|p| p.get("model"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if m.is_some() {
                    model = m;
                }
            }
            _ => {}
        }
    }

    // Also count user_message events for old format (no task_started)
    // We already handle that above by incrementing on first "user_message"
    // For accuracy, re-scan and count task_started events specifically
    // (already done in the loop above — task_started increments turn_count)

    // Sessions with no turns have no active task — not ongoing regardless of event stream.
    if turn_count == 0 {
        is_ongoing = false;
    }

    // Validate with file mtime: sessions last modified more than 60 seconds ago
    // cannot be actively processing a turn, regardless of missing task_complete events.
    // Many older CLI versions didn't emit task_complete, causing false positives otherwise.
    if is_ongoing && !has_session_end {
        const ONGOING_THRESHOLD_SECS: u64 = 60;
        if let Ok(metadata) = fs::metadata(path) {
            if let Ok(modified) = metadata.modified() {
                if let Ok(elapsed) = SystemTime::now().duration_since(modified) {
                    if elapsed.as_secs() > ONGOING_THRESHOLD_SECS {
                        is_ongoing = false;
                    }
                }
            }
        }
    }

    let date_group = date_group_from_path(path);

    Some(CodexSessionInfo {
        id,
        path: path.to_string_lossy().to_string(),
        cwd,
        git_branch,
        originator,
        model,
        cli_version,
        thread_name,
        turn_count,
        start_time,
        end_time,
        total_tokens,
        is_ongoing,
        is_external_worker,
        is_inline_worker: false, // set by discover_sessions second pass
        is_headless,
        is_archived,
        worker_nickname,
        worker_role,
        spawned_worker_ids,
        date_group,
        ai_title,
        approval_mode,
        history_base_thread_id,
        forked_from_thread_id,
    })
}

fn worker_metadata(payload: &Value) -> (Option<String>, Option<String>) {
    let thread_spawn = payload
        .get("source")
        .and_then(|source| source.get("subagent"))
        .and_then(|subagent| subagent.get("thread_spawn"));

    let nickname = thread_spawn
        .and_then(|spawn| opt_str(spawn, "agent_nickname").or_else(|| opt_str(spawn, "nickname")));
    let role = thread_spawn
        .and_then(|spawn| opt_str(spawn, "agent_role").or_else(|| opt_str(spawn, "agent_type")));

    (nickname, role)
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

fn output_text(payload: &Value) -> Option<String> {
    match payload.get("output") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(arr)) => Some(
            arr.iter()
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => None,
    }
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn discover_sessions_reads_id_from_session_id_field() {
        // v0.129.0+ PR #20437: session_id field in session_meta payload
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/05/07");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-05-07T00-00-00-newsessid.jsonl");
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
        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions.iter().find(|s| s.id == "new-sess-id").unwrap();
        assert_eq!(session.id, "new-sess-id");
    }

    #[test]
    fn discover_sessions_reads_id_from_thread_session_id() {
        // v0.129.0+ PR #21336: sessionId moved onto Thread object
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/05/07");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-05-07T00-01-00-threadsessid.jsonl");
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
        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions.iter().find(|s| s.id == "thread-sess-id").unwrap();
        assert_eq!(session.id, "thread-sess-id");
    }

    #[test]
    fn date_group_from_path_test() {
        let path = PathBuf::from("/home/user/.codex/sessions/2026/04/25/rollout-abc.jsonl");
        let dg = date_group_from_path(&path);
        assert_eq!(dg, "2026/04/25");
    }

    #[test]
    fn discover_sessions_links_sdk_spawn_agent_output() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/04/27");
        std::fs::create_dir_all(&day_dir).unwrap();

        let parent_path = day_dir.join("rollout-2026-04-27T16-50-45-019dcd46-parent.jsonl");
        std::fs::write(
            &parent_path,
            [
                r#"{"timestamp":"2026-04-27T04:50:45Z","type":"session_meta","payload":{"id":"parent","timestamp":"2026-04-27T04:50:45Z","source":"exec"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:02Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"message\":\"Collect evidence\"}","call_id":"call_spawn"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_spawn","output":"{\"agent_id\":\"worker\",\"nickname\":\"Parfit\"}"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777279924.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let child_path = day_dir.join("rollout-2026-04-27T16-52-43-worker.jsonl");
        std::fs::write(
            &child_path,
            r#"{"timestamp":"2026-04-27T04:52:43Z","type":"session_meta","payload":{"id":"worker","timestamp":"2026-04-27T04:52:43Z","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent","depth":1,"agent_nickname":"Parfit","agent_role":"worker"}}}}}"#,
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let parent = sessions.iter().find(|s| s.id == "parent").unwrap();
        let child = sessions.iter().find(|s| s.id == "worker").unwrap();

        assert_eq!(parent.spawned_worker_ids, vec!["worker"]);
        assert!(child.is_external_worker);
        assert!(child.is_inline_worker);
        assert_eq!(child.worker_nickname.as_deref(), Some("Parfit"));
        assert_eq!(child.worker_role.as_deref(), Some("worker"));
    }

    #[test]
    fn discover_sessions_links_collab_spawn_end_event() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/04/16");
        std::fs::create_dir_all(&day_dir).unwrap();

        let parent_path = day_dir.join("rollout-2026-04-16T11-38-08-019d9382-parent.jsonl");
        std::fs::write(
            &parent_path,
            [
                r#"{"timestamp":"2026-04-16T11:38:08Z","type":"session_meta","payload":{"id":"parent","timestamp":"2026-04-16T11:38:08Z","source":"cli"}}"#,
                r#"{"timestamp":"2026-04-16T11:48:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-16T11:48:02Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"message\":\"Collect graph\"}","call_id":"call_spawn"}}"#,
                r#"{"timestamp":"2026-04-16T11:48:03Z","type":"event_msg","payload":{"type":"collab_agent_spawn_end","call_id":"call_spawn","sender_thread_id":"parent","new_thread_id":"worker","new_agent_nickname":"Noether","new_agent_role":"worker","prompt":"Collect graph","status":"pending_init"}}"#,
                r#"{"timestamp":"2026-04-16T11:48:04Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_spawn","output":"{\"agent_id\":\"worker\",\"nickname\":\"Noether\"}"}}"#,
                r#"{"timestamp":"2026-04-16T11:48:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1776335285.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let child_path = day_dir.join("rollout-2026-04-16T11-48-09-worker.jsonl");
        std::fs::write(
            &child_path,
            r#"{"timestamp":"2026-04-16T11:48:09Z","type":"session_meta","payload":{"id":"worker","timestamp":"2026-04-16T11:48:09Z","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent","depth":1,"agent_nickname":"Noether","agent_role":"worker"}}}}}"#,
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let parent = sessions.iter().find(|s| s.id == "parent").unwrap();
        let child = sessions.iter().find(|s| s.id == "worker").unwrap();

        assert_eq!(parent.spawned_worker_ids, vec!["worker"]);
        assert!(child.is_external_worker);
        assert!(child.is_inline_worker);
        assert_eq!(child.worker_nickname.as_deref(), Some("Noether"));
        assert_eq!(child.worker_role.as_deref(), Some("worker"));
    }

    #[test]
    fn discover_sessions_marks_later_started_turn_ongoing_after_completed_turn() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/04/27");
        std::fs::create_dir_all(&day_dir).unwrap();

        let session_path = day_dir.join("rollout-2026-04-27T17-10-00-active.jsonl");
        std::fs::write(
            &session_path,
            [
                r#"{"timestamp":"2026-04-27T05:10:00Z","type":"session_meta","payload":{"id":"active","timestamp":"2026-04-27T05:10:00Z","source":"cli"}}"#,
                r#"{"timestamp":"2026-04-27T05:10:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-27T05:10:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777281002.0}}"#,
                r#"{"timestamp":"2026-04-27T05:10:03Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2"}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions.iter().find(|s| s.id == "active").unwrap();

        assert_eq!(session.turn_count, 2);
        assert!(session.is_ongoing);
        assert_eq!(session.end_time, None);
    }

    #[test]
    fn discover_sessions_marks_completed_latest_turn_not_ongoing() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/04/27");
        std::fs::create_dir_all(&day_dir).unwrap();

        let session_path = day_dir.join("rollout-2026-04-27T17-20-00-complete.jsonl");
        std::fs::write(
            &session_path,
            [
                r#"{"timestamp":"2026-04-27T05:20:00Z","type":"session_meta","payload":{"id":"complete","timestamp":"2026-04-27T05:20:00Z","source":"cli"}}"#,
                r#"{"timestamp":"2026-04-27T05:20:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-27T05:20:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777281602.0}}"#,
                r#"{"timestamp":"2026-04-27T05:20:03Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2"}}"#,
                r#"{"timestamp":"2026-04-27T05:20:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-2","completed_at":1777281604.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions.iter().find(|s| s.id == "complete").unwrap();

        assert_eq!(session.turn_count, 2);
        assert!(!session.is_ongoing);
        assert!(session.end_time.is_some());
    }

    #[test]
    fn discover_sessions_reads_ai_title() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/04/30");
        std::fs::create_dir_all(&day_dir).unwrap();

        let session_path = day_dir.join("rollout-2026-04-30T10-00-00-aititle.jsonl");
        std::fs::write(
            &session_path,
            [
                r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"aititle-session","timestamp":"2026-04-30T10:00:00Z","ai-title":"Refactor the auth module"}}"#,
                r#"{"timestamp":"2026-04-30T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-30T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746007202.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions.iter().find(|s| s.id == "aititle-session").unwrap();
        assert_eq!(
            session.ai_title.as_deref(),
            Some("Refactor the auth module")
        );
    }

    #[test]
    fn discover_sessions_session_end_marks_not_ongoing() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/04/30");
        std::fs::create_dir_all(&day_dir).unwrap();

        let session_path = day_dir.join("rollout-2026-04-30T10-01-00-ended.jsonl");
        std::fs::write(
            &session_path,
            [
                r#"{"timestamp":"2026-04-30T10:01:00Z","type":"session_meta","payload":{"id":"ended-session","timestamp":"2026-04-30T10:01:00Z"}}"#,
                r#"{"timestamp":"2026-04-30T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-30T10:01:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746007262.0}}"#,
                r#"{"timestamp":"2026-04-30T10:01:03Z","type":"session_end","payload":{}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions.iter().find(|s| s.id == "ended-session").unwrap();
        assert!(!session.is_ongoing);
    }

    #[test]
    fn discover_sessions_session_end_overrides_ongoing_turn() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/04/30");
        std::fs::create_dir_all(&day_dir).unwrap();

        let session_path = day_dir.join("rollout-2026-04-30T10-02-00-endmarker.jsonl");
        std::fs::write(
            &session_path,
            [
                r#"{"timestamp":"2026-04-30T10:02:00Z","type":"session_meta","payload":{"id":"endmarker-session","timestamp":"2026-04-30T10:02:00Z"}}"#,
                r#"{"timestamp":"2026-04-30T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-30T10:02:02Z","type":"session_end","payload":{}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "endmarker-session")
            .unwrap();
        assert!(!session.is_ongoing);
    }

    // Codex v0.130.0 (PR #21424): `codex remote-control` starts headless app-server sessions.
    // Sessions initiated this way carry originator == "remote-control" in session_meta.

    #[test]
    fn discover_sessions_detects_remote_control_via_originator() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/05/08");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-05-08T10-00-00-headless.jsonl");
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
        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "headless-session")
            .unwrap();
        assert!(
            session.is_headless,
            "originator:remote-control must set is_headless"
        );
        assert!(!session.is_external_worker);
    }

    #[test]
    fn discover_sessions_detects_remote_control_via_source_string() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/05/08");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-05-08T10-01-00-headless2.jsonl");
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
        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "headless-session-2")
            .unwrap();
        assert!(
            session.is_headless,
            "source:remote-control must set is_headless"
        );
    }

    #[test]
    fn discover_sessions_regular_session_is_not_headless() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/05/08");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-05-08T10-02-00-regular.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-08T10:02:00Z","type":"session_meta","payload":{"id":"regular-session","timestamp":"2026-05-08T10:02:00Z","source":"exec","cli_version":"0.130.0"}}"#,
                r#"{"timestamp":"2026-05-08T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-08T10:02:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746698522.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions.iter().find(|s| s.id == "regular-session").unwrap();
        assert!(!session.is_headless, "exec session must not be headless");
    }

    #[test]
    fn discover_sessions_subagent_source_is_not_headless() {
        // source.subagent (object) must not be confused with source == "remote-control" (string).
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/05/08");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-05-08T10-03-00-subagent.jsonl");
        std::fs::write(
            &path,
            r#"{"timestamp":"2026-05-08T10:03:00Z","type":"session_meta","payload":{"id":"subagent-session","timestamp":"2026-05-08T10:03:00Z","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent","depth":1}}}}}"#,
        )
        .unwrap();
        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "subagent-session")
            .unwrap();
        assert!(!session.is_headless, "subagent source must not be headless");
        assert!(
            session.is_external_worker,
            "subagent source must be external_worker"
        );
    }

    #[test]
    fn reads_total_tokens_from_task_complete_v0128() {
        // Codex v0.128.0 adds prompt_tokens/completion_tokens/total_tokens to task_complete.
        // The discover scanner should use these as a fallback when no token_count event exists.
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/04/30");
        std::fs::create_dir_all(&day_dir).unwrap();
        let session_path = day_dir.join("rollout-2026-04-30T10-00-00-s1.jsonl");
        std::fs::write(
            &session_path,
            [
                r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"s1","timestamp":"2026-04-30T10:00:00Z"}}"#,
                r#"{"timestamp":"2026-04-30T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-30T10:00:10Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746007210.0,"prompt_tokens":1500,"completion_tokens":300,"total_tokens":1800}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions.iter().find(|s| s.id == "s1").unwrap();

        assert_eq!(session.total_tokens, Some(1800));
    }

    // Codex v0.131.0 (PR #22268): collab_agent_spawn_end event renamed new_thread_id → new_session_id.
    // Verify the discover scanner reads new_session_id when new_thread_id is absent.
    #[test]
    fn discover_sessions_links_collab_spawn_end_event_v0131_new_session_id() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/05/18");
        std::fs::create_dir_all(&day_dir).unwrap();

        let parent_path = day_dir.join("rollout-2026-05-18T10-00-00-parent-v131.jsonl");
        std::fs::write(
            &parent_path,
            [
                r#"{"timestamp":"2026-05-18T10:00:00Z","type":"session_meta","payload":{"id":"parent-v131","timestamp":"2026-05-18T10:00:00Z","cli_version":"0.131.0","source":"cli"}}"#,
                r#"{"timestamp":"2026-05-18T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-18T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"message\":\"Gather data\"}","call_id":"call_spawn_v131"}}"#,
                r#"{"timestamp":"2026-05-18T10:00:03Z","type":"event_msg","payload":{"type":"collab_agent_spawn_end","call_id":"call_spawn_v131","sender_session_id":"parent-v131","new_session_id":"worker-v131","new_agent_nickname":"Hypatia","new_agent_role":"worker","prompt":"Gather data","status":"pending_init"}}"#,
                r#"{"timestamp":"2026-05-18T10:00:04Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_spawn_v131","output":"{\"agent_id\":\"worker-v131\",\"nickname\":\"Hypatia\"}"}}"#,
                r#"{"timestamp":"2026-05-18T10:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747562405.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let child_path = day_dir.join("rollout-2026-05-18T10-00-09-worker-v131.jsonl");
        std::fs::write(
            &child_path,
            r#"{"timestamp":"2026-05-18T10:00:09Z","type":"session_meta","payload":{"id":"worker-v131","timestamp":"2026-05-18T10:00:09Z","cli_version":"0.131.0","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent-v131","depth":1,"agent_nickname":"Hypatia","agent_role":"worker"}}}}}"#,
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let parent = sessions.iter().find(|s| s.id == "parent-v131").unwrap();
        let child = sessions.iter().find(|s| s.id == "worker-v131").unwrap();

        assert_eq!(parent.spawned_worker_ids, vec!["worker-v131"]);
        assert!(child.is_external_worker);
        assert!(child.is_inline_worker);
        assert_eq!(child.worker_nickname.as_deref(), Some("Hypatia"));
        assert_eq!(child.worker_role.as_deref(), Some("worker"));
    }

    // Codex v0.131.0 (PRs #22594, #22647, #22724): profile-v2 layered config format.
    //
    // codex-trace reads only JSONL session files — never Codex TOML config files. The
    // profile-v2 changes affect what appears in session_meta: a `profile` field may name
    // the active profile; `instructions_file` is gone from config so instructions arrive
    // via `base_instructions.text` or are absent entirely. The discover scanner must handle
    // both cases without panicking.
    //
    // Note: As of Codex v0.134.0 (PRs #23883, #24051, #24055, #24059), --profile-v2 was
    // renamed to --profile and all legacy profile v1 support was removed. See the v0134_*
    // tests below for the corresponding v0.134.0 verification.

    #[test]
    fn discover_sessions_v0131_profile_v2_session_discovered_correctly() {
        // session_meta with profile-v2 `profile` field (--profile-v2 renamed to --profile
        // in v0.134.0): discover scanner must not panic and must populate cli_version
        // correctly from the payload.
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/05/18");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-05-18T10-00-00-profilev2.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-18T10:00:00Z","type":"session_meta","payload":{"id":"v0131-disc-profile","timestamp":"2026-05-18T10:00:00Z","cwd":"/home/user","cli_version":"0.131.0","model_provider":"openai","profile":"work","base_instructions":{"text":"You are helpful."}}}"#,
                r#"{"timestamp":"2026-05-18T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-18T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747562402.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "v0131-disc-profile")
            .unwrap();
        assert_eq!(session.cli_version.as_deref(), Some("0.131.0"));
        assert_eq!(session.turn_count, 1);
        assert!(!session.is_ongoing);
    }

    #[test]
    fn discover_sessions_v0131_no_instructions_discovered_correctly() {
        // sessions from v0.131.0 without instructions (instructions_file removed) must be
        // discovered and indexed normally — the absent field has no effect on discovery.
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/05/18");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-05-18T10-01-00-noinstr.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-18T10:01:00Z","type":"session_meta","payload":{"id":"v0131-disc-noinstr","timestamp":"2026-05-18T10:01:00Z","cwd":"/home/user","cli_version":"0.131.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-05-18T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-18T10:01:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747562462.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "v0131-disc-noinstr")
            .unwrap();
        assert_eq!(session.cli_version.as_deref(), Some("0.131.0"));
        assert_eq!(session.turn_count, 1);
        assert!(!session.is_ongoing);
    }

    // Codex v0.134.0 (PRs #23883, #24051, #24055, #24059): --profile-v2 renamed to --profile;
    // legacy profile v1 support removed entirely.
    //
    // codex-trace reads JSONL session files only — it never invokes `codex` or reads Codex
    // TOML config. Sessions from v0.134.0+ carry the same `profile` field in session_meta
    // as v0.131.0+ sessions. The discover scanner is unaffected; these tests confirm
    // v0.134.0 sessions are discovered and indexed correctly.

    #[test]
    fn discover_sessions_v0134_profile_session_discovered_correctly() {
        // session_meta with --profile active (flag renamed from --profile-v2 in v0.134.0):
        // discover scanner must not panic and must populate cli_version correctly.
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/05/26");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-05-26T10-00-00-profile.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-26T10:00:00Z","type":"session_meta","payload":{"id":"v0134-disc-profile","timestamp":"2026-05-26T10:00:00Z","cwd":"/home/user","cli_version":"0.134.0","model_provider":"openai","profile":"work","base_instructions":{"text":"You are helpful."}}}"#,
                r#"{"timestamp":"2026-05-26T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-26T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748254802.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "v0134-disc-profile")
            .unwrap();
        assert_eq!(session.cli_version.as_deref(), Some("0.134.0"));
        assert_eq!(session.turn_count, 1);
        assert!(!session.is_ongoing);
    }

    #[test]
    fn discover_sessions_v0134_no_profile_discovered_correctly() {
        // v0.134.0 session without --profile: no `profile` field in session_meta.
        // The discover scanner must index the session normally — the absent field has no
        // effect on discovery (legacy profile v1 removal is transparent to codex-trace).
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/05/26");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-05-26T10-01-00-noprofile.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-26T10:01:00Z","type":"session_meta","payload":{"id":"v0134-disc-noprofile","timestamp":"2026-05-26T10:01:00Z","cwd":"/home/user","cli_version":"0.134.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-05-26T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-26T10:01:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748254862.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "v0134-disc-noprofile")
            .unwrap();
        assert_eq!(session.cli_version.as_deref(), Some("0.134.0"));
        assert_eq!(session.turn_count, 1);
        assert!(!session.is_ongoing);
    }

    // Codex v0.137.0 (PRs #25089, #25087): cold session rollout files are now stored
    // compressed with zstd. discover_sessions must detect and decompress them transparently.

    fn compress_zstd(data: &[u8]) -> Vec<u8> {
        zstd::encode_all(data, 3).expect("zstd compress failed")
    }

    #[test]
    fn discover_sessions_v0137_zstd_compressed_session() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/06/04");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-06-04T10-00-00-zstd.jsonl");
        let content = [
            r#"{"timestamp":"2026-06-04T10:00:00Z","type":"session_meta","payload":{"id":"v0137-disc-zstd","timestamp":"2026-06-04T10:00:00Z","cwd":"/project","cli_version":"0.137.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-04T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-04T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748995202.0}}"#,
        ]
        .join("\n");
        std::fs::write(&path, compress_zstd(content.as_bytes())).unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions.iter().find(|s| s.id == "v0137-disc-zstd").unwrap();
        assert_eq!(session.cli_version.as_deref(), Some("0.137.0"));
        assert_eq!(session.turn_count, 1);
        assert!(!session.is_ongoing);
    }

    // Codex v0.136.0: sessions can be archived via `codex archive`. The JSONL gains
    // a session_archived top-level entry; `codex unarchive` appends session_unarchived.
    // discover_sessions must expose is_archived correctly in all cases.

    #[test]
    fn discover_sessions_v0136_session_archived_event_sets_is_archived() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/06/01");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-06-01T10-00-00-archived.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-06-01T10:00:00Z","type":"session_meta","payload":{"id":"archived-session","timestamp":"2026-06-01T10:00:00Z","cwd":"/project","cli_version":"0.136.0"}}"#,
                r#"{"timestamp":"2026-06-01T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-06-01T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748772002.0}}"#,
                r#"{"timestamp":"2026-06-01T10:00:03Z","type":"session_archived"}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "archived-session")
            .unwrap();
        assert!(
            session.is_archived,
            "session_archived event must set is_archived"
        );
        assert!(!session.is_ongoing);
    }

    #[test]
    fn discover_sessions_v0136_session_unarchived_clears_is_archived() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/06/01");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-06-01T10-01-00-unarchived.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-06-01T10:01:00Z","type":"session_meta","payload":{"id":"unarchived-session","timestamp":"2026-06-01T10:01:00Z","cwd":"/project","cli_version":"0.136.0"}}"#,
                r#"{"timestamp":"2026-06-01T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-06-01T10:01:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748772062.0}}"#,
                r#"{"timestamp":"2026-06-01T10:01:03Z","type":"session_archived"}"#,
                r#"{"timestamp":"2026-06-01T10:01:04Z","type":"session_unarchived"}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "unarchived-session")
            .unwrap();
        assert!(
            !session.is_archived,
            "session_unarchived event must clear is_archived"
        );
    }

    #[test]
    fn discover_sessions_v0136_archived_flag_in_session_meta_payload() {
        // session_meta.archived = true covers the case where the meta is written with the
        // archived flag already set (e.g. a session archived in a previous codex run).
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/06/01");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-06-01T10-02-00-metaarchived.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-06-01T10:02:00Z","type":"session_meta","payload":{"id":"meta-archived-session","timestamp":"2026-06-01T10:02:00Z","cwd":"/project","cli_version":"0.136.0","archived":true}}"#,
                r#"{"timestamp":"2026-06-01T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-06-01T10:02:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748772122.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "meta-archived-session")
            .unwrap();
        assert!(
            session.is_archived,
            "archived:true in session_meta payload must set is_archived"
        );
    }

    #[test]
    fn discover_sessions_v0136_regular_session_is_not_archived() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/06/01");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-06-01T10-03-00-notarchived.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-06-01T10:03:00Z","type":"session_meta","payload":{"id":"not-archived-session","timestamp":"2026-06-01T10:03:00Z","cwd":"/project","cli_version":"0.136.0"}}"#,
                r#"{"timestamp":"2026-06-01T10:03:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-06-01T10:03:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748772182.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "not-archived-session")
            .unwrap();
        assert!(
            !session.is_archived,
            "session without archive events must not be archived"
        );
    }

    #[test]
    fn discover_sessions_v0137_plain_and_compressed_coexist() {
        // Mixed directory: some plain, some compressed — both must be discovered correctly.
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/06/04");
        std::fs::create_dir_all(&day_dir).unwrap();

        let plain_path = day_dir.join("rollout-2026-06-04T09-00-00-plain.jsonl");
        std::fs::write(
            &plain_path,
            [
                r#"{"timestamp":"2026-06-04T09:00:00Z","type":"session_meta","payload":{"id":"v0136-plain","timestamp":"2026-06-04T09:00:00Z","cli_version":"0.136.0"}}"#,
                r#"{"timestamp":"2026-06-04T09:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"t1"}}"#,
                r#"{"timestamp":"2026-06-04T09:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t1","completed_at":1748991602.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let compressed_path = day_dir.join("rollout-2026-06-04T10-00-00-compressed.jsonl");
        let compressed_content = [
            r#"{"timestamp":"2026-06-04T10:00:00Z","type":"session_meta","payload":{"id":"v0137-compressed","timestamp":"2026-06-04T10:00:00Z","cli_version":"0.137.0"}}"#,
            r#"{"timestamp":"2026-06-04T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"t1"}}"#,
            r#"{"timestamp":"2026-06-04T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t1","completed_at":1748995202.0}}"#,
        ]
        .join("\n");
        std::fs::write(
            &compressed_path,
            compress_zstd(compressed_content.as_bytes()),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        assert!(
            sessions.iter().any(|s| s.id == "v0136-plain"),
            "plain session must be found"
        );
        assert!(
            sessions.iter().any(|s| s.id == "v0137-compressed"),
            "compressed session must be found"
        );
    }

    // Issue #209 / Codex v0.146.0 (PR #34566 touches rollout compression cleanup):
    // Codex's background compression worker (codex-rs/rollout/src/compression.rs) replaces a
    // cold `rollout-*.jsonl` file with a `rollout-*.jsonl.zst` sibling and removes the plain
    // file — it does not rewrite zstd bytes into the original `.jsonl` name. discover_sessions
    // must recognize the `.jsonl.zst` suffix so a compressed session isn't silently dropped.

    #[test]
    fn discover_sessions_v0146_compressed_only_session_uses_real_zst_suffix() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/07/28");
        std::fs::create_dir_all(&day_dir).unwrap();

        // Real Codex compression appends `.zst` to the plain filename rather than
        // compressing in place under the original `.jsonl` name.
        let compressed_path = day_dir.join("rollout-2026-07-28T10-00-00-coldsession.jsonl.zst");
        let content = [
            r#"{"timestamp":"2026-07-28T10:00:00Z","type":"session_meta","payload":{"id":"v0146-cold","timestamp":"2026-07-28T10:00:00Z","cli_version":"0.146.0"}}"#,
            r#"{"timestamp":"2026-07-28T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"t1"}}"#,
            r#"{"timestamp":"2026-07-28T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t1","completed_at":1753696802.0}}"#,
        ]
        .join("\n");
        std::fs::write(&compressed_path, compress_zstd(content.as_bytes())).unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        assert!(
            sessions.iter().any(|s| s.id == "v0146-cold"),
            "session compressed to a `.jsonl.zst` sibling must still be discovered, not silently dropped"
        );
    }

    #[test]
    fn discover_sessions_v0146_compressed_sibling_of_existing_plain_not_double_counted() {
        // Codex briefly materializes a compressed rollout back to plain `.jsonl` for append
        // before removing the `.zst` copy. If both are momentarily present, the session must
        // be counted once, not twice.
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/07/28");
        std::fs::create_dir_all(&day_dir).unwrap();

        let base_name = "rollout-2026-07-28T11-00-00-transition.jsonl";
        let content = [
            r#"{"timestamp":"2026-07-28T11:00:00Z","type":"session_meta","payload":{"id":"v0146-transition","timestamp":"2026-07-28T11:00:00Z","cli_version":"0.146.0"}}"#,
            r#"{"timestamp":"2026-07-28T11:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"t1"}}"#,
            r#"{"timestamp":"2026-07-28T11:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t1","completed_at":1753700402.0}}"#,
        ]
        .join("\n");

        std::fs::write(day_dir.join(base_name), &content).unwrap();
        std::fs::write(
            day_dir.join(format!("{base_name}.zst")),
            compress_zstd(content.as_bytes()),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let matches: Vec<_> = sessions
            .iter()
            .filter(|s| s.id == "v0146-transition")
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "plain + compressed copies of the same rollout must be counted once, not twice"
        );
    }

    // Codex v0.142.0 (PR #29432, PR #29457): persistent log verbosity reduced.
    //
    // PR #29432: Responses WebSocket events are no longer logged on every message.
    // PR #29457: Noisy/duplicated telemetry targets are filtered from persistent logs.
    //
    // codex-trace reads session data from JSONL rollout files at ~/.codex/sessions/ — it does
    // not read persistent log files or WebSocket streams. Session reconstruction relies on
    // task_started/task_complete turn boundaries, response_item entries, and turn_context; none
    // of these depend on the per-WebSocket-message events or telemetry entries that v0.142.0
    // stops writing. discover_sessions must work correctly even when the event stream is sparser.

    #[test]
    fn discover_sessions_v0142_sparse_session_discovered_correctly() {
        // v0.142.0 session: no per-WebSocket-message event_msg entries (PR #29432 stops
        // writing them). Session reconstruction works purely from task_started, response_item,
        // and task_complete. discover_sessions must populate all fields correctly.
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/06/22");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-06-22T10-00-00-v0142.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-06-22T10:00:00Z","type":"session_meta","payload":{"id":"v0142-disc-session","timestamp":"2026-06-22T10:00:00Z","cwd":"/project","cli_version":"0.142.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-06-22T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-06-22T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
                r#"{"timestamp":"2026-06-22T10:00:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project"}}"#,
                r#"{"timestamp":"2026-06-22T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1782122404.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "v0142-disc-session")
            .expect("v0.142.0 session must be discovered");
        assert_eq!(session.cli_version.as_deref(), Some("0.142.0"));
        assert_eq!(session.cwd.as_deref(), Some("/project"));
        assert_eq!(session.turn_count, 1);
        assert!(!session.is_ongoing, "completed session must not be ongoing");
    }

    #[test]
    fn discover_sessions_v0142_responses_ws_events_in_old_format_do_not_corrupt_discovery() {
        // Pre-v0.142.0 sessions may contain per-WebSocket-message event_msg entries
        // (response.output_item.added, response.done, etc.) that v0.142.0 no longer writes.
        // discover_sessions must index these sessions correctly — unknown event_msg payload
        // types are ignored by the scanner's match arms, leaving turn_count intact.
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/06/22");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-06-22T10-01-00-v0142-ws.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-06-22T10:01:00Z","type":"session_meta","payload":{"id":"v0142-ws-disc","timestamp":"2026-06-22T10:01:00Z","cwd":"/project","cli_version":"0.142.0"}}"#,
                r#"{"timestamp":"2026-06-22T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                // WebSocket streaming events that v0.142.0 no longer logs — must be harmlessly skipped
                r#"{"timestamp":"2026-06-22T10:01:02Z","type":"event_msg","payload":{"type":"response.output_item.added","item":{"type":"message"}}}"#,
                r#"{"timestamp":"2026-06-22T10:01:03Z","type":"event_msg","payload":{"type":"response.done","response_id":"resp-1","status":"completed"}}"#,
                // Telemetry entry that v0.142.0 filters (PR #29457) — must also be skipped
                r#"{"timestamp":"2026-06-22T10:01:04Z","type":"event_msg","payload":{"type":"telemetry_flush","target":"metrics","batch_size":7}}"#,
                r#"{"timestamp":"2026-06-22T10:01:05Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Done"}}"#,
                r#"{"timestamp":"2026-06-22T10:01:06Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1782122466.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "v0142-ws-disc")
            .expect("session with WebSocket events must be discoverable");
        assert_eq!(session.cli_version.as_deref(), Some("0.142.0"));
        assert_eq!(
            session.turn_count, 1,
            "WebSocket events must not inflate turn_count"
        );
        assert!(!session.is_ongoing);
    }

    #[test]
    fn v0140_discover_sessions_with_secret_auth_storage_configuration_in_session_meta() {
        // Codex v0.140.0 (PRs #27504, #27535, #27539, #27541): CLI auth and MCP OAuth
        // credentials are now stored in an encrypted secret namespace. The new config option
        // `secret_auth_storage_configuration` controls the storage backend. If this value
        // ever appears in session_meta, discover_sessions must handle it without errors —
        // the loosely-typed Value parser ignores all fields it does not explicitly consume.
        // codex-trace never reads credential files; it only reads JSONL session files.
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/06/15");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-06-15T10-00-00-v0140.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-06-15T10:00:00Z","type":"session_meta","payload":{"id":"v0140-disc-session","timestamp":"2026-06-15T10:00:00Z","cwd":"/project","cli_version":"0.140.0","model_provider":"openai","secret_auth_storage_configuration":"keychain","mcp_oauth_storage":"encrypted"}}"#,
                r#"{"timestamp":"2026-06-15T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-06-15T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1749988802.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "v0140-disc-session")
            .expect("v0.140.0 session must be discovered");
        assert_eq!(session.cli_version.as_deref(), Some("0.140.0"));
        assert_eq!(session.cwd.as_deref(), Some("/project"));
        assert!(!session.is_ongoing, "completed session must not be ongoing");
    }

    #[test]
    fn v0143_token_budget_abort_marks_session_not_ongoing() {
        // Codex v0.142.0 introduced `token_budget_abort`; v0.143.0 (PRs #29918, #30144)
        // reliably flushes it to disk during shutdown. discover_sessions must recognise
        // this event as a turn-terminating signal so the session is NOT shown as ongoing.
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/07/12");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-07-12T10-00-00-v0143-budget.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-07-12T10:00:00Z","type":"session_meta","payload":{"id":"v0143-budget-disc","timestamp":"2026-07-12T10:00:00Z","cwd":"/project","cli_version":"0.143.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-07-12T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-07-12T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Working..."}}"#,
                r#"{"timestamp":"2026-07-12T10:00:03Z","type":"event_msg","payload":{"type":"token_budget_abort","turn_id":"turn-1","limit":8000,"used":8001}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "v0143-budget-disc")
            .expect("session terminated by token_budget_abort must be discovered");
        assert_eq!(session.cli_version.as_deref(), Some("0.143.0"));
        assert!(
            !session.is_ongoing,
            "token_budget_abort must mark session as not ongoing"
        );
        assert_eq!(
            session.end_time.as_deref(),
            Some("2026-07-12T10:00:03Z"),
            "end_time must be set from token_budget_abort timestamp"
        );
    }

    #[test]
    fn v0143_inference_stream_cancelled_marks_session_not_ongoing() {
        // Codex v0.143.0 (PRs #29918, #30144) reliably preserves `inference_stream_cancelled`
        // events on disk during shutdown. discover_sessions must recognise this event as a
        // turn-terminating signal so the session is NOT shown as ongoing.
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/07/12");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-07-12T10-01-00-v0143-cancel.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-07-12T10:01:00Z","type":"session_meta","payload":{"id":"v0143-cancel-disc","timestamp":"2026-07-12T10:01:00Z","cwd":"/project","cli_version":"0.143.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-07-12T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-07-12T10:01:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Working..."}}"#,
                r#"{"timestamp":"2026-07-12T10:01:03Z","type":"event_msg","payload":{"type":"inference_stream_cancelled","turn_id":"turn-1","reason":"user_interrupt"}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "v0143-cancel-disc")
            .expect("session terminated by inference_stream_cancelled must be discovered");
        assert_eq!(session.cli_version.as_deref(), Some("0.143.0"));
        assert!(
            !session.is_ongoing,
            "inference_stream_cancelled must mark session as not ongoing"
        );
        assert_eq!(
            session.end_time.as_deref(),
            Some("2026-07-12T10:01:03Z"),
            "end_time must be set from inference_stream_cancelled timestamp"
        );
    }

    // Codex v0.146.0 (PRs #34621, #35220, issue #210): paginated threads that fork or
    // inherit history from another rollout carry session_meta.history_base.thread_id,
    // pointing at the source rollout this one continues from. discover_sessions must
    // surface this so a paginated-history continuation is not treated as a fully
    // independent session with no prior context.

    #[test]
    fn discover_sessions_v0146_reads_history_base_thread_id() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/08/01");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-08-01T10-00-00-v0146-fork.jsonl");
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

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "v0146-fork-child")
            .expect("paginated fork continuation must be discovered");
        assert_eq!(
            session.history_base_thread_id.as_deref(),
            Some("v0146-fork-parent"),
            "history_base.thread_id must be surfaced as history_base_thread_id"
        );
    }

    #[test]
    fn discover_sessions_v0146_no_history_base_is_none() {
        // Legacy-history sessions (and paginated sessions with no inherited prefix) must
        // not set history_base_thread_id.
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/08/01");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-08-01T10-01-00-v0146-nofork.jsonl");
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

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "v0146-nofork")
            .expect("session must be discovered");
        assert!(
            session.history_base_thread_id.is_none(),
            "session without history_base must have history_base_thread_id == None"
        );
    }

    #[test]
    fn discover_sessions_v0146_malformed_history_base_does_not_panic() {
        // Defensive: a history_base object missing thread_id must be tolerated, not panic.
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/08/01");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-08-01T10-02-00-v0146-malformed.jsonl");
        std::fs::write(
            &path,
            r#"{"timestamp":"2026-08-01T10:02:00Z","type":"session_meta","payload":{"id":"v0146-malformed","timestamp":"2026-08-01T10:02:00Z","history_mode":"paginated","history_base":{"end_ordinal_exclusive":5}}}"#,
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "v0146-malformed")
            .expect("session must be discovered even with malformed history_base");
        assert!(session.history_base_thread_id.is_none());
    }

    // Codex v0.147.0 (issue #220): persistent thread sections (PRs #35722/#36007/#36380)
    // are SQLite-only and never reach the rollout JSONL, and incremental transcript
    // browsing (PRs #36948/#36950) is TUI rendering built on the existing v0.146.0
    // pagination protocol. These tests lock in that a v0.147.0 session still discovers
    // and parses correctly, and that the v0.146.0 pagination marker keeps working
    // unaffected by either change.

    #[test]
    fn discover_sessions_v0147_session_parses_normally() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/08/10");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-08-10T09-00-00-v0147-basic.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-08-10T09:00:00Z","type":"session_meta","payload":{"id":"v0147-basic","timestamp":"2026-08-10T09:00:00Z","cwd":"/project","cli_version":"0.147.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-08-10T09:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-08-10T09:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1786435202.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "v0147-basic")
            .expect("v0.147.0 session must be discovered");
        assert_eq!(session.cli_version.as_deref(), Some("0.147.0"));
    }

    #[test]
    fn discover_sessions_v0147_history_base_thread_id_still_works() {
        // Regression: the TUI-side incremental transcript browsing added in v0.147.0
        // (PRs #36948/#36950) must not disturb the v0.146.0 pagination lineage marker.
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/08/10");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-08-10T09-01-00-v0147-fork.jsonl");
        std::fs::write(
            &path,
            r#"{"timestamp":"2026-08-10T09:01:00Z","type":"session_meta","payload":{"id":"v0147-fork-child","timestamp":"2026-08-10T09:01:00Z","cli_version":"0.147.0","history_mode":"paginated","history_base":{"thread_id":"v0147-fork-parent","end_ordinal_exclusive":8,"end_byte_offset":1024}}}"#,
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "v0147-fork-child")
            .expect("v0.147.0 paginated fork continuation must be discovered");
        assert_eq!(
            session.history_base_thread_id.as_deref(),
            Some("v0147-fork-parent")
        );
    }

    #[test]
    fn discover_sessions_v0147_unrecognized_section_field_does_not_panic() {
        // Defensive: even if a future Codex patch release ever mirrored SQLite section
        // metadata into the rollout file, an unrecognized `section` object on session_meta
        // must be tolerated (ignored), not panic — this scanner reads fields by name via
        // serde_json::Value and has no code path that looks at `section` at all.
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/08/10");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-08-10T09-02-00-v0147-section.jsonl");
        std::fs::write(
            &path,
            r#"{"timestamp":"2026-08-10T09:02:00Z","type":"session_meta","payload":{"id":"v0147-section","timestamp":"2026-08-10T09:02:00Z","cli_version":"0.147.0","section":{"id":"pinned","name":"Pinned"}}}"#,
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "v0147-section")
            .expect("session must be discovered even with an unrecognized section field");
        assert_eq!(session.cli_version.as_deref(), Some("0.147.0"));
    }

    // Codex v0.148.0/v0.149.0 (PRs #37367, #38819, issue #238): `codex exec fork` and
    // `codex exec resume` populate the app-server's SessionConfiguredEvent.forked_from_id
    // (previously hardcoded to None in codex-rs/exec/src/lib.rs). That value is persisted
    // verbatim into the rollout's session_meta.forked_from_id field by
    // codex-rs/rollout/src/recorder.rs for any forked or fork-descended thread — this is a
    // distinct, session-level signal from the per-turn task_started.forked_from_thread_id
    // field. discover_sessions must surface it so fork lineage from `codex exec fork` is
    // detected even when no per-turn signal is present.

    #[test]
    fn discover_sessions_v0148_reads_forked_from_id_for_exec_fork() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/08/20");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-08-20T10-00-00-v0148-exec-fork.jsonl");
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

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "v0148-exec-fork-child")
            .expect("codex exec fork session must be discovered");
        assert_eq!(
            session.forked_from_thread_id.as_deref(),
            Some("v0148-exec-fork-parent"),
            "session_meta.forked_from_id must be surfaced as forked_from_thread_id even with \
             no task_started.forked_from_thread_id present"
        );
    }

    #[test]
    fn discover_sessions_no_forked_from_id_is_none() {
        // Non-forked sessions (the vast majority) must not set forked_from_thread_id.
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/08/20");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-08-20T10-01-00-v0148-nofork.jsonl");
        std::fs::write(
            &path,
            r#"{"timestamp":"2026-08-20T10:01:00Z","type":"session_meta","payload":{"id":"v0148-nofork","timestamp":"2026-08-20T10:01:00Z","cwd":"/project","cli_version":"0.148.0","model_provider":"openai"}}"#,
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "v0148-nofork")
            .expect("session must be discovered");
        assert!(
            session.forked_from_thread_id.is_none(),
            "session without forked_from_id must have forked_from_thread_id == None"
        );
    }

    #[test]
    fn discover_sessions_empty_forked_from_id_is_none() {
        // Defensive: an empty-string forked_from_id must be treated as absent, not surfaced
        // as a bogus lineage pointer.
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/08/20");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-08-20T10-02-00-v0148-empty-fork.jsonl");
        std::fs::write(
            &path,
            r#"{"timestamp":"2026-08-20T10:02:00Z","type":"session_meta","payload":{"id":"v0148-empty-fork","timestamp":"2026-08-20T10:02:00Z","cli_version":"0.148.0","forked_from_id":""}}"#,
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "v0148-empty-fork")
            .expect("session must be discovered even with an empty forked_from_id");
        assert!(session.forked_from_thread_id.is_none());
    }

    // Codex v0.149.0 (PR #39385): the TUI's queue-by-name command resolution now picks
    // the most recently active session when multiple sessions share a queue name,
    // instead of erroring on the ambiguity. codex-trace never resolves sessions by
    // name — discovery reads one CodexSessionInfo per rollout file, identified solely
    // by the `id` extracted from session_meta. `thread_name` (from `thread_name_updated`)
    // is stored only as a display label and is never used to look up, merge, or
    // deduplicate sessions. Verify two independent session files sharing the same
    // thread_name are still discovered as two fully separate sessions, each keeping
    // its own id — there is no name-based resolution path here for the upstream
    // TUI behavior change to affect.
    #[test]
    fn v0149_duplicate_thread_name_sessions_remain_independent() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/08/20");
        std::fs::create_dir_all(&day_dir).unwrap();

        let older_path = day_dir.join("rollout-2026-08-20T09-00-00-v0149-older.jsonl");
        std::fs::write(
            &older_path,
            [
                r#"{"timestamp":"2026-08-20T09:00:00Z","type":"session_meta","payload":{"id":"v0149-older","timestamp":"2026-08-20T09:00:00Z","cwd":"/project","cli_version":"0.149.0"}}"#,
                r#"{"timestamp":"2026-08-20T09:00:01Z","type":"event_msg","payload":{"type":"thread_name_updated","thread_name":"shared-queue-name"}}"#,
                r#"{"timestamp":"2026-08-20T09:00:02Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-08-20T09:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1755680403.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let newer_path = day_dir.join("rollout-2026-08-20T10-00-00-v0149-newer.jsonl");
        std::fs::write(
            &newer_path,
            [
                r#"{"timestamp":"2026-08-20T10:00:00Z","type":"session_meta","payload":{"id":"v0149-newer","timestamp":"2026-08-20T10:00:00Z","cwd":"/project","cli_version":"0.149.0"}}"#,
                r#"{"timestamp":"2026-08-20T10:00:01Z","type":"event_msg","payload":{"type":"thread_name_updated","thread_name":"shared-queue-name"}}"#,
                r#"{"timestamp":"2026-08-20T10:00:02Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-08-20T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1755684003.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        assert_eq!(sessions.len(), 2);

        let older = sessions.iter().find(|s| s.id == "v0149-older").unwrap();
        let newer = sessions.iter().find(|s| s.id == "v0149-newer").unwrap();
        assert_eq!(older.thread_name.as_deref(), Some("shared-queue-name"));
        assert_eq!(newer.thread_name.as_deref(), Some("shared-queue-name"));
    }

    // Codex v0.149.0 (PR #39034): queued messages written by another process can now
    // be dispatched into a session's rollout file. Whichever session's file receives
    // the dispatched message, codex-trace still discovers/attributes it correctly:
    // discovery is driven purely by the file's own on-disk content (id, turns, thread
    // name), not by which process authored a given line. Simulate a queued message
    // landing as an additional turn appended to an already-active session's rollout
    // and verify it is folded into that same session, not lost or misattributed.
    #[test]
    fn v0149_queued_message_appended_to_existing_session_is_attributed_correctly() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/08/20");
        std::fs::create_dir_all(&day_dir).unwrap();

        let path = day_dir.join("rollout-2026-08-20T11-00-00-v0149-target.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-08-20T11:00:00Z","type":"session_meta","payload":{"id":"v0149-target","timestamp":"2026-08-20T11:00:00Z","cwd":"/project","cli_version":"0.149.0"}}"#,
                r#"{"timestamp":"2026-08-20T11:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-08-20T11:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1755687602.0}}"#,
                // Message queued by another process, dispatched once this session picked it up.
                r#"{"timestamp":"2026-08-20T11:05:00Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2"}}"#,
                r#"{"timestamp":"2026-08-20T11:05:01Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-2","completed_at":1755687901.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        assert_eq!(sessions.len(), 1);
        let session = sessions
            .iter()
            .find(|s| s.id == "v0149-target")
            .expect("queued-into session must still be discovered by its own id");
        assert_eq!(session.turn_count, 2);
        assert!(!session.is_ongoing);
    }
}
