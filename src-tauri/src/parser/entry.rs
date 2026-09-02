use serde_json::Value;

/// A raw JSONL line from a Codex session file, loosely typed.
#[derive(Debug, Clone)]
pub struct RawEntry {
    pub entry_type: String,
    pub timestamp: Option<String>,
    pub payload: Value,
    /// The raw line value (useful for oldest-format session_meta where fields are at root)
    pub raw: Value,
}

impl RawEntry {
    /// Parse a single JSONL line into a RawEntry.
    pub fn parse(line: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(line).ok()?;

        // Skip "state" placeholder entries
        if v.get("record_type").and_then(|t| t.as_str()) == Some("state") {
            return None;
        }

        // Skip non-full view mode entries (Codex v0.130.0+, PR #21566).
        // The thread turns endpoint now exposes three view modes: "unloaded"
        // (metadata-only stub), "summary" (partial), and "full" (complete).
        // Only absent (legacy) or "full" entries carry complete turn data;
        // any other view_mode is a placeholder and must be skipped so callers
        // never receive silently truncated turn content.
        if let Some(vm) = v.get("view_mode").and_then(|t| t.as_str()) {
            if vm != "full" {
                return None;
            }
        }

        let entry_type = detect_entry_type(&v);
        let timestamp = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string());
        let payload = v.get("payload").cloned().unwrap_or(Value::Null);

        Some(RawEntry {
            entry_type,
            timestamp,
            payload,
            raw: v,
        })
    }
}

fn detect_entry_type(v: &Value) -> String {
    // Check explicit type field first
    if let Some(t) = v.get("type").and_then(|t| t.as_str()) {
        return t.to_string();
    }

    // Mid format: has payload but no type
    if v.get("payload").is_some() {
        return "session_meta".to_string();
    }

    // Oldest format: has id + timestamp at root
    if v.get("id").is_some() && v.get("timestamp").is_some() {
        return "session_meta_root".to_string();
    }

    // Bare old-format entries (cli_version < 0.44): function_call, function_call_output, message, reasoning
    if v.get("call_id").is_some() && v.get("arguments").is_some() && v.get("name").is_some() {
        return "function_call".to_string();
    }
    if v.get("call_id").is_some() && v.get("output").is_some() {
        return "function_call_output".to_string();
    }
    if v.get("role").is_some() && v.get("content").is_some() {
        return "message".to_string();
    }
    if v.get("encrypted_content").is_some() {
        return "reasoning".to_string();
    }

    "unknown".to_string()
}

/// Extract the event_msg payload type (e.g. "task_started", "user_message", etc.)
pub fn event_msg_type(payload: &Value) -> Option<&str> {
    payload.get("type").and_then(|t| t.as_str())
}

/// Extract the session ID from a session_meta payload.
///
/// Tries paths in version order for forward compatibility:
/// 1. `id` — all pre-v0.129.0 sessions
/// 2. `session_id` — v0.129.0+ (PR #20437)
/// 3. `thread.sessionId` — v0.129.0+ v2 API path (PR #21336)
pub fn extract_session_id(payload: &Value) -> String {
    payload
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            payload
                .get("session_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            payload
                .get("thread")
                .and_then(|t| t.get("sessionId"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Parse an ISO timestamp string to Unix seconds (u64).
pub fn parse_timestamp_secs(ts: &str) -> Option<u64> {
    use chrono::DateTime;
    let dt = ts.parse::<DateTime<chrono::Utc>>().ok()?;
    Some(dt.timestamp() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_session_id_reads_id_field() {
        let v: Value =
            serde_json::from_str(r#"{"id":"abc-123","timestamp":"2026-05-07T00:00:00Z"}"#).unwrap();
        assert_eq!(extract_session_id(&v), "abc-123");
    }

    #[test]
    fn extract_session_id_falls_back_to_session_id_field() {
        // v0.129.0+ PR #20437: session_id field added alongside or instead of id
        let v: Value =
            serde_json::from_str(r#"{"session_id":"sess-456","timestamp":"2026-05-07T00:00:00Z"}"#)
                .unwrap();
        assert_eq!(extract_session_id(&v), "sess-456");
    }

    #[test]
    fn extract_session_id_falls_back_to_thread_session_id() {
        // v0.129.0+ PR #21336: sessionId moved onto Thread object in v2 API
        let v: Value = serde_json::from_str(
            r#"{"thread":{"sessionId":"thread-789"},"timestamp":"2026-05-07T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(extract_session_id(&v), "thread-789");
    }

    #[test]
    fn extract_session_id_prefers_id_over_session_id() {
        let v: Value =
            serde_json::from_str(r#"{"id":"primary","session_id":"secondary"}"#).unwrap();
        assert_eq!(extract_session_id(&v), "primary");
    }

    #[test]
    fn extract_session_id_returns_empty_when_absent() {
        let v: Value = serde_json::from_str(r#"{"timestamp":"2026-05-07T00:00:00Z"}"#).unwrap();
        assert_eq!(extract_session_id(&v), "");
    }

    #[test]
    fn parse_new_session_meta() {
        let line = r#"{"timestamp":"2026-04-25T10:00:00Z","type":"session_meta","payload":{"id":"abc","cwd":"/tmp"}}"#;
        let e = RawEntry::parse(line).unwrap();
        assert_eq!(e.entry_type, "session_meta");
        assert_eq!(e.payload["id"], "abc");
    }

    #[test]
    fn parse_state_placeholder_returns_none() {
        let line = r#"{"record_type":"state"}"#;
        assert!(RawEntry::parse(line).is_none());
    }

    #[test]
    fn parse_event_msg() {
        let line = r#"{"timestamp":"2026-04-25T10:00:00Z","type":"event_msg","payload":{"type":"user_message","message":"hello"}}"#;
        let e = RawEntry::parse(line).unwrap();
        assert_eq!(e.entry_type, "event_msg");
        assert_eq!(event_msg_type(&e.payload), Some("user_message"));
    }

    #[test]
    fn parse_timestamp() {
        assert!(parse_timestamp_secs("2026-04-25T10:00:00Z").is_some());
    }

    fn parse_response_item() {
        let line = r#"{"timestamp":"2026-04-25T10:00:00Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call_1"}}"#;
        let e = RawEntry::parse(line).unwrap();
        assert_eq!(e.entry_type, "response_item");
        assert_eq!(e.payload["type"], "function_call");
    }

    // `response_item` is a JSONL log entry type written by the Codex CLI into session
    // files. It is entirely unrelated to the `codex responses` CLI subcommand that was
    // removed in Codex v0.128.0 (PR #19640). This test guards against that confusion
    // and ensures all expected response_item payload types continue to parse correctly.
    #[test]
    fn response_item_payload_types_parsed_from_jsonl_not_cli_subcommand() {
        let cases = [
            (
                r#"{"timestamp":"2026-04-25T10:00:00Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"c1"}}"#,
                "function_call",
            ),
            (
                r#"{"timestamp":"2026-04-25T10:00:01Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"ok"}}"#,
                "function_call_output",
            ),
            (
                r#"{"timestamp":"2026-04-25T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"hello"}}"#,
                "message",
            ),
            (
                r#"{"timestamp":"2026-04-25T10:00:03Z","type":"response_item","payload":{"type":"reasoning","encrypted_content":"..."}}"#,
                "reasoning",
            ),
        ];
        for (line, expected_payload_type) in cases {
            let e = RawEntry::parse(line).unwrap();
            assert_eq!(e.entry_type, "response_item");
            assert_eq!(e.payload["type"], expected_payload_type);
        }
    }

    /// Codex CLI flags boundary: codex-trace never invokes `codex` at runtime.
    #[test]
    fn codex_cli_flags_read_as_jsonl_data_not_invoked() {
        let line = r#"{"timestamp":"2026-04-30T12:00:00Z","type":"session_meta","payload":{"id":"s1","cwd":"/home/user","permission_profile":"full-auto"}}"#;
        let e = RawEntry::parse(line).unwrap();
        assert_eq!(e.entry_type, "session_meta");
        assert_eq!(e.payload["permission_profile"], "full-auto");
    }

    // Codex v0.130.0 (PR #21566): thread turns endpoint now exposes three view
    // modes. "unloaded" and "summary" entries are placeholders / partial stubs;
    // only absent (legacy) or "full" entries contain complete turn data.

    #[test]
    fn view_mode_unloaded_returns_none() {
        let line = r#"{"timestamp":"2026-05-08T10:00:00Z","type":"response_item","view_mode":"unloaded","payload":{"type":"function_call","name":"exec_command","call_id":"c1"}}"#;
        assert!(RawEntry::parse(line).is_none());
    }

    #[test]
    fn view_mode_summary_returns_none() {
        let line = r#"{"timestamp":"2026-05-08T10:00:00Z","type":"response_item","view_mode":"summary","payload":{"type":"message","role":"assistant","content":"partial"}}"#;
        assert!(RawEntry::parse(line).is_none());
    }

    #[test]
    fn view_mode_full_is_parsed_normally() {
        let line = r#"{"timestamp":"2026-05-08T10:00:00Z","type":"response_item","view_mode":"full","payload":{"type":"function_call","name":"exec_command","call_id":"c2"}}"#;
        let e = RawEntry::parse(line).expect("view_mode:full must parse");
        assert_eq!(e.entry_type, "response_item");
        assert_eq!(e.payload["name"], "exec_command");
    }

    #[test]
    fn absent_view_mode_is_parsed_normally() {
        // Legacy entries (pre-v0.130.0) have no view_mode field; they must still parse.
        let line = r#"{"timestamp":"2026-05-08T10:00:00Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"hello"}}"#;
        let e = RawEntry::parse(line).expect("legacy entry without view_mode must parse");
        assert_eq!(e.entry_type, "response_item");
    }

    // Codex v0.130.0 (PR #21683): "research preview" text removed from startup banner.
    // codex-trace reads version from session_meta.payload.cli_version — not from any
    // banner text output by `codex exec`. This test confirms v0.130.0 sessions parse
    // correctly and that version detection is unaffected by the banner wording change.
    #[test]
    fn v0130_startup_banner_research_preview_removal_does_not_affect_version_detection() {
        let line = r#"{"timestamp":"2026-05-08T10:00:00Z","type":"session_meta","payload":{"id":"v0130-session","timestamp":"2026-05-08T10:00:00Z","cwd":"/tmp","cli_version":"0.130.0","model_provider":"openai"}}"#;
        let e = RawEntry::parse(line).unwrap();
        assert_eq!(e.entry_type, "session_meta");
        assert_eq!(e.payload["cli_version"], "0.130.0");
        assert_eq!(e.payload["id"], "v0130-session");
    }

    #[test]
    fn log_db_log_writer_refactor_does_not_affect_jsonl_session_parser() {
        // Codex v0.128.0 PRs #19234/#19959 refactored the internal log DB into a
        // LogWriter interface and fixed its batch flush timing. That subsystem is a
        // SQLite-backed telemetry store — entirely separate from the JSONL session
        // files at ~/.codex/sessions/ that codex-trace reads. Verify all four
        // standard entry types produced by a v0.128.0 session parse correctly.
        let lines = [
            r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"v0128-session","timestamp":"2026-04-30T10:00:00Z","cwd":"/tmp","cli_version":"0.128.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:03Z","type":"turn_context","payload":{"model":"gpt-5.4","cwd":"/tmp"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746007204.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "turn_context",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.128.0");
    }

    #[test]
    fn codex_v0_130_0_startup_banner_change_does_not_affect_session_parsing() {
        // Codex v0.130.0 (PR #21683) removed "research preview" from the `codex exec`
        // startup banner. Session JSONL files at ~/.codex/sessions/ contain structured
        // data, not banner text — so this UI change has no effect on parsing. Verify
        // that all standard entry types from a v0.130.0 session parse correctly.
        let lines = [
            r#"{"timestamp":"2026-05-08T10:00:00Z","type":"session_meta","payload":{"id":"v0130-session","timestamp":"2026-05-08T10:00:00Z","cwd":"/tmp","cli_version":"0.130.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/tmp"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746700804.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "turn_context",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.130.0");
    }

    // Codex v0.130.0 (PR #21356): built-in MCPs promoted to first-class runtime servers.
    // After this change a session_meta may include extra MCP server metadata fields
    // (e.g. an mcp_servers list with is_builtin flags). The parser must not panic on
    // these extra fields — they are simply ignored by the loosely-typed RawEntry model.
    #[test]
    fn v0130_session_meta_with_builtin_mcp_server_metadata_does_not_panic() {
        // session_meta carrying an mcp_servers list that includes built-in entries.
        // This extra field is irrelevant to codex-trace's parsing but must not crash it.
        let line = r#"{"timestamp":"2026-05-08T10:00:00Z","type":"session_meta","payload":{"id":"v0130-mcp-meta","timestamp":"2026-05-08T10:00:00Z","cwd":"/tmp","cli_version":"0.130.0","mcp_servers":[{"name":"computer_use","is_builtin":true,"status":"connected"},{"name":"github","is_builtin":false,"status":"connected"}]}}"#;
        let e = RawEntry::parse(line).expect("session_meta with mcp_servers must parse");
        assert_eq!(e.entry_type, "session_meta");
        assert_eq!(e.payload["id"], "v0130-mcp-meta");
        assert_eq!(e.payload["cli_version"], "0.130.0");
        // The mcp_servers field is accessible via the payload but not required for parsing.
        assert!(e.payload.get("mcp_servers").is_some());
    }

    // Codex v0.131.0 (PRs #22594, #22647, #22724): profile-v2 layered config format.
    //
    // PR #22594 introduced --profile-v2 as a new layered config file format.
    // PR #22647 made Codex reject the legacy [profiles] TOML section when profile-v2 is active.
    // PR #22724 removed the experimental `instructions_file` config key entirely.
    //
    // Note: As of Codex v0.134.0 (PRs #23883, #24051, #24055, #24059), --profile-v2 was
    // renamed to --profile and all legacy profile v1 support was removed. See the v0134_*
    // tests below for the corresponding v0.134.0 verification.
    //
    // codex-trace does NOT read Codex CLI config files (TOML profiles). It reads only the
    // JSONL session files at ~/.codex/sessions/. The profile-v2 changes affect what Codex
    // writes into session_meta entries:
    //   - A `profile` field may appear in session_meta indicating the active profile name.
    //   - Instructions from the active profile arrive via `base_instructions.text` (already
    //     read by parse_session_meta_new) or may be absent entirely when no profile is set.
    //   - The `instructions_file` config key is gone — sessions from v0.131.0+ will not
    //     have instructions sourced from that key (opt_str returns None gracefully).
    //
    // The loosely-typed RawEntry model ignores unknown fields, so profile-v2 metadata in
    // session_meta does not cause parse failures or panics.

    #[test]
    fn v0131_session_meta_with_profile_v2_field_does_not_panic() {
        // session_meta from a v0.131.0 session started with --profile-v2 active (renamed to
        // --profile in v0.134.0). The `profile` field names the active profile; codex-trace
        // ignores it gracefully.
        let line = r#"{"timestamp":"2026-05-18T10:00:00Z","type":"session_meta","payload":{"id":"v0131-profile-v2","timestamp":"2026-05-18T10:00:00Z","cwd":"/tmp","cli_version":"0.131.0","profile":"work","base_instructions":{"text":"You are a helpful assistant."}}}"#;
        let e = RawEntry::parse(line).expect("session_meta with profile-v2 fields must parse");
        assert_eq!(e.entry_type, "session_meta");
        assert_eq!(e.payload["id"], "v0131-profile-v2");
        assert_eq!(e.payload["cli_version"], "0.131.0");
        assert_eq!(e.payload["profile"], "work");
        assert_eq!(
            e.payload["base_instructions"]["text"],
            "You are a helpful assistant."
        );
    }

    #[test]
    fn v0131_session_meta_without_instructions_does_not_panic() {
        // v0.131.0 removed `instructions_file` from the Codex config. Sessions started
        // without a profile that provided instructions will have no `instructions` or
        // `base_instructions` field — opt_str returns None, not an error.
        let line = r#"{"timestamp":"2026-05-18T10:01:00Z","type":"session_meta","payload":{"id":"v0131-no-instructions","timestamp":"2026-05-18T10:01:00Z","cwd":"/home/user","cli_version":"0.131.0","model_provider":"openai"}}"#;
        let e = RawEntry::parse(line).expect("session_meta without instructions must parse");
        assert_eq!(e.entry_type, "session_meta");
        assert_eq!(e.payload["id"], "v0131-no-instructions");
        assert!(e.payload.get("instructions").is_none());
        assert!(e.payload.get("base_instructions").is_none());
    }

    #[test]
    fn v0131_all_standard_entry_types_parse_correctly() {
        // Regression guard: all four standard JSONL entry types must parse under v0.131.0.
        let lines = [
            r#"{"timestamp":"2026-05-18T10:02:00Z","type":"session_meta","payload":{"id":"v0131-session","timestamp":"2026-05-18T10:02:00Z","cwd":"/tmp","cli_version":"0.131.0","profile":"default","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-05-18T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-18T10:02:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
            r#"{"timestamp":"2026-05-18T10:02:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/tmp"}}"#,
            r#"{"timestamp":"2026-05-18T10:02:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747562524.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "turn_context",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.131.0");
        assert_eq!(meta.payload["profile"], "default");
    }

    // Codex v0.131.0 (PRs #21757, #22193): HTTP request header names changed from
    // underscore form (x_codex_session_id, x_codex_thread_id) to hyphen form
    // (x-codex-session-id, x-codex-thread-id). These are transport-layer headers sent
    // by the Codex CLI to the OpenAI API; they are not logged into the JSONL session
    // files at ~/.codex/sessions/ that codex-trace reads.
    //
    // Session IDs are extracted from JSONL payload fields (id, session_id,
    // thread.sessionId) — the HTTP header rename has no impact on this parser.
    // This test guards against future regressions where someone mistakenly tries to
    // read header-name strings from JSONL payloads.
    #[test]
    fn v0131_hyphenated_api_headers_do_not_affect_session_id_extraction() {
        // session_meta payload from a v0.131.0 session — structurally identical to
        // prior versions. The HTTP header rename is invisible at this layer; the
        // session ID continues to arrive in the `session_id` payload field.
        let payload: serde_json::Value = serde_json::from_str(
            r#"{"session_id":"sess-hyphen-131","timestamp":"2026-05-18T10:00:00Z","cwd":"/tmp","cli_version":"0.131.0"}"#,
        )
        .unwrap();
        assert_eq!(extract_session_id(&payload), "sess-hyphen-131");

        // Confirm neither underscore nor hyphen header-name strings appear as field
        // keys — they are HTTP transport details, not JSONL payload keys.
        assert!(payload.get("x_codex_session_id").is_none());
        assert!(payload.get("x-codex-session-id").is_none());
        assert!(payload.get("x_codex_thread_id").is_none());
        assert!(payload.get("x-codex-thread-id").is_none());
    }

    // Codex v0.132.0 (PR #22706): "Remove legacy shell output formatting paths".
    // exec_command_end events no longer carry a `formatted_output` field — output is
    // exclusively in `aggregated_output`. The JSONL entry types themselves are unchanged;
    // this regression guard confirms all four standard types parse correctly under v0.132.0
    // and that exec_command_end events carrying only `aggregated_output` (no `formatted_output`)
    // are valid JSONL that passes through RawEntry parsing without error.
    #[test]
    fn v0132_all_standard_entry_types_parse_correctly() {
        let lines = [
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"v0132-session","timestamp":"2026-05-20T10:00:00Z","cwd":"/tmp","cli_version":"0.132.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/tmp"}}"#,
            // exec_command_end with aggregated_output only — formatted_output field absent (removed in v0.132.0)
            r#"{"timestamp":"2026-05-20T10:00:04Z","type":"event_msg","payload":{"type":"exec_command_end","call_id":"call_1","aggregated_output":"hello\n","exit_code":0,"status":"completed","duration":{"secs":0,"nanos":50000000}}}"#,
            r#"{"timestamp":"2026-05-20T10:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748606405.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "turn_context",
            "event_msg",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.132.0");
        // exec_command_end payload must contain aggregated_output and no formatted_output
        let exec_end = RawEntry::parse(lines[4]).unwrap();
        assert_eq!(exec_end.payload["type"], "exec_command_end");
        assert_eq!(exec_end.payload["aggregated_output"], "hello\n");
        assert!(exec_end.payload.get("formatted_output").is_none());
    }

    // Codex v0.134.0 (PR #24081): `codex-tui.log` is now opt-in.
    //
    // Before v0.134.0, the TUI log file was written unconditionally at its default
    // path. PR #24081 made this opt-in: the file no longer exists unless the user
    // explicitly enables TUI logging.
    //
    // codex-trace reads session data exclusively from JSONL files at
    // ~/.codex/sessions/ — it does not read `codex-tui.log`. The opt-in change
    // therefore has no effect on session parsing or session discovery. Verify that
    // all four standard JSONL entry types continue to parse correctly for v0.134.0
    // sessions regardless of whether the TUI log is present on disk.

    #[test]
    fn v0134_tui_log_opt_in_does_not_affect_jsonl_session_parser() {
        // Codex v0.134.0 PR #24081 made `codex-tui.log` opt-in. codex-trace reads
        // session data from JSONL files at ~/.codex/sessions/, not from the TUI log,
        // so the opt-in change has no effect on parsing. Verify all four standard
        // entry types produced by a v0.134.0 session parse correctly.
        let lines = [
            r#"{"timestamp":"2026-05-26T10:00:00Z","type":"session_meta","payload":{"id":"v0134-session","timestamp":"2026-05-26T10:00:00Z","cwd":"/tmp","cli_version":"0.134.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-05-26T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-26T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
            r#"{"timestamp":"2026-05-26T10:00:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/tmp"}}"#,
            r#"{"timestamp":"2026-05-26T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748253604.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "turn_context",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.134.0");
        assert_eq!(meta.payload["id"], "v0134-session");
    }

    // Codex v0.134.0 (PRs #23883, #24051, #24055, #24059): --profile-v2 renamed to --profile;
    // legacy profile v1 support removed entirely.
    //
    // PRs #23883, #24051, #24055, #24059 promoted --profile to the primary profile selector
    // and removed all legacy profile v1 resolution and write paths. Passing a legacy profile
    // selector now returns an error instead of silently falling back. This is a CLI-level
    // change: codex-trace does NOT invoke `codex` at runtime and does NOT read Codex TOML
    // config files. Sessions from v0.134.0+ carry the same `profile` field in session_meta
    // as v0.131.0+ sessions — the only observable difference for codex-trace is the
    // cli_version bump. The parser is unaffected; these tests confirm v0.134.0 sessions
    // parse correctly.

    #[test]
    fn v0134_session_meta_with_profile_field_does_not_panic() {
        // session_meta from v0.134.0 with --profile active (flag renamed from --profile-v2).
        // The `profile` field names the active profile; codex-trace reads it gracefully.
        let line = r#"{"timestamp":"2026-05-26T10:00:00Z","type":"session_meta","payload":{"id":"v0134-profile","timestamp":"2026-05-26T10:00:00Z","cwd":"/tmp","cli_version":"0.134.0","profile":"work","base_instructions":{"text":"You are a helpful assistant."}}}"#;
        let e = RawEntry::parse(line).expect("session_meta with profile field must parse");
        assert_eq!(e.entry_type, "session_meta");
        assert_eq!(e.payload["id"], "v0134-profile");
        assert_eq!(e.payload["cli_version"], "0.134.0");
        assert_eq!(e.payload["profile"], "work");
        assert_eq!(
            e.payload["base_instructions"]["text"],
            "You are a helpful assistant."
        );
    }

    #[test]
    fn v0134_session_meta_without_profile_does_not_panic() {
        // v0.134.0 session started without --profile: no `profile` field in session_meta.
        // The parser must handle the absent field gracefully (opt_str returns None).
        let line = r#"{"timestamp":"2026-05-26T10:01:00Z","type":"session_meta","payload":{"id":"v0134-no-profile","timestamp":"2026-05-26T10:01:00Z","cwd":"/home/user","cli_version":"0.134.0","model_provider":"openai"}}"#;
        let e = RawEntry::parse(line).expect("session_meta without profile must parse");
        assert_eq!(e.entry_type, "session_meta");
        assert_eq!(e.payload["id"], "v0134-no-profile");
        assert!(e.payload.get("profile").is_none());
    }

    #[test]
    fn v0134_all_standard_entry_types_parse_correctly_with_profile() {
        // Regression guard: all four standard JSONL entry types must parse under v0.134.0.
        let lines = [
            r#"{"timestamp":"2026-05-26T10:02:00Z","type":"session_meta","payload":{"id":"v0134-session-profile","timestamp":"2026-05-26T10:02:00Z","cwd":"/tmp","cli_version":"0.134.0","profile":"default","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-05-26T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-26T10:02:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
            r#"{"timestamp":"2026-05-26T10:02:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/tmp"}}"#,
            r#"{"timestamp":"2026-05-26T10:02:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748254924.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "turn_context",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.134.0");
        assert_eq!(meta.payload["profile"], "default");
    }

    // Codex v0.133.0 (PR #22709): TurnContextItem fields trimmed.
    // turn_context payloads now carry only the fields still used internally; previously
    // populated fields like cwd and effort may be absent. The loosely-typed RawEntry
    // model must parse both old (extra fields) and new (trimmed) payloads without error.

    #[test]
    fn v0133_turn_context_trimmed_payload_parses_as_turn_context_entry() {
        // v0.133.0 turn_context with minimal payload — only model is present.
        let line = r#"{"timestamp":"2026-05-21T10:00:02Z","type":"turn_context","payload":{"model":"gpt-5"}}"#;
        let e = RawEntry::parse(line).expect("turn_context with minimal payload must parse");
        assert_eq!(e.entry_type, "turn_context");
        assert_eq!(e.payload["model"], "gpt-5");
        // cwd and effort are absent — must not panic
        assert!(e.payload.get("cwd").is_none());
        assert!(e.payload.get("effort").is_none());
    }

    #[test]
    fn v0133_all_standard_entry_types_parse_correctly() {
        // Regression guard: all standard JSONL entry types must parse under v0.133.0.
        // turn_context payload is trimmed — only model is present (PR #22709).
        let lines = [
            r#"{"timestamp":"2026-05-21T10:00:00Z","type":"session_meta","payload":{"id":"v0133-session","timestamp":"2026-05-21T10:00:00Z","cwd":"/tmp","cli_version":"0.133.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:03Z","type":"turn_context","payload":{"model":"gpt-5"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748167204.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "turn_context",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.133.0");
    }

    // Codex v0.133.0 (PR #23564): code-mode exec output is now preserved raw unless an
    // explicit output token limit is requested. function_call_output entries for exec_command
    // now carry the full raw output. The RawEntry parser must pass it through without
    // truncating or erroring regardless of output size or content.

    #[test]
    fn v0133_raw_exec_output_in_function_call_output_parses_correctly() {
        // v0.133.0 raw output: no "Output:" wrapper. Content may be arbitrarily large.
        let lines = [
            r#"{"timestamp":"2026-05-21T10:00:00Z","type":"session_meta","payload":{"id":"v0133-exec-session","timestamp":"2026-05-21T10:00:00Z","cwd":"/project","cli_version":"0.133.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call_exec","arguments":"{\"cmd\":\"cargo build\",\"workdir\":\"/project\"}"}}"#,
            // v0.133.0 raw output in function_call_output — no "Output:" marker, no metadata footer
            r#"{"timestamp":"2026-05-21T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_exec","output":"   Compiling my-crate v1.0.0\n   Compiling dep-crate v2.3.1\nFinished with exit code: 0\n"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748167204.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "response_item",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        // Verify function_call_output payload carries raw output as-is
        let output_entry = RawEntry::parse(lines[3]).unwrap();
        assert_eq!(output_entry.payload["type"], "function_call_output");
        assert_eq!(output_entry.payload["call_id"], "call_exec");
        let raw_output = output_entry.payload["output"].as_str().unwrap();
        assert!(
            raw_output.contains("Compiling"),
            "raw output content must be preserved"
        );
        // "exit code" in raw output is not a structural marker at this parse layer
        assert!(
            raw_output.contains("exit code"),
            "raw content preserved verbatim"
        );
    }

    // Codex v0.135.0 (PR #24591): memory state moved to a dedicated SQLite DB.
    // Active memories are injected into context at turn start and written into
    // turn_context payloads. The RawEntry parser must pass through the memories
    // array so downstream consumers (handle_turn_context) can extract it.

    #[test]
    fn v0135_turn_context_with_memories_parses_correctly() {
        let line = r#"{"timestamp":"2026-05-28T10:00:00Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project","memories":["User prefers terse output","Project uses TypeScript strict mode"]}}"#;
        let e = RawEntry::parse(line).expect("turn_context with memories must parse");
        assert_eq!(e.entry_type, "turn_context");
        let mems = e.payload["memories"]
            .as_array()
            .expect("memories must be array");
        assert_eq!(mems.len(), 2);
        assert_eq!(mems[0], "User prefers terse output");
        assert_eq!(mems[1], "Project uses TypeScript strict mode");
    }

    // Codex v0.134.0 (PR #22882): subagent identity fields added to hook input payloads.
    //
    // PreToolUse and PostToolUse hook inputs now carry `subagent_id` and `subagent_name`
    // so that tool calls can be attributed to the subagent that produced them in
    // multi-agent sessions. These fields are additive — the loosely-typed RawEntry model
    // ignores unknown fields by design, so no deserialization errors can occur regardless
    // of whether the fields are present.

    #[test]
    fn v0134_exec_command_end_with_subagent_identity_parses_without_error() {
        // exec_command_end event carrying the new subagent_id / subagent_name fields
        // (PostToolUse hook input, logged by Codex ≥ v0.134.0). The RawEntry parser
        // must accept these fields without panic — they are ignored at this layer and
        // consumed downstream in toolcall.rs.
        let line = r#"{"timestamp":"2026-05-26T10:00:05Z","type":"event_msg","payload":{"type":"exec_command_end","call_id":"call_1","aggregated_output":"ok\n","exit_code":0,"status":"completed","duration":{"secs":0,"nanos":50000000},"subagent_id":"worker-session-abc","subagent_name":"Parfit"}}"#;
        let e = RawEntry::parse(line).expect("exec_command_end with subagent fields must parse");
        assert_eq!(e.entry_type, "event_msg");
        assert_eq!(
            e.payload.get("type").and_then(|t| t.as_str()),
            Some("exec_command_end")
        );
        // Subagent fields flow through to the payload Value for downstream extraction.
        assert_eq!(e.payload["subagent_id"], "worker-session-abc");
        assert_eq!(e.payload["subagent_name"], "Parfit");
    }

    #[test]
    fn v0134_function_call_with_subagent_identity_parses_without_error() {
        // function_call response_item carrying subagent_id / subagent_name (PreToolUse
        // hook input). Must parse correctly so downstream toolcall.rs can extract the
        // identity and store it on the resulting ToolCall.
        let line = r#"{"timestamp":"2026-05-26T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"echo hi\"}","call_id":"call_sub","subagent_id":"worker-session-xyz","subagent_name":"Noether"}}"#;
        let e = RawEntry::parse(line).expect("function_call with subagent fields must parse");
        assert_eq!(e.entry_type, "response_item");
        assert_eq!(e.payload["subagent_id"], "worker-session-xyz");
        assert_eq!(e.payload["subagent_name"], "Noether");
    }

    #[test]
    fn v0135_all_standard_entry_types_parse_correctly() {
        // Regression guard: all four standard JSONL entry types from a v0.135.0 session
        // must parse correctly. The turn_context now includes a memories array.
        let lines = [
            r#"{"timestamp":"2026-05-28T10:00:00Z","type":"session_meta","payload":{"id":"v0135-session","timestamp":"2026-05-28T10:00:00Z","cwd":"/project","cli_version":"0.135.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-05-28T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-28T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
            r#"{"timestamp":"2026-05-28T10:00:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project","memories":["Active memory note"]}}"#,
            r#"{"timestamp":"2026-05-28T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748426404.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "turn_context",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.135.0");
    }

    // Codex v0.135.0 (PR #24652): plain image wrapper spans removed from session output.
    // Before v0.135.0, image content in function_call_output and message response_items was
    // wrapped in {"type":"image_span","content":[...]}. v0.135.0+ emits bare image items
    // such as {"type":"image_url","image_url":{"url":"..."}}. The RawEntry parser must pass
    // through both formats without panicking; downstream classification is in toolcall.rs.

    #[test]
    fn v0135_function_call_output_with_bare_image_item_does_not_panic() {
        // v0.135.0+: image_generation output carries a bare image_url item (no image_span wrapper).
        let line = r#"{"timestamp":"2026-05-28T10:00:05Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_img","output":[{"type":"image_url","image_url":{"url":"data:image/png;base64,abc123"}}]}}"#;
        let e = RawEntry::parse(line).expect("function_call_output with bare image_url must parse");
        assert_eq!(e.entry_type, "response_item");
        assert_eq!(e.payload["type"], "function_call_output");
        assert_eq!(e.payload["call_id"], "call_img");
        // The output array is accessible and contains one image_url item.
        let output_arr = e.payload["output"]
            .as_array()
            .expect("output must be array");
        assert_eq!(output_arr.len(), 1);
        assert_eq!(output_arr[0]["type"], "image_url");
    }

    #[test]
    fn v0135_image_generation_session_parses_correctly() {
        // Full v0.135.0 session with an image_generation tool call.
        // function_call output contains bare image_url items (no image_span wrapper).
        let lines = [
            r#"{"timestamp":"2026-05-28T10:00:00Z","type":"session_meta","payload":{"id":"v0135-img-session","timestamp":"2026-05-28T10:00:00Z","cwd":"/project","cli_version":"0.135.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-05-28T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-28T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"image_generation","call_id":"call_img","arguments":"{\"prompt\":\"a sunset over mountains\"}"}}"#,
            r#"{"timestamp":"2026-05-28T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_img","output":[{"type":"image_url","image_url":{"url":"data:image/png;base64,abc123"}}]}}"#,
            r#"{"timestamp":"2026-05-28T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748426404.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "response_item",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.135.0");
        assert_eq!(meta.payload["id"], "v0135-img-session");
    }

    // Codex v0.132.0 (PR #23123): `codex exec resume --output-schema` emits response_items with
    // type "structured_output". The RawEntry parser must pass these through without panic.
    // Downstream, handle_response_item (turn.rs) extracts the content as final_answer.

    #[test]
    fn v0132_structured_output_response_item_parses_correctly() {
        let line = r#"{"timestamp":"2026-05-20T10:00:03Z","type":"response_item","payload":{"type":"structured_output","content":{"result":"done","count":42},"output_schema":{"type":"object","properties":{"result":{"type":"string"},"count":{"type":"integer"}}}}}"#;
        let e = RawEntry::parse(line).expect("structured_output response_item must parse");
        assert_eq!(e.entry_type, "response_item");
        assert_eq!(e.payload["type"], "structured_output");
        assert_eq!(e.payload["content"]["result"], "done");
        assert_eq!(e.payload["content"]["count"], 42);
    }

    #[test]
    fn v0132_session_meta_with_output_schema_field_parses_correctly() {
        // session_meta from a session started with --output-schema carries an output_schema
        // field in its payload. The loosely-typed RawEntry model ignores unknown fields, so
        // this must parse without panic and produce the correct entry_type.
        let line = r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"v0132-schema-session","timestamp":"2026-05-20T10:00:00Z","cwd":"/tmp","cli_version":"0.132.0","output_schema":{"type":"object","properties":{"result":{"type":"string"}}}}}"#;
        let e = RawEntry::parse(line).expect("session_meta with output_schema must parse");
        assert_eq!(e.entry_type, "session_meta");
        assert_eq!(e.payload["id"], "v0132-schema-session");
        assert_eq!(e.payload["cli_version"], "0.132.0");
        assert!(
            e.payload.get("output_schema").is_some(),
            "output_schema field must be accessible via payload"
        );
    }

    // Codex v0.136.0 (PR #24962): shell hook output event schemas tightened.
    // hook output events are now emitted as event_msg entries with type "shell_hook_output".
    // The strict schema requires call_id, hook_type, stdout, exit_code; previously nullable
    // fields (metadata, stderr) are absent rather than null. RawEntry must parse these
    // as event_msg entries without error.

    #[test]
    fn v0136_shell_hook_output_parses_as_event_msg() {
        // v0.136.0 strict schema — no metadata, no stderr fields
        let line = r#"{"timestamp":"2026-06-01T10:00:00Z","type":"event_msg","payload":{"type":"shell_hook_output","call_id":"hook-1","hook_type":"pre_exec","stdout":"hook ok\n","exit_code":0,"duration":{"secs":0,"nanos":5000000}}}"#;
        let e = RawEntry::parse(line).expect("shell_hook_output event must parse");
        assert_eq!(e.entry_type, "event_msg");
        assert_eq!(event_msg_type(&e.payload), Some("shell_hook_output"));
        assert_eq!(e.payload["hook_type"], "pre_exec");
        assert_eq!(e.payload["exit_code"], 0);
    }

    #[test]
    fn v0136_shell_hook_output_absent_nullable_fields_does_not_panic() {
        // v0.136.0 tightening: metadata and stderr are absent (not null).
        // Verify that RawEntry parsing does not panic on the minimal strict payload.
        let line = r#"{"timestamp":"2026-06-01T10:01:00Z","type":"event_msg","payload":{"type":"shell_hook_output","call_id":"hook-min","hook_type":"post_mcp","stdout":"","exit_code":0}}"#;
        let e = RawEntry::parse(line).expect("minimal shell_hook_output must parse");
        assert_eq!(e.entry_type, "event_msg");
        assert_eq!(event_msg_type(&e.payload), Some("shell_hook_output"));
        // These fields are absent in the v0.136.0 strict schema — must not be present
        assert!(e.payload.get("metadata").is_none());
        assert!(e.payload.get("stderr").is_none());
    }

    // Codex v0.150.0 (PR #40511): a new `Interrupt` variant was added to the HookEventName
    // enum — it fires for an active turn just before its interrupted-abort event, flushing
    // the turn transcript first. codex-trace's shell_hook_output schema carries the hook
    // event name as a free-form `hook_type` string (see #104, #107), so the new value must
    // parse the same way existing hook_type values (pre_exec, post_exec, pre_mcp) do.

    #[test]
    fn v0150_interrupt_hook_output_parses_as_event_msg() {
        let line = r#"{"timestamp":"2026-08-01T10:00:00Z","type":"event_msg","payload":{"type":"shell_hook_output","call_id":"hook-interrupt-1","hook_type":"interrupt","stdout":"turn interrupted\n","exit_code":0}}"#;
        let e = RawEntry::parse(line).expect("interrupt shell_hook_output event must parse");
        assert_eq!(e.entry_type, "event_msg");
        assert_eq!(event_msg_type(&e.payload), Some("shell_hook_output"));
        assert_eq!(e.payload["hook_type"], "interrupt");
    }

    // Codex v0.132.0 (PR #23123): `codex exec resume --output-schema` produces structured
    // JSON output items. A "structured_output" response_item carries a JSON-validated
    // content object as its payload. RawEntry must parse these without panicking.

    #[test]
    fn v0132_exec_resume_output_schema_structured_output_item_parses_correctly() {
        let line = r#"{"timestamp":"2026-05-20T10:00:03Z","type":"response_item","payload":{"type":"structured_output","content":{"result":"ok","value":42}}}"#;
        let e = RawEntry::parse(line).expect("structured_output response_item must parse");
        assert_eq!(e.entry_type, "response_item");
        assert_eq!(e.payload["type"], "structured_output");
        assert_eq!(e.payload["content"]["result"], "ok");
        assert_eq!(e.payload["content"]["value"], 42);
    }

    #[test]
    fn v0132_exec_resume_output_schema_full_session_entry_types_parse_correctly() {
        // Regression guard: all standard JSONL entry types plus structured_output must
        // parse correctly for a v0.132.0 session run with --output-schema.
        let lines = [
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"v0132-schema-session","timestamp":"2026-05-20T10:00:00Z","cwd":"/tmp","cli_version":"0.132.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/tmp"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"response_item","payload":{"type":"structured_output","content":{"result":"done","items":["a","b"]}}}"#,
            r#"{"timestamp":"2026-05-20T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748606404.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "turn_context",
            "response_item",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.132.0");
        // The structured_output item content must be accessible via the payload.
        let schema_item = RawEntry::parse(lines[3]).unwrap();
        assert_eq!(schema_item.payload["type"], "structured_output");
        assert_eq!(schema_item.payload["content"]["result"], "done");
    }

    #[test]
    fn v0136_all_standard_entry_types_parse_correctly() {
        // Regression guard: all standard entry types plus shell_hook_output must parse
        // correctly for a v0.136.0 session.
        let lines = [
            r#"{"timestamp":"2026-06-01T10:02:00Z","type":"session_meta","payload":{"id":"v0136-session","timestamp":"2026-06-01T10:02:00Z","cwd":"/project","cli_version":"0.136.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-01T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-01T10:02:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
            r#"{"timestamp":"2026-06-01T10:02:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project"}}"#,
            r#"{"timestamp":"2026-06-01T10:02:04Z","type":"event_msg","payload":{"type":"shell_hook_output","call_id":"hook-v0136","hook_type":"pre_exec","stdout":"ok\n","exit_code":0}}"#,
            r#"{"timestamp":"2026-06-01T10:02:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748779325.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "turn_context",
            "event_msg",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.136.0");
    }

    // Codex v0.138.0 (PRs #25944, #25947): local image attachments and standalone image
    // generations now expose their saved file paths. The file_path is a top-level field in
    // the function_call_output response_item payload alongside call_id and output.
    // RawEntry must parse the payload through without error; toolcall.rs extracts the field.

    #[test]
    fn v0138_function_call_output_with_file_path_parses_as_response_item() {
        // image_generation result with the new file_path field (v0.138.0+)
        let line = r#"{"timestamp":"2026-06-08T10:00:00Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_img_138","output":[{"type":"image_url","image_url":{"url":"data:image/png;base64,abc123"}}],"file_path":"/home/user/.codex/images/sunset_abc123.png"}}"#;
        let e = RawEntry::parse(line).expect("function_call_output with file_path must parse");
        assert_eq!(e.entry_type, "response_item");
        assert_eq!(e.payload["type"], "function_call_output");
        assert_eq!(e.payload["call_id"], "call_img_138");
        assert_eq!(
            e.payload["file_path"],
            "/home/user/.codex/images/sunset_abc123.png"
        );
        let output_arr = e.payload["output"]
            .as_array()
            .expect("output must be array");
        assert_eq!(output_arr[0]["type"], "image_url");
    }

    #[test]
    fn v0138_all_standard_entry_types_parse_correctly() {
        // Regression guard: all standard JSONL entry types must parse under v0.138.0.
        let lines = [
            r#"{"timestamp":"2026-06-08T10:00:00Z","type":"session_meta","payload":{"id":"v0138-session","timestamp":"2026-06-08T10:00:00Z","cwd":"/project","cli_version":"0.138.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-08T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-08T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"image_generation","call_id":"call_img","arguments":"{\"prompt\":\"a sunset\"}"}}"#,
            r#"{"timestamp":"2026-06-08T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_img","output":[{"type":"image_url","image_url":{"url":"data:image/png;base64,abc"}}],"file_path":"/home/user/.codex/images/sunset_123.png"}}"#,
            r#"{"timestamp":"2026-06-08T10:00:04Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project"}}"#,
            r#"{"timestamp":"2026-06-08T10:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1749376805.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "response_item",
            "turn_context",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.138.0");
        // Verify the file_path field is accessible in the image output payload
        let img_output = RawEntry::parse(lines[3]).unwrap();
        assert_eq!(
            img_output.payload["file_path"],
            "/home/user/.codex/images/sunset_123.png"
        );
    }

    // Codex v0.141.0 (PR #28355): ResponseItem gains a new optional top-level `metadata` field.
    // The field carries additional per-item structured data populated by the server in certain
    // response flows. RawEntry must parse response_items with a metadata field without error,
    // and the field must be accessible via entry.payload so downstream consumers can read it.

    #[test]
    fn v0141_response_item_with_metadata_field_parses_correctly() {
        let line = r#"{"timestamp":"2026-06-18T10:00:00Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello","metadata":{"server_key":"srv-abc123","model_version":"gpt-5.4-preview","trace_id":"trace-xyz"}}}"#;
        let e = RawEntry::parse(line).expect("response_item with metadata must parse");
        assert_eq!(e.entry_type, "response_item");
        assert_eq!(e.payload["type"], "message");
        // metadata field must be accessible and preserved round-trip
        let meta = &e.payload["metadata"];
        assert!(!meta.is_null(), "metadata field must be present");
        assert_eq!(meta["server_key"], "srv-abc123");
        assert_eq!(meta["trace_id"], "trace-xyz");
    }

    #[test]
    fn v0141_response_item_without_metadata_is_backward_compatible() {
        // Pre-v0.141.0 sessions must still parse normally when metadata is absent.
        let line = r#"{"timestamp":"2026-06-18T10:00:00Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#;
        let e = RawEntry::parse(line).expect("response_item without metadata must parse");
        assert_eq!(e.entry_type, "response_item");
        assert_eq!(e.payload["type"], "message");
        assert!(
            e.payload.get("metadata").is_none(),
            "metadata must be absent in pre-v0.141.0 entries"
        );
    }

    #[test]
    fn v0141_function_call_response_item_with_metadata_parses_correctly() {
        let line = r#"{"timestamp":"2026-06-18T10:00:01Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call_meta_1","arguments":"{\"cmd\":\"echo hi\"}","metadata":{"priority":"high","request_id":"req-789"}}}"#;
        let e = RawEntry::parse(line).expect("function_call with metadata must parse");
        assert_eq!(e.entry_type, "response_item");
        assert_eq!(e.payload["type"], "function_call");
        assert_eq!(e.payload["metadata"]["priority"], "high");
        assert_eq!(e.payload["metadata"]["request_id"], "req-789");
    }

    #[test]
    fn v0141_all_standard_entry_types_parse_correctly() {
        let lines = [
            r#"{"timestamp":"2026-06-18T10:00:00Z","type":"session_meta","payload":{"id":"v0141-session","timestamp":"2026-06-18T10:00:00Z","cwd":"/project","cli_version":"0.141.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-18T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-18T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello","metadata":{"server_key":"srv-v0141","trace_id":"trace-0141"}}}"#,
            r#"{"timestamp":"2026-06-18T10:00:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project"}}"#,
            r#"{"timestamp":"2026-06-18T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1750240804.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "turn_context",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.141.0");
        // metadata field must be preserved on the response_item payload
        let msg_entry = RawEntry::parse(lines[2]).unwrap();
        assert_eq!(msg_entry.payload["metadata"]["server_key"], "srv-v0141");
    }

    // Codex v0.142.0 (PR #28968): the `metadata` field on chat message response_items was
    // renamed to `internal_chat_message_metadata_passthrough` to make the internal-passthrough
    // nature of the field explicit. Sessions written by v0.142.0+ will carry the new key;
    // pre-v0.142.0 sessions retain the old `metadata` key (backward compat — both must parse).

    #[test]
    fn v0142_chat_message_metadata_renamed_to_internal_passthrough() {
        // Regression: new key must be present and old `metadata` key must be absent.
        let line = r#"{"timestamp":"2026-06-22T10:00:00Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello v0142","internal_chat_message_metadata_passthrough":{"server_key":"srv-v0142","trace_id":"trace-v0142","model_version":"gpt-5.4-preview"}}}"#;
        let e = RawEntry::parse(line)
            .expect("response_item with internal_chat_message_metadata_passthrough must parse");
        assert_eq!(e.entry_type, "response_item");
        assert_eq!(e.payload["type"], "message");
        // New field must be accessible.
        let meta = &e.payload["internal_chat_message_metadata_passthrough"];
        assert!(
            !meta.is_null(),
            "internal_chat_message_metadata_passthrough must be present"
        );
        assert_eq!(meta["server_key"], "srv-v0142");
        assert_eq!(meta["trace_id"], "trace-v0142");
        // Old field must be absent — the key was renamed, not aliased.
        assert!(
            e.payload.get("metadata").is_none(),
            "old `metadata` key must be absent in v0.142.0+ chat message items"
        );
    }

    #[test]
    fn v0142_function_call_response_item_with_internal_passthrough_parses_correctly() {
        // function_call items also use internal_chat_message_metadata_passthrough in v0.142.0+.
        let line = r#"{"timestamp":"2026-06-22T10:00:01Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call-v142","arguments":"{\"cmd\":\"ls\"}","internal_chat_message_metadata_passthrough":{"priority":"high","request_id":"req-v142"}}}"#;
        let e = RawEntry::parse(line)
            .expect("function_call with internal_chat_message_metadata_passthrough must parse");
        assert_eq!(e.payload["type"], "function_call");
        assert_eq!(
            e.payload["internal_chat_message_metadata_passthrough"]["priority"],
            "high"
        );
        assert_eq!(
            e.payload["internal_chat_message_metadata_passthrough"]["request_id"],
            "req-v142"
        );
        assert!(
            e.payload.get("metadata").is_none(),
            "old `metadata` key must be absent in v0.142.0+ function_call items"
        );
    }

    #[test]
    fn v0142_pre_v0142_sessions_with_old_metadata_key_remain_backward_compatible() {
        // Sessions written before v0.142.0 carry the old `metadata` key — they must still parse.
        let line = r#"{"timestamp":"2026-06-18T10:00:00Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello","metadata":{"server_key":"srv-abc123","trace_id":"trace-xyz"}}}"#;
        let e = RawEntry::parse(line).expect("pre-v0.142.0 response_item must still parse");
        let meta = &e.payload["metadata"];
        assert!(
            !meta.is_null(),
            "old metadata key must remain accessible for pre-v0.142.0 sessions"
        );
        assert_eq!(meta["server_key"], "srv-abc123");
        assert!(
            e.payload
                .get("internal_chat_message_metadata_passthrough")
                .is_none(),
            "new field must be absent in pre-v0.142.0 sessions"
        );
    }

    // Codex v0.139.0 (PRs #24118, #27084): tool and connector input schemas now preserve
    // oneOf and allOf structures instead of flattening them. Large schemas also keep more
    // shallow structure when compacted.
    //
    // codex-trace reads session JSONL at the RawEntry level using serde_json::Value — it
    // never deserialises tool schemas into typed structs, so oneOf/allOf in schema payloads
    // cannot cause parse failures here. These tests confirm all standard entry types from a
    // v0.139.0 session parse correctly and that function_call entries carrying JSON-object
    // arguments (not stringified-JSON arguments) are faithfully preserved.

    #[test]
    fn v0139_all_standard_entry_types_parse_correctly() {
        let lines = [
            r#"{"timestamp":"2026-06-09T10:00:00Z","type":"session_meta","payload":{"id":"v0139-session","timestamp":"2026-06-09T10:00:00Z","cwd":"/project","cli_version":"0.139.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-09T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-09T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
            r#"{"timestamp":"2026-06-09T10:00:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project"}}"#,
            r#"{"timestamp":"2026-06-09T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1749466804.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "turn_context",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.139.0");
        assert_eq!(meta.payload["id"], "v0139-session");
    }

    #[test]
    fn v0139_function_call_with_object_arguments_does_not_panic() {
        let line = r#"{"timestamp":"2026-06-09T10:00:02Z","type":"response_item","payload":{"type":"function_call","call_id":"call-v139","name":"exec_command","arguments":{"cmd":"echo hello","workdir":"/tmp"}}}"#;
        let e = RawEntry::parse(line).expect("function_call with object arguments must parse");
        assert_eq!(e.entry_type, "response_item");
        assert_eq!(e.payload["type"], "function_call");
        assert_eq!(e.payload["call_id"], "call-v139");
        assert_eq!(e.payload["arguments"]["cmd"], "echo hello");
        assert_eq!(e.payload["arguments"]["workdir"], "/tmp");
    }

    #[test]
    fn v0139_mcp_tool_call_with_oneof_allof_schema_does_not_panic() {
        let line = r#"{"timestamp":"2026-06-09T10:00:03Z","type":"response_item","payload":{"type":"mcp_tool_call","call_id":"mcp-v139","server":"my-connector","tool":"submit_form","arguments":{"value":{"oneOf":[{"type":"string"},{"type":"number"}]}}}}"#;
        let e = RawEntry::parse(line).expect("mcp_tool_call with oneOf argument must parse");
        assert_eq!(e.entry_type, "response_item");
        assert_eq!(e.payload["type"], "mcp_tool_call");
        assert!(e.payload["arguments"]["value"]["oneOf"].is_array());
    }

    #[test]
    fn v0139_session_meta_with_one_of_tool_schema_does_not_panic() {
        let line = r#"{"timestamp":"2026-06-09T10:00:00Z","type":"session_meta","payload":{"id":"v0139-oneof","timestamp":"2026-06-09T10:00:00Z","cwd":"/project","cli_version":"0.139.0","tools":[{"name":"complex_tool","description":"A tool with a oneOf schema","input_schema":{"type":"object","properties":{"action":{"oneOf":[{"type":"string","enum":["create","update","delete"]},{"type":"object","properties":{"custom_op":{"type":"string"},"target":{"type":"string"}},"required":["custom_op"]}]}},"required":["action"]}}]}}"#;
        let e = RawEntry::parse(line).expect("session_meta with oneOf tool schema must parse");
        assert_eq!(e.entry_type, "session_meta");
        assert_eq!(e.payload["id"], "v0139-oneof");
        assert_eq!(e.payload["cli_version"], "0.139.0");
        assert!(e.payload.get("tools").is_some());
    }

    // Codex v0.140.0 (PRs #27504, #27535, #27539, #27541): CLI auth tokens and MCP OAuth
    // credentials were migrated from plaintext files to an encrypted secret store. The new
    // config option `secret_auth_storage_configuration` controls which backend is used.
    //
    // codex-trace reads only JSONL session files at ~/.codex/sessions/ — it never reads
    // credential files, auth token files, or Codex CLI config files from the Codex home
    // directory. The encrypted credential storage change is therefore invisible to this
    // parser. These tests confirm all standard entry types from a v0.140.0 session parse
    // correctly, and that a session_meta carrying the new auth config field is handled
    // without errors (unknown fields are gracefully ignored by the Value-based parser).

    #[test]
    fn v0140_all_standard_entry_types_parse_correctly() {
        let lines = [
            r#"{"timestamp":"2026-06-15T10:00:00Z","type":"session_meta","payload":{"id":"v0140-session","timestamp":"2026-06-15T10:00:00Z","cwd":"/project","cli_version":"0.140.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-15T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-15T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
            r#"{"timestamp":"2026-06-15T10:00:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project"}}"#,
            r#"{"timestamp":"2026-06-15T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1749988804.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "turn_context",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.140.0");
        assert_eq!(meta.payload["id"], "v0140-session");
    }

    // Codex v0.142.0 (PR #28368): multi-agent v2 inter-agent messages now use typed
    // envelopes. The `agent_message` event_msg payload's `message` field changed from a
    // plain string to a typed object `{"type": "<kind>", "content": "..."}`. The RawEntry
    // parser must pass the payload through without error — payload extraction is unaffected
    // because RawEntry is Value-based. The type-aware decoding happens in turn.rs.

    #[test]
    fn v0142_agent_message_with_typed_envelope_parses_as_event_msg() {
        let line = r#"{"timestamp":"2026-06-22T10:00:02Z","type":"event_msg","payload":{"type":"agent_message","message":{"type":"text","content":"Hello from the subagent."},"phase":"main"}}"#;
        let e = RawEntry::parse(line).expect("typed-envelope agent_message must parse");
        assert_eq!(e.entry_type, "event_msg");
        assert_eq!(event_msg_type(&e.payload), Some("agent_message"));
        // The typed envelope is accessible as a Value for downstream decoding.
        let msg = &e.payload["message"];
        assert!(msg.is_object(), "message must be an object in v0.142.0+");
        assert_eq!(msg["type"], "text");
        assert_eq!(msg["content"], "Hello from the subagent.");
    }

    #[test]
    fn v0142_all_standard_entry_types_parse_correctly() {
        let lines = [
            r#"{"timestamp":"2026-06-22T10:00:00Z","type":"session_meta","payload":{"id":"v0142-session","timestamp":"2026-06-22T10:00:00Z","cwd":"/project","cli_version":"0.142.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:04Z","type":"event_msg","payload":{"type":"agent_message","message":{"type":"text","content":"Processing."},"phase":"commentary"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1750593605.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "turn_context",
            "event_msg",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.142.0");
        assert_eq!(meta.payload["id"], "v0142-session");
        // Verify the typed-envelope agent_message payload is accessible.
        let agent_msg = RawEntry::parse(lines[4]).unwrap();
        assert_eq!(agent_msg.payload["message"]["type"], "text");
        assert_eq!(agent_msg.payload["message"]["content"], "Processing.");
    }

    #[test]
    fn v0140_session_meta_with_secret_auth_storage_configuration_does_not_panic() {
        // session_meta may carry the new secret_auth_storage_configuration field introduced
        // in v0.140.0. The field names the credential storage backend ("keychain", "file",
        // etc.). codex-trace never reads credential files — it only extracts known fields
        // from the payload — so this unknown field must be silently ignored.
        let line = r#"{"timestamp":"2026-06-15T10:00:00Z","type":"session_meta","payload":{"id":"v0140-auth","timestamp":"2026-06-15T10:00:00Z","cwd":"/project","cli_version":"0.140.0","model_provider":"openai","secret_auth_storage_configuration":"keychain","mcp_oauth_storage":"encrypted"}}"#;
        let e = RawEntry::parse(line).expect("session_meta with auth config fields must parse");
        assert_eq!(e.entry_type, "session_meta");
        assert_eq!(e.payload["id"], "v0140-auth");
        assert_eq!(e.payload["cli_version"], "0.140.0");
        // Auth config fields are present in the raw payload but codex-trace does not use them.
        assert_eq!(e.payload["secret_auth_storage_configuration"], "keychain");
        assert_eq!(e.payload["mcp_oauth_storage"], "encrypted");
    }

    // Codex v0.136.0: `codex archive` / `codex unarchive` append session_archived and
    // session_unarchived entries to the JSONL file.

    #[test]
    fn v0136_session_archived_event_parses_correctly() {
        let line = r#"{"timestamp":"2026-06-01T12:00:00Z","type":"session_archived","payload":{}}"#;
        let e = RawEntry::parse(line).expect("session_archived must parse");
        assert_eq!(e.entry_type, "session_archived");
    }

    #[test]
    fn v0136_session_unarchived_event_parses_correctly() {
        let line =
            r#"{"timestamp":"2026-06-01T13:00:00Z","type":"session_unarchived","payload":{}}"#;
        let e = RawEntry::parse(line).expect("session_unarchived must parse");
        assert_eq!(e.entry_type, "session_unarchived");
    }

    // Codex v0.141.0 (PRs #26242, #26245): exec-server remote transport migrated to
    // authenticated, end-to-end encrypted Noise relay channels by default. The previous
    // plaintext/TLS WebSocket between the CLI and exec-server is replaced by Noise-protocol
    // relay frames.
    //
    // codex-trace reads session data exclusively from JSONL files at ~/.codex/sessions/ —
    // it never connects to the exec-server, never reads WebSocket frames, and never touches
    // the Noise relay transport. The app-server decrypts Noise frames before surfacing events
    // via its standard APIs; those events continue to be logged to disk in the same JSONL
    // format as all prior versions.
    //
    // The transport change is therefore invisible to this parser: entry types, field names,
    // and payload shapes are unchanged. The tests below confirm all standard JSONL entry
    // types from a v0.141.0 session parse correctly.

    #[test]
    fn v0141_all_standard_entry_types_parse_correctly_noise_relay() {
        let lines = [
            r#"{"timestamp":"2026-06-18T10:00:00Z","type":"session_meta","payload":{"id":"v0141-session","timestamp":"2026-06-18T10:00:00Z","cwd":"/project","cli_version":"0.141.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-18T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-18T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
            r#"{"timestamp":"2026-06-18T10:00:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project"}}"#,
            r#"{"timestamp":"2026-06-18T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1750244404.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "turn_context",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.141.0");
        assert_eq!(meta.payload["id"], "v0141-session");
    }

    // Remote exec sessions started via the new Noise relay transport produce JSONL entries
    // in the same format as local sessions — the Noise encryption boundary is at the network
    // layer, not in the on-disk session format. codex-trace reads files written after
    // decryption and is unaffected by the transport-layer change.
    #[test]
    fn v0141_noise_relay_transport_change_does_not_affect_remote_exec_session_parsing() {
        let lines = [
            r#"{"timestamp":"2026-06-18T10:01:00Z","type":"session_meta","payload":{"id":"v0141-remote-session","timestamp":"2026-06-18T10:01:00Z","cwd":"/remote/project","cli_version":"0.141.0","model_provider":"openai","originator":"exec-server"}}"#,
            r#"{"timestamp":"2026-06-18T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-18T10:01:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"echo remote\",\"workdir\":\"/remote/project\"}","call_id":"call-remote-1"}}"#,
            r#"{"timestamp":"2026-06-18T10:01:03Z","type":"event_msg","payload":{"type":"exec_command_end","call_id":"call-remote-1","aggregated_output":"remote\n","exit_code":0,"status":"completed","duration":{"secs":0,"nanos":10000000}}}"#,
            r#"{"timestamp":"2026-06-18T10:01:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1750244464.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "event_msg",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.141.0");
        assert_eq!(meta.payload["id"], "v0141-remote-session");
        // originator field is passed through unchanged regardless of transport layer
        assert_eq!(meta.payload["originator"], "exec-server");
    }

    // Codex v0.140.0 (PRs #27070, #27071, #27703): /import command writes lifecycle
    // event_msg entries; v0.141.0 (PR #28008) adds external_agent_import_result.

    #[test]
    fn v0140_agent_context_import_event_msg_parses_correctly() {
        let line = r#"{"timestamp":"2026-06-15T10:00:00Z","type":"event_msg","payload":{"type":"agent_context_imported","source":"claude-code","thread_count":3,"token_count":12400}}"#;
        let e = RawEntry::parse(line).expect("agent_context_imported event_msg must parse");
        assert_eq!(e.entry_type, "event_msg");
        assert_eq!(
            e.payload.get("type").and_then(|t| t.as_str()),
            Some("agent_context_imported")
        );
        assert_eq!(e.payload["source"], "claude-code");
    }

    #[test]
    fn v0141_external_agent_import_result_response_item_parses_correctly() {
        let line = r#"{"timestamp":"2026-06-15T10:00:01Z","type":"response_item","payload":{"type":"external_agent_import_result","source":"claude-code","imported_thread_ids":["t1","t2"],"total_tokens":12400}}"#;
        let e = RawEntry::parse(line).expect("external_agent_import_result must parse");
        assert_eq!(e.payload["type"], "external_agent_import_result");
        assert_eq!(e.payload["total_tokens"], 12400);
    }

    // Codex v0.142.2 (PR #28360): ResponseItem gains a `turn_id` field on its `metadata`
    // object. The field allows downstream consumers to correlate response items to turns
    // by explicit ID rather than by positional heuristics. The loosely-typed RawEntry
    // model passes the metadata through unchanged; turn.rs reads metadata.turn_id to
    // prefer the explicit signal over current_turn_id when both are available.

    #[test]
    fn v0142_response_item_with_metadata_turn_id_parses_correctly() {
        let line = r#"{"timestamp":"2026-06-25T10:00:00Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello","metadata":{"turn_id":"turn-abc","trace_id":"trace-xyz"}}}"#;
        let e = RawEntry::parse(line).expect("response_item with metadata.turn_id must parse");
        assert_eq!(e.entry_type, "response_item");
        assert_eq!(e.payload["type"], "message");
        // metadata.turn_id must be accessible for downstream turn correlation
        assert_eq!(e.payload["metadata"]["turn_id"], "turn-abc");
        assert_eq!(e.payload["metadata"]["trace_id"], "trace-xyz");
    }

    #[test]
    fn v0142_function_call_response_item_with_metadata_turn_id_parses_correctly() {
        let line = r#"{"timestamp":"2026-06-25T10:00:01Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call-1","arguments":"{\"cmd\":\"echo hi\"}","metadata":{"turn_id":"turn-abc"}}}"#;
        let e = RawEntry::parse(line).expect("function_call with metadata.turn_id must parse");
        assert_eq!(e.entry_type, "response_item");
        assert_eq!(e.payload["type"], "function_call");
        assert_eq!(e.payload["metadata"]["turn_id"], "turn-abc");
    }

    // Codex v0.144.0 (PR #28772): MCP tools can request interactive authentication by
    // default (no longer behind an experimental flag). Sessions with MCP tools that need
    // auth will emit mcp_auth_request and mcp_auth_result event_msg entries during the
    // OAuth / API-key handshake. These must parse without errors since they now appear in
    // ordinary sessions, not just experimental ones.

    #[test]
    fn v0144_mcp_auth_request_event_msg_parses_correctly() {
        let line = r#"{"timestamp":"2026-07-01T10:00:02Z","type":"event_msg","payload":{"type":"mcp_auth_request","server":"github","call_id":"auth-1","auth_url":"https://github.com/login/oauth/authorize?client_id=abc","instructions":"Visit the URL to grant access."}}"#;
        let e = RawEntry::parse(line).expect("mcp_auth_request event_msg must parse");
        assert_eq!(e.entry_type, "event_msg");
        assert_eq!(event_msg_type(&e.payload), Some("mcp_auth_request"));
        assert_eq!(e.payload["server"], "github");
        assert_eq!(e.payload["call_id"], "auth-1");
    }

    #[test]
    fn v0144_mcp_auth_result_event_msg_parses_correctly() {
        let line = r#"{"timestamp":"2026-07-01T10:00:05Z","type":"event_msg","payload":{"type":"mcp_auth_result","server":"github","call_id":"auth-1","status":"authenticated"}}"#;
        let e = RawEntry::parse(line).expect("mcp_auth_result event_msg must parse");
        assert_eq!(e.entry_type, "event_msg");
        assert_eq!(event_msg_type(&e.payload), Some("mcp_auth_result"));
        assert_eq!(e.payload["server"], "github");
        assert_eq!(e.payload["status"], "authenticated");
    }

    #[test]
    fn v0144_mcp_auth_result_failed_event_msg_parses_correctly() {
        let line = r#"{"timestamp":"2026-07-01T10:00:06Z","type":"event_msg","payload":{"type":"mcp_auth_result","server":"slack","call_id":"auth-2","status":"failed","error":"User cancelled authentication"}}"#;
        let e = RawEntry::parse(line).expect("mcp_auth_result failed event_msg must parse");
        assert_eq!(e.entry_type, "event_msg");
        assert_eq!(event_msg_type(&e.payload), Some("mcp_auth_result"));
        assert_eq!(e.payload["status"], "failed");
    }

    #[test]
    fn v0144_all_standard_entry_types_parse_correctly_with_mcp_auth_flow() {
        let lines = [
            r#"{"timestamp":"2026-07-01T10:00:00Z","type":"session_meta","payload":{"id":"v0144-session","timestamp":"2026-07-01T10:00:00Z","cwd":"/project","cli_version":"0.144.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-07-01T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-07-01T10:00:02Z","type":"event_msg","payload":{"type":"mcp_auth_request","server":"github","call_id":"auth-1","auth_url":"https://github.com/login/oauth/authorize?client_id=abc","instructions":"Visit the URL to grant access."}}"#,
            r#"{"timestamp":"2026-07-01T10:00:05Z","type":"event_msg","payload":{"type":"mcp_auth_result","server":"github","call_id":"auth-1","status":"authenticated"}}"#,
            r#"{"timestamp":"2026-07-01T10:00:06Z","type":"response_item","payload":{"type":"mcp_tool_call","call_id":"mcp-1","server":"github","tool":"get_repo","arguments":{"owner":"openai","repo":"codex"}}}"#,
            r#"{"timestamp":"2026-07-01T10:00:07Z","type":"response_item","payload":{"type":"mcp_tool_call_output","call_id":"mcp-1","output":"Repository info..."}}"#,
            r#"{"timestamp":"2026-07-01T10:00:08Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1751360408.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "event_msg",
            "event_msg",
            "response_item",
            "response_item",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.144.0");
        assert_eq!(meta.payload["id"], "v0144-session");
    }

    #[test]
    fn v0142_response_item_without_metadata_turn_id_is_backward_compatible() {
        // Pre-v0.142.2 response items carry no metadata.turn_id — must parse normally.
        let line = r#"{"timestamp":"2026-06-25T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#;
        let e = RawEntry::parse(line).expect("response_item without metadata.turn_id must parse");
        assert_eq!(e.entry_type, "response_item");
        assert!(
            e.payload.get("metadata").is_none(),
            "metadata must be absent in pre-v0.142.2 entries"
        );
    }

    #[test]
    fn v0142_2_all_standard_entry_types_parse_correctly_with_metadata_turn_id() {
        // Regression guard: all standard entry types from a v0.142.2 session must parse.
        // response_items carry metadata.turn_id; other entry types are unchanged.
        let lines = [
            r#"{"timestamp":"2026-06-25T10:00:00Z","type":"session_meta","payload":{"id":"v0142-session","timestamp":"2026-06-25T10:00:00Z","cwd":"/project","cli_version":"0.142.2","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-25T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-25T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello","metadata":{"turn_id":"turn-1","trace_id":"trace-001"}}}"#,
            r#"{"timestamp":"2026-06-25T10:00:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project"}}"#,
            r#"{"timestamp":"2026-06-25T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1751000004.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "turn_context",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.142.2");
        // metadata.turn_id must be accessible on response_item payload
        let msg_entry = RawEntry::parse(lines[2]).unwrap();
        assert_eq!(msg_entry.payload["metadata"]["turn_id"], "turn-1");
    }

    // Codex v0.144.0 (PR #28772): MCP tools can now request interactive authentication by
    // default without requiring an experimental opt-in flag. When an MCP server requires
    // credentials (e.g. OAuth), it emits mcp_auth_challenge at the start of the interactive
    // flow and mcp_auth_complete once the user has authenticated. These event_msg types were
    // previously rare because they required an opt-in; v0.144.0 makes them standard so
    // sessions with active MCP servers will regularly contain them.

    #[test]
    fn v0144_mcp_auth_challenge_event_msg_parses_correctly() {
        let line = r#"{"timestamp":"2026-07-09T10:00:02Z","type":"event_msg","payload":{"type":"mcp_auth_challenge","server":"github","auth_url":"https://github.com/login/oauth/authorize?client_id=abc123","call_id":"mcp-auth-1"}}"#;
        let e = RawEntry::parse(line).expect("mcp_auth_challenge event_msg must parse");
        assert_eq!(e.entry_type, "event_msg");
        assert_eq!(event_msg_type(&e.payload), Some("mcp_auth_challenge"));
        assert_eq!(e.payload["server"], "github");
        assert_eq!(e.payload["call_id"], "mcp-auth-1");
    }

    #[test]
    fn v0144_mcp_auth_complete_event_msg_parses_correctly() {
        let line = r#"{"timestamp":"2026-07-09T10:00:05Z","type":"event_msg","payload":{"type":"mcp_auth_complete","server":"github","call_id":"mcp-auth-1","success":true}}"#;
        let e = RawEntry::parse(line).expect("mcp_auth_complete event_msg must parse");
        assert_eq!(e.entry_type, "event_msg");
        assert_eq!(event_msg_type(&e.payload), Some("mcp_auth_complete"));
        assert_eq!(e.payload["server"], "github");
        assert_eq!(e.payload["success"], true);
    }

    #[test]
    fn v0144_all_standard_entry_types_parse_correctly() {
        // Regression guard: standard entry types from a v0.144.0 session with an MCP
        // interactive auth flow must all parse without errors.
        let lines = [
            r#"{"timestamp":"2026-07-09T10:00:00Z","type":"session_meta","payload":{"id":"v0144-session","timestamp":"2026-07-09T10:00:00Z","cwd":"/project","cli_version":"0.144.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-07-09T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-07-09T10:00:02Z","type":"event_msg","payload":{"type":"mcp_auth_challenge","server":"github","auth_url":"https://github.com/login/oauth/authorize?client_id=abc123","call_id":"mcp-auth-1"}}"#,
            r#"{"timestamp":"2026-07-09T10:00:05Z","type":"event_msg","payload":{"type":"mcp_auth_complete","server":"github","call_id":"mcp-auth-1","success":true}}"#,
            r#"{"timestamp":"2026-07-09T10:00:06Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Authenticated with GitHub."}}"#,
            r#"{"timestamp":"2026-07-09T10:00:07Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project"}}"#,
            r#"{"timestamp":"2026-07-09T10:00:08Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1752055208.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "event_msg",
            "event_msg",
            "response_item",
            "turn_context",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.144.0");
        assert_eq!(meta.payload["id"], "v0144-session");
        // Verify auth event fields are accessible
        let challenge = RawEntry::parse(lines[2]).unwrap();
        assert_eq!(
            event_msg_type(&challenge.payload),
            Some("mcp_auth_challenge")
        );
        assert_eq!(challenge.payload["server"], "github");
        let complete = RawEntry::parse(lines[3]).unwrap();
        assert_eq!(event_msg_type(&complete.payload), Some("mcp_auth_complete"));
        assert_eq!(complete.payload["success"], true);
    }

    // Codex v0.147.0 (PR #36054): the deprecated `codex exec --full-auto` CLI flag
    // has been removed entirely; `--sandbox workspace-write` is now the only way to
    // get that behavior. codex-trace never invokes the Codex CLI and never passes
    // this flag to any process — it only reads `permission_profile` as recorded
    // session metadata, which is unrelated to the removed CLI flag and keeps its
    // existing string values (including the historical "full-auto" value). Verify
    // all four standard entry types from a v0.147.0 session still parse correctly
    // and that a "full-auto" permission profile value is read as data, unaffected
    // by the flag's removal.
    #[test]
    fn v0147_full_auto_flag_removal_does_not_affect_session_meta_parsing() {
        let lines = [
            r#"{"timestamp":"2026-08-01T10:00:00Z","type":"session_meta","payload":{"id":"v0147-session","timestamp":"2026-08-01T10:00:00Z","cwd":"/project","cli_version":"0.147.0","model_provider":"openai","permission_profile":"full-auto"}}"#,
            r#"{"timestamp":"2026-08-01T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-08-01T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
            r#"{"timestamp":"2026-08-01T10:00:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project"}}"#,
            r#"{"timestamp":"2026-08-01T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1785578404.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "turn_context",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.147.0");
        assert_eq!(meta.payload["permission_profile"], "full-auto");
    }
}
