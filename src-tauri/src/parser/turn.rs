use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use super::entry::{parse_timestamp_secs, RawEntry};
use super::mentions::{decode_task_mentions, TaskMentionRef};
use super::spawn::parse_spawn_agent_output;
use super::toolcall::{ToolCall, ToolCallBuilder};

/// Codex's own sentinel text for a synthetic `agent_message` event it inserts at the end of
/// the transcript it copies in from an imported Claude/Cursor conversation (pre-v0.140.0
/// external-agent import feature). Codex v0.147.0 (PR #36356, issue #221) added support for
/// syncing further edits into that same imported thread rather than creating a duplicate, so
/// this marker can now sit mid-transcript — right before the newly-synced turns — instead of
/// only ever trailing a finished import. It carries no real content and must never be
/// rendered as an assistant message.
const EXTERNAL_SESSION_IMPORTED_MARKER: &str = "<EXTERNAL SESSION IMPORTED>";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Complete,
    Aborted,
    Cancelled,
    Ongoing,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMsg {
    pub text: String,
    pub phase: Option<String>,
    pub timestamp: String,
    pub is_reasoning: bool,
    /// Position of this message in the raw entry stream. Tool calls carry a parallel index
    /// (see `CodexTurn::tool_call_orders`) drawn from the same counter, so the frontend can
    /// interleave messages and tool calls in true chronological order instead of rendering
    /// them in separate blocks.
    #[serde(default)]
    pub order: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenInfo {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub total_tokens: u64,
    pub context_window_tokens: Option<u64>,
    pub model_context_window: u64,
}

/// A single memory summary item from a `turn_context` memories payload.
///
/// Codex v0.132.0 (PR #23148) made memory summaries versioned: items are now
/// objects `{"content":"...","version":1}` instead of plain strings.
/// Pre-v0.132.0 sessions carry plain strings; `version` is `None` for those.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemorySummary {
    pub content: String,
    /// Format version. None for pre-v0.132.0 sessions (plain-string format).
    pub version: Option<u32>,
}

/// Compaction metadata embedded in turn headers (Codex v0.135.0, PR #24368).
/// Captures the state of context compaction at the start of a turn so that
/// context-window accounting in traces remains accurate even after compaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionMeta {
    /// Context-window tokens present before compaction.
    pub tokens_before: Option<u64>,
    /// Context-window tokens remaining after compaction.
    pub tokens_after: Option<u64>,
    /// Optional human-readable summary of what was compacted.
    pub summary: Option<String>,
    /// What triggered the compaction: `"auto"` (threshold-based) or `"manual"` (user-requested).
    /// Null for sessions that predate this field.
    pub compaction_trigger: Option<String>,
    /// Codex v0.142.0 (PR #29256): opaque ID linking this context window to its compaction
    /// ancestor, enabling lineage reconstruction across compaction boundaries.
    /// Null for sessions predating v0.142.0.
    pub lineage_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollabSpawn {
    pub call_id: String,
    pub new_session_id: String,
    pub agent_nickname: String,
    pub agent_role: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub prompt_preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextElement {
    pub placeholder: String,
    pub byte_range: ByteRange,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexTurn {
    pub turn_id: String,
    pub started_at: Option<u64>,
    pub completed_at: Option<u64>,
    pub duration_ms: Option<u64>,
    pub status: TurnStatus,
    pub user_message: Option<String>,
    pub text_elements: Vec<TextElement>,
    pub agent_messages: Vec<AgentMsg>,
    pub tool_calls: Vec<ToolCall>,
    /// Display-order index for each tool call, parallel to `tool_calls` (same length, same
    /// order). The value is the position of the call's first appearance in the raw entry
    /// stream, matching `AgentMsg::order`, so the frontend can interleave tool calls with
    /// agent messages instead of dumping all tool calls at the end of the turn.
    #[serde(default)]
    pub tool_call_orders: Vec<usize>,
    pub final_answer: Option<String>,
    pub total_tokens: Option<TokenInfo>,
    pub model: Option<String>,
    pub cwd: Option<String>,
    pub reasoning_effort: Option<String>,
    pub error: Option<String>,
    pub aborted_reason: Option<String>,
    pub has_compaction: bool,
    pub thread_name: Option<String>,
    pub collab_spawns: Vec<CollabSpawn>,
    /// Codex v0.134.0 (PR #23980): OpenTelemetry trace ID from TurnStartedEvent.
    /// Null for sessions captured before v0.134.0.
    pub trace_id: Option<String>,
    /// Codex v0.135.0 (PR #24160): thread ID this turn was forked from, if any.
    /// Null for turns that are not forks of another thread.
    pub forked_from_thread_id: Option<String>,
    /// Codex v0.135.0 (PR #24368): compaction metadata present at turn start.
    /// Null when the turn header carries no compaction info (pre-v0.135.0 sessions).
    pub compaction_meta: Option<CompactionMeta>,
    /// Active memories injected into context at turn start (Codex v0.135.0+, PR #24591).
    /// Items carry an optional version field (Codex v0.132.0+, PR #23148).
    /// Empty for sessions from older Codex versions.
    pub memories: Vec<MemorySummary>,
    /// Audio transcript segments from realtime voice turns (Codex v0.143.0+, PRs #29918, #30144;
    /// live mid-turn segments in Codex v0.145.0+ V3 streaming realtime audio).
    /// Empty for non-voice sessions.
    #[serde(default)]
    pub audio_transcript: Vec<String>,
    /// Warning messages emitted during the turn (Codex `EventMsg::Warning`), e.g. skill
    /// catalog budget/truncation notices (Codex v0.146.0+). Empty when no warnings occurred.
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Codex v0.150.0 (PRs #40308, #40315): tasks `@`-mentioned in `user_message` via the TUI
    /// composer's task-mention encoding, decoded by `super::mentions::decode_task_mentions`.
    /// Empty for turns with no task mentions and for pre-v0.150.0 sessions.
    #[serde(default)]
    pub task_mentions: Vec<TaskMentionRef>,
}

impl CodexTurn {
    pub fn new(turn_id: String) -> Self {
        CodexTurn {
            turn_id,
            started_at: None,
            completed_at: None,
            duration_ms: None,
            status: TurnStatus::Ongoing,
            user_message: None,
            text_elements: Vec::new(),
            agent_messages: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_orders: Vec::new(),
            final_answer: None,
            total_tokens: None,
            model: None,
            cwd: None,
            reasoning_effort: None,
            error: None,
            aborted_reason: None,
            has_compaction: false,
            thread_name: None,
            collab_spawns: Vec::new(),
            trace_id: None,
            forked_from_thread_id: None,
            compaction_meta: None,
            memories: Vec::new(),
            audio_transcript: Vec::new(),
            warnings: Vec::new(),
            task_mentions: Vec::new(),
        }
    }
}

/// Build turns from a sequence of raw entries.
/// Handles both new format (task_started/task_complete) and old format (user_message-bounded).
pub fn build_turns(entries: &[RawEntry]) -> Vec<CodexTurn> {
    let mut turns: indexmap::IndexMap<String, CodexTurn> = indexmap::IndexMap::new();
    let mut current_turn_id: Option<String> = None;
    let mut tool_builders: HashMap<String, ToolCallBuilder> = HashMap::new();

    // Detect format: new (has task_started) vs old (user_message-bounded)
    let has_task_started = entries.iter().any(|e| {
        e.entry_type == "event_msg"
            && e.payload.get("type").and_then(|t| t.as_str()) == Some("task_started")
    });

    let mut synthetic_turn_counter = 0u32;
    // Position of each tool call's first appearance in the raw stream, keyed by call_id.
    // Gives tool calls the same kind of order index as agent messages so the two can be
    // interleaved chronologically in the UI.
    let mut call_order: HashMap<String, usize> = HashMap::new();

    for (index, entry) in entries.iter().enumerate() {
        if let Some(call_id) = call_id_of(entry) {
            call_order.entry(call_id).or_insert(index);
        }
        match entry.entry_type.as_str() {
            "event_msg" => {
                handle_event_msg(
                    entry,
                    &mut turns,
                    &mut current_turn_id,
                    &mut tool_builders,
                    has_task_started,
                    &mut synthetic_turn_counter,
                    index,
                );
            }
            "response_item"
            | "function_call"
            | "function_call_output"
            | "message"
            | "reasoning" => {
                handle_response_item(entry, &mut turns, &current_turn_id, &mut tool_builders);
            }
            "turn_context" => {
                handle_turn_context(entry, &mut turns, &current_turn_id);
            }
            "compacted" => {
                if let Some(ref tid) = current_turn_id {
                    if let Some(turn) = turns.get_mut(tid) {
                        turn.has_compaction = true;
                    }
                }
            }
            _ => {}
        }
    }

    // Finalize all tool builders
    for (turn_id, mut builder) in tool_builders {
        builder.drain_pending();
        if let Some(turn) = turns.get_mut(&turn_id) {
            // Record each call's stream position, parallel to tool_calls. Calls with no
            // recorded position (should not happen for well-formed sessions) sort last.
            for tc in &builder.finalized {
                let order = call_order.get(&tc.call_id).copied().unwrap_or(usize::MAX);
                turn.tool_call_orders.push(order);
            }
            turn.tool_calls.extend(builder.finalized);
        }
    }

    let mut result: Vec<CodexTurn> = turns.into_values().collect();
    result.sort_by_key(|t| t.started_at.unwrap_or(0));
    result
}

/// Extract a tool call's `call_id` from a raw entry, checking both the parsed payload and the
/// raw line (different entry shapes carry it in different places). Returns None for entries not
/// associated with a tool call.
fn call_id_of(entry: &RawEntry) -> Option<String> {
    for v in [&entry.payload, &entry.raw] {
        if let Some(cid) = v
            .get("call_id")
            .and_then(|c| c.as_str())
            .filter(|s| !s.is_empty())
        {
            return Some(cid.to_string());
        }
    }
    None
}

fn handle_event_msg(
    entry: &RawEntry,
    turns: &mut indexmap::IndexMap<String, CodexTurn>,
    current_turn_id: &mut Option<String>,
    tool_builders: &mut HashMap<String, ToolCallBuilder>,
    has_task_started: bool,
    synthetic_counter: &mut u32,
    index: usize,
) {
    let payload = &entry.payload;
    let msg_type = match payload.get("type").and_then(|t| t.as_str()) {
        Some(t) => t,
        None => return,
    };
    let ts = entry.timestamp.as_deref().unwrap_or("");

    match msg_type {
        "task_started" => {
            let turn_id = payload
                .get("turn_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if turn_id.is_empty() {
                return;
            }
            // Prefer turn_start_timestamp from payload (added in Codex v0.128.0 via #19473).
            // Fall back to the outer JSONL line timestamp for sessions captured before that.
            let started_at = payload
                .get("turn_start_timestamp")
                .and_then(|v| v.as_f64())
                .map(|v| v as u64)
                .or_else(|| entry.timestamp.as_deref().and_then(parse_timestamp_secs));
            let mut turn = CodexTurn::new(turn_id.clone());
            turn.started_at = started_at;
            // Codex v0.134.0 (PR #23980): trace_id for OTel correlation.
            turn.trace_id = payload
                .get("trace_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            // Codex v0.135.0 (PR #24160): forked_from_thread_id for session-tree reconstruction.
            turn.forked_from_thread_id = payload
                .get("forked_from_thread_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            // Codex v0.135.0 (PR #24368): compaction metadata for context-window accounting.
            turn.compaction_meta = payload.get("compaction").map(|c| CompactionMeta {
                tokens_before: c.get("tokens_before").and_then(|v| v.as_u64()),
                tokens_after: c.get("tokens_after").and_then(|v| v.as_u64()),
                summary: c
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string()),
                compaction_trigger: c
                    .get("compaction_trigger")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string()),
                lineage_id: c
                    .get("lineage_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string()),
            });
            turns.insert(turn_id.clone(), turn);
            *current_turn_id = Some(turn_id.clone());
            let builder = tool_builders
                .entry(turn_id)
                .or_insert_with(ToolCallBuilder::new);
            // Codex v0.141.0+ (PRs #27365, #27371): parse dynamic_tools and build a tool
            // registry so namespaced MCP/connector tools can be classified correctly.
            let registry = parse_dynamic_tools(payload);
            if !registry.is_empty() {
                builder.set_dynamic_tool_registry(registry);
            }
        }

        "user_message" => {
            let raw_message = payload
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            // Codex v0.150.0 (PRs #40308, #40315): task `@`-mentions from the TUI composer are
            // encoded inline into the message text; decode back to readable `@Title` text and
            // collect the referenced tasks.
            let (message, task_mentions) = decode_task_mentions(raw_message);

            if !has_task_started {
                // Old format: each user_message starts a new turn
                *synthetic_counter += 1;
                let turn_id = format!("turn-{synthetic_counter}");
                let started_at = entry.timestamp.as_deref().and_then(parse_timestamp_secs);
                let mut turn = CodexTurn::new(turn_id.clone());
                turn.started_at = started_at;
                turn.user_message = Some(message);
                turn.task_mentions = task_mentions;
                turns.insert(turn_id.clone(), turn);
                *current_turn_id = Some(turn_id.clone());
                tool_builders
                    .entry(turn_id)
                    .or_insert_with(ToolCallBuilder::new);
            } else if let Some(ref tid) = current_turn_id {
                if let Some(turn) = turns.get_mut(tid) {
                    if turn.user_message.is_none() {
                        turn.user_message = Some(message);
                        turn.task_mentions = task_mentions;
                    }
                }
            }
        }

        "agent_message" => {
            if let Some(ref tid) = current_turn_id {
                if let Some(turn) = turns.get_mut(tid) {
                    let text = payload
                        .get("message")
                        .map(extract_message_text)
                        .unwrap_or_default();
                    if !text.is_empty() && text != EXTERNAL_SESSION_IMPORTED_MARKER {
                        let phase = payload
                            .get("phase")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let is_final = phase.as_deref() == Some("final_answer");
                        if is_final && turn.final_answer.is_none() {
                            turn.final_answer = Some(text.clone());
                        }
                        turn.agent_messages.push(AgentMsg {
                            text,
                            phase,
                            timestamp: ts.to_string(),
                            is_reasoning: false,
                            order: index,
                        });
                    }
                }
            }
        }

        "agent_reasoning" => {
            if let Some(ref tid) = current_turn_id {
                if let Some(turn) = turns.get_mut(tid) {
                    let text = payload
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if !text.is_empty() {
                        turn.agent_messages.push(AgentMsg {
                            text,
                            phase: None,
                            timestamp: ts.to_string(),
                            is_reasoning: true,
                            order: index,
                        });
                    }
                }
            }
        }

        "task_complete" => {
            let turn_id = payload
                .get("turn_id")
                .and_then(|v| v.as_str())
                .unwrap_or(current_turn_id.as_deref().unwrap_or(""))
                .to_string();
            if let Some(turn) = turns.get_mut(&turn_id) {
                turn.status = TurnStatus::Complete;
                // Prefer task_complete.last_agent_message as final_answer
                if let Some(last_msg) = payload
                    .get("last_agent_message")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    turn.final_answer = Some(last_msg.to_string());
                }
                turn.completed_at = payload
                    .get("completed_at")
                    .and_then(|v| v.as_f64())
                    .map(|v| v as u64)
                    .or_else(|| entry.timestamp.as_deref().and_then(parse_timestamp_secs));
                turn.duration_ms = payload.get("duration_ms").and_then(|v| v.as_u64());
                // Codex v0.128.0 adds prompt_tokens/completion_tokens/total_tokens to task_complete.
                // Use these only when no richer token_count event has already populated the turn.
                if turn.total_tokens.is_none() {
                    let prompt_tokens = payload.get("prompt_tokens").and_then(|v| v.as_u64());
                    let completion_tokens =
                        payload.get("completion_tokens").and_then(|v| v.as_u64());
                    let total = payload
                        .get("total_tokens")
                        .and_then(|v| v.as_u64())
                        .or_else(|| prompt_tokens.zip(completion_tokens).map(|(p, c)| p + c));
                    if let Some(total_tokens) = total {
                        turn.total_tokens = Some(TokenInfo {
                            input_tokens: prompt_tokens.unwrap_or(0),
                            cached_input_tokens: 0,
                            output_tokens: completion_tokens.unwrap_or(0),
                            reasoning_output_tokens: 0,
                            total_tokens,
                            context_window_tokens: None,
                            model_context_window: 0,
                        });
                    }
                }
            }
        }

        "turn_aborted" => {
            let turn_id_field = payload
                .get("turn_id")
                .and_then(|v| v.as_str())
                .unwrap_or(current_turn_id.as_deref().unwrap_or(""))
                .to_string();
            let target_id = if !turn_id_field.is_empty() {
                turn_id_field
            } else {
                current_turn_id.clone().unwrap_or_default()
            };
            if let Some(turn) = turns.get_mut(&target_id) {
                turn.status = TurnStatus::Aborted;
                turn.aborted_reason = payload
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                turn.completed_at = payload
                    .get("completed_at")
                    .and_then(|v| v.as_f64())
                    .map(|v| v as u64)
                    .or_else(|| entry.timestamp.as_deref().and_then(parse_timestamp_secs));
                turn.duration_ms = payload.get("duration_ms").and_then(|v| v.as_u64());
            }
        }

        // Codex v0.142.0 (PRs #28746, #28494, #28707, #29423, #29255): token budget events.
        // token_budget_abort fires when the rollout budget is fully exhausted; it terminates
        // the turn just like turn_aborted. The reason field carries "token_budget_exhausted"
        // or similar — record it so the UI can explain why the turn stopped.
        "token_budget_abort" => {
            let turn_id_field = payload
                .get("turn_id")
                .and_then(|v| v.as_str())
                .unwrap_or(current_turn_id.as_deref().unwrap_or(""))
                .to_string();
            let target_id = if !turn_id_field.is_empty() {
                turn_id_field
            } else {
                current_turn_id.clone().unwrap_or_default()
            };
            if let Some(turn) = turns.get_mut(&target_id) {
                turn.status = TurnStatus::Aborted;
                turn.aborted_reason = payload
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| Some("token_budget_exhausted".to_string()));
                turn.completed_at = payload
                    .get("completed_at")
                    .and_then(|v| v.as_f64())
                    .map(|v| v as u64)
                    .or_else(|| entry.timestamp.as_deref().and_then(parse_timestamp_secs));
                turn.duration_ms = payload.get("duration_ms").and_then(|v| v.as_u64());
            }
        }

        "inference_stream_cancelled" => {
            let turn_id_field = payload
                .get("turn_id")
                .and_then(|v| v.as_str())
                .unwrap_or(current_turn_id.as_deref().unwrap_or(""))
                .to_string();
            let target_id = if !turn_id_field.is_empty() {
                turn_id_field
            } else {
                current_turn_id.clone().unwrap_or_default()
            };
            if let Some(turn) = turns.get_mut(&target_id) {
                turn.status = TurnStatus::Cancelled;
                turn.completed_at = payload
                    .get("completed_at")
                    .and_then(|v| v.as_f64())
                    .map(|v| v as u64)
                    .or_else(|| entry.timestamp.as_deref().and_then(parse_timestamp_secs));
                turn.duration_ms = payload.get("duration_ms").and_then(|v| v.as_u64());
            }
        }

        "token_count" => {
            if let Some(ref tid) = current_turn_id {
                if let Some(turn) = turns.get_mut(tid) {
                    if let Some(info) = payload.get("info").filter(|v| !v.is_null()) {
                        if let Some(total) = info.get("total_token_usage") {
                            let context_window_tokens = info
                                .get("last_token_usage")
                                .and_then(|last| last.get("total_tokens"))
                                .and_then(|v| v.as_u64());
                            turn.total_tokens = Some(TokenInfo {
                                input_tokens: u64_field(total, "input_tokens"),
                                cached_input_tokens: u64_field(total, "cached_input_tokens"),
                                output_tokens: u64_field(total, "output_tokens"),
                                reasoning_output_tokens: u64_field(
                                    total,
                                    "reasoning_output_tokens",
                                ),
                                total_tokens: u64_field(total, "total_tokens"),
                                context_window_tokens,
                                model_context_window: u64_field(info, "model_context_window"),
                            });
                        }
                    }
                }
            }
        }

        "error" => {
            if let Some(ref tid) = current_turn_id {
                if let Some(turn) = turns.get_mut(tid) {
                    let msg = payload
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    turn.status = TurnStatus::Error;
                    turn.error = Some(msg);
                }
            }
        }

        // Codex v0.146.0: skill catalog rendering emits `EventMsg::Warning` when the
        // catalog is truncated or skills are omitted for budget reasons. `warning` is a
        // generic notice event (also used for e.g. model-reroute warnings), so surface
        // its message on the current turn rather than dropping it.
        "warning" => {
            if let Some(ref tid) = current_turn_id {
                if let Some(turn) = turns.get_mut(tid) {
                    let msg = payload
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if !msg.is_empty() {
                        turn.warnings.push(msg);
                    }
                }
            }
        }

        "exec_command_end" => {
            if let Some(ref tid) = current_turn_id {
                let builder = tool_builders
                    .entry(tid.clone())
                    .or_insert_with(ToolCallBuilder::new);
                builder.finalize_exec(msg_type, payload);
            }
        }

        "mcp_tool_call_end" => {
            if let Some(ref tid) = current_turn_id {
                let builder = tool_builders
                    .entry(tid.clone())
                    .or_insert_with(ToolCallBuilder::new);
                builder.finalize_mcp(msg_type, payload);
            }
        }

        "patch_apply_end" => {
            if let Some(ref tid) = current_turn_id {
                let builder = tool_builders
                    .entry(tid.clone())
                    .or_insert_with(ToolCallBuilder::new);
                builder.finalize_patch(msg_type, payload);
            }
        }

        "web_search_end" => {
            if let Some(ref tid) = current_turn_id {
                let builder = tool_builders
                    .entry(tid.clone())
                    .or_insert_with(ToolCallBuilder::new);
                builder.add_web_search(msg_type, payload);
            }
        }

        "collab_agent_spawn_end" => {
            if let Some(ref tid) = current_turn_id {
                // Record collab spawn metadata
                if let Some(turn) = turns.get_mut(tid) {
                    let call_id = str_field(payload, "call_id");
                    // v0.131.0+ (PR #22268): field renamed new_thread_id → new_session_id
                    let new_session_id = payload
                        .get("new_session_id")
                        .or_else(|| payload.get("new_thread_id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let agent_nickname = str_field(payload, "new_agent_nickname");
                    let agent_role = str_field(payload, "new_agent_role");
                    let model = payload
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let reasoning_effort = payload
                        .get("reasoning_effort")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let prompt = payload.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
                    let prompt_preview = prompt.chars().take(200).collect();

                    turn.collab_spawns.push(CollabSpawn {
                        call_id: call_id.clone(),
                        new_session_id,
                        agent_nickname,
                        agent_role,
                        model,
                        reasoning_effort,
                        prompt_preview,
                    });
                }

                let builder = tool_builders
                    .entry(tid.clone())
                    .or_insert_with(ToolCallBuilder::new);
                builder.finalize_spawn(msg_type, payload);
            }
        }

        "collab_waiting_end" => {
            if let Some(ref tid) = current_turn_id {
                let builder = tool_builders
                    .entry(tid.clone())
                    .or_insert_with(ToolCallBuilder::new);
                builder.finalize_wait(msg_type, payload);
            }
        }

        "collab_close_end" => {
            if let Some(ref tid) = current_turn_id {
                let builder = tool_builders
                    .entry(tid.clone())
                    .or_insert_with(ToolCallBuilder::new);
                builder.finalize_close(msg_type, payload);
            }
        }

        other if other.ends_with("_end") => {
            if let Some(ref tid) = current_turn_id {
                let builder = tool_builders
                    .entry(tid.clone())
                    .or_insert_with(ToolCallBuilder::new);
                builder.finalize_unknown_end(other, payload);
            }
        }

        // Codex v0.136.0 (PR #24962): shell hook outputs from pre/post-tool lifecycle hooks.
        // The tightened schema requires call_id, hook_type, stdout, and exit_code; previously
        // nullable fields (metadata, stderr) are now absent rather than null. Parse only the
        // stable v0.136.0 fields so both old and new payloads deserialize correctly.
        "shell_hook_output" => {
            if let Some(ref tid) = current_turn_id {
                let builder = tool_builders
                    .entry(tid.clone())
                    .or_insert_with(ToolCallBuilder::new);
                builder.finalize_shell_hook(payload);
            }
        }

        "thread_name_updated" => {
            if let Some(ref tid) = current_turn_id {
                if let Some(turn) = turns.get_mut(tid) {
                    turn.thread_name = payload
                        .get("thread_name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
            }
        }

        // Codex v0.133.0 (PRs #23300, #23685, #23696, #23732): Goals feature enabled by
        // default. Goal lifecycle events are emitted as event_msg turn items in the session
        // stream. codex-trace does not model goal state — these events are intentionally
        // skipped so they don't corrupt turn data.
        "goal_created" | "goal_updated" | "goal_completed" | "goal_paused" => {}

        // Codex v0.140.0 (PRs #27070, #27071, #27703): /import command emits event_msg
        // lifecycle events as it imports agent context into the session. These events modify
        // the thread's initial context but carry no turn-building semantics for codex-trace
        // — they are treated as pass-through so sessions beginning with imported context
        // parse correctly without corrupting turn state.
        "agent_context_import_started"
        | "agent_context_imported"
        | "external_agent_import_started"
        | "external_agent_imported" => {}

        // Codex v0.142.0 (PRs #28746, #28494, #28707, #29423, #29255): token budget events.
        // token_budget_reminder is emitted when usage crosses a threshold percentage.
        // No turn-building semantics — silently skip.
        "token_budget_reminder" => {}

        // Codex v0.144.0 (PR #28772): MCP tools can now request interactive authentication
        // by default, without requiring an experimental opt-in flag. Sessions with MCP tools
        // that need auth emit these events during the OAuth / API-key handshake.
        // codex-trace does not model MCP auth state — these events carry no turn-building
        // semantics and are explicitly skipped so ordinary sessions with auth flows parse
        // without corrupting turn data.
        "mcp_auth_request" | "mcp_auth_result" | "mcp_auth_challenge" | "mcp_auth_complete" => {}

        // Codex v0.143.0 (PRs #29918, #30144): trailing realtime transcript text preserved
        // during shutdown. Codex v0.145.0 reintroduces streaming realtime V3 audio and emits
        // this event type mid-turn (live voice segments, not just shutdown-time trailing text).
        // Extract the transcript so voice-turn content is surfaced rather than silently dropped.
        "realtime_transcript_text" => {
            let text = payload
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !text.is_empty() {
                let target_id = payload
                    .get("turn_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .or_else(|| current_turn_id.clone());
                if let Some(ref tid) = target_id {
                    if let Some(turn) = turns.get_mut(tid) {
                        turn.audio_transcript.push(text);
                    }
                }
            }
        }

        _ => {}
    }
}

fn handle_response_item(
    entry: &RawEntry,
    turns: &mut indexmap::IndexMap<String, CodexTurn>,
    current_turn_id: &Option<String>,
    tool_builders: &mut HashMap<String, ToolCallBuilder>,
) {
    let payload = if entry.entry_type == "response_item" {
        &entry.payload
    } else {
        &entry.raw
    };

    let item_type = match payload.get("type").and_then(|t| t.as_str()) {
        Some(t) => t,
        None => return,
    };

    // Codex v0.142.2 (PR #28360): turn_id is now stored in ResponseItem metadata.
    // Prefer the explicit metadata.turn_id over positional current_turn_id — the
    // metadata field allows correct correlation even when response items are emitted
    // for non-contiguous turns.
    let metadata_turn_id = payload
        .get("metadata")
        .and_then(|m| m.get("turn_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    // Codex Desktop v0.153 stores the same correlation field under
    // internal_chat_message_metadata_passthrough rather than metadata.
    let metadata_turn_id = metadata_turn_id.or_else(|| {
        payload
            .get("internal_chat_message_metadata_passthrough")
            .and_then(|m| m.get("turn_id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    });

    let tid: &str = match metadata_turn_id.as_deref().or(current_turn_id.as_deref()) {
        Some(t) => t,
        None => return,
    };

    let builder = tool_builders
        .entry(tid.to_string())
        .or_insert_with(ToolCallBuilder::new);

    match item_type {
        "function_call" => {
            let call_id = str_field(payload, "call_id");
            let name = str_field(payload, "name");
            // Codex v0.139.0 (PRs #24118, #27084): tool/connector input schemas now preserve
            // oneOf and allOf structures. arguments may arrive as a JSON object rather than a
            // stringified JSON string — handle both forms so complex schema structures are not
            // silently dropped (matching the existing mcp_tool_call handler behaviour).
            let arguments_str = match payload.get("arguments") {
                Some(Value::String(s)) => s.clone(),
                Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()),
                None => "{}".to_string(),
            };
            let namespace = payload
                .get("namespace")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            // v0.130.0+ (PR #21454): string-keyed MCP tool maps removed; function_call
            // entries now carry tool_id: { server, tool } instead of a flat namespace string.
            // Store the server directly to avoid parse_mcp_namespace misinterpreting it.
            // v0.133.0+ (PRs #23353, #23737): tool_id also carries plugin_id.
            let tool_id = payload.get("tool_id");
            let mcp_server_direct = if namespace.is_none() {
                tool_id
                    .and_then(|tid| tid.get("server"))
                    .and_then(|s| s.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            } else {
                None
            };
            let plugin_id = tool_id
                .and_then(|tid| tid.get("plugin_id"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            builder.add_function_call(
                call_id,
                name,
                &arguments_str,
                namespace,
                mcp_server_direct,
                plugin_id,
            );
        }

        "function_call_output" => {
            let call_id = str_field(payload, "call_id");
            let output = match payload.get("output") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(arr)) => arr
                    .iter()
                    .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            };
            // Codex v0.138.0 (PRs #25944, #25947): image_generation and local image attachment
            // results now include a top-level file_path field exposing the saved file path.
            let file_path = payload
                .get("file_path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            if let Some(spawn) = spawn_from_function_call_output(builder, &call_id, &output) {
                if let Some(turn) = turns.get_mut(tid) {
                    turn.collab_spawns.push(spawn);
                }
            }
            builder.add_function_call_output(&call_id, &output, file_path);
        }

        "custom_tool_call" => {
            let call_id = str_field(payload, "call_id");
            let name = str_field(payload, "name");
            let input = payload
                .get("input")
                .and_then(custom_tool_text)
                .filter(|s| !s.is_empty());
            builder.add_custom_tool_call(call_id, name, input);
        }

        "custom_tool_call_output" => {
            let call_id = str_field(payload, "call_id");
            // Legacy Codex writes `output` as a JSON string containing `output`
            // and optional `metadata.exit_code`. Codex Desktop v0.153+ writes a
            // structured array of text blocks instead.
            let (output, exit_code) = extract_custom_tool_output(payload);
            builder.finalize_custom_tool_output(&call_id, &output, exit_code);
        }

        // Codex v0.129.0 (PR #20540): apply_patch file changes moved from the
        // patch_apply_end event_msg into this turn item. Backfill the result onto the
        // PatchApply call that custom_tool_call_output already finalized.
        "apply_patch_end" => {
            let call_id = str_field(payload, "call_id");
            let success = payload.get("success").and_then(|v| v.as_bool());
            let changes = payload.get("changes").cloned();
            builder.backfill_patch_result(&call_id, success, changes);
        }

        // Codex v0.142.2 (PR #29486): MCP tools are now discovered via tool-search calls
        // rather than enumerated upfront in task_started.dynamic_tools. The model issues a
        // tool_search_call mid-turn; matching tools are returned in tool_search_call_output.
        // Merge the discovered tools into the current turn's dynamic tool registry so
        // subsequent function_call entries are classified as McpTool correctly.
        //
        // Codex v0.147.0 (issue #222): the bundled MCP SDK bump to 3.0.0 (#36001) and the
        // opt-in MCP 2026-07-28 protocol (#35724, #35725, #35590, #35742) were audited against
        // the openai/codex source. Both only change the wire protocol between Codex and MCP
        // *servers* (SDK type renames, paginated `server/discover` requests, multi-round
        // tools/call negotiation, non-blocking server startup) — that pagination is resolved
        // internally before Codex ever builds `dynamic_tools` or tool-search results. Neither
        // change touches `codex-rs/protocol` (rollout item types), `app-server/src/dynamic_tools.rs`,
        // or the tool_search_call/tool_search_call_output mechanism handled here, so the
        // model-facing discovery flow below (and issue #161's fix) is unaffected. No rollout
        // JSONL shape change; no parser change needed.
        "tool_search_call" => {
            // Search request — tool definitions arrive in the paired tool_search_call_output.
        }

        "tool_search_call_output" => {
            let extra = parse_tool_search_results(payload);
            if !extra.is_empty() {
                builder.extend_dynamic_tool_registry(extra);
            }
        }

        // Codex v0.129.0 (PR #20677): MCP tool calls are now emitted as first-class
        // response_item turn entries with dedicated types instead of reusing function_call
        // with an mcp__ namespace. Wire them into the existing ToolCallBuilder paths so
        // they are classified correctly as McpTool rather than silently discarded.
        "mcp_tool_call" => {
            let call_id = str_field(payload, "call_id");
            let server = payload.get("server").and_then(|v| v.as_str()).unwrap_or("");
            let tool = payload.get("tool").and_then(|v| v.as_str()).unwrap_or("");
            // Use the tool name directly; namespace carries the server for McpTool classification.
            let name = if !tool.is_empty() {
                tool.to_string()
            } else {
                str_field(payload, "name")
            };
            let namespace = if !server.is_empty() {
                Some(format!("mcp__{server}"))
            } else {
                payload
                    .get("namespace")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            };
            // arguments may be a JSON object (not a string) in the new format.
            let arguments_str = match payload.get("arguments") {
                Some(Value::String(s)) => s.clone(),
                Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()),
                None => "{}".to_string(),
            };
            // v0.133.0+ (PRs #23353, #23737): plugin_id field identifies which plugin
            // the MCP tool belongs to. Absent for older sessions.
            let plugin_id = payload
                .get("plugin_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            builder.add_function_call(call_id, name, &arguments_str, namespace, None, plugin_id);
        }

        "mcp_tool_call_output" => {
            let call_id = str_field(payload, "call_id");
            // output may be a content array [{"type":"text","text":"..."}] or a plain string.
            let output = match payload.get("output") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(arr)) => arr
                    .iter()
                    .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            };
            builder.add_function_call_output(&call_id, &output, None);
        }

        // Codex < v0.133.0 (PR #23075 removed UserTurn): user input was emitted as a
        // response_item with type "user_turn" rather than a "user_message" event_msg.
        // Migrate by extracting the message text and storing it on the current turn.
        "user_turn" => {
            if let Some(turn) = turns.get_mut(tid) {
                if turn.user_message.is_none() {
                    let text = extract_content_text(payload);
                    if !text.is_empty() {
                        turn.user_message = Some(text);
                    }
                }
            }
        }

        // Codex < v0.133.0 (PR #23081 removed UserInputWithTurnContext): combined entry
        // bundling user input and turn context into one response_item. Apply both: extract
        // the user message from the "input" sub-field and update context fields from "context".
        "user_input_with_turn_context" => {
            if let Some(turn) = turns.get_mut(tid) {
                if turn.user_message.is_none() {
                    let input = payload.get("input").unwrap_or(payload);
                    let text = extract_content_text(input);
                    if !text.is_empty() {
                        turn.user_message = Some(text);
                    }
                }
                if let Some(ctx) = payload.get("context") {
                    if turn.model.is_none() {
                        turn.model = ctx
                            .get("model")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                    }
                    if turn.cwd.is_none() {
                        turn.cwd = ctx
                            .get("cwd")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                    }
                    if turn.reasoning_effort.is_none() {
                        turn.reasoning_effort = ctx
                            .get("effort")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                    }
                }
            }
        }

        // Codex v0.133.0+ (PRs #23080, #22508): UserTurn and UserInputWithTurnContext were
        // replaced by a split UserInput + ThreadSettings model. UserInput carries the user's
        // message text; ThreadSettings carries per-turn config (model, cwd, effort).
        "user_input" => {
            if let Some(turn) = turns.get_mut(tid) {
                if turn.user_message.is_none() {
                    let text = extract_content_text(payload);
                    if !text.is_empty() {
                        turn.user_message = Some(text);
                    }
                }
            }
        }

        // Codex v0.133.0+ (PRs #23080, #22508): ThreadSettings carries per-turn context
        // (model, cwd, effort) that was previously bundled inside UserInputWithTurnContext.
        "thread_settings" => {
            if let Some(turn) = turns.get_mut(tid) {
                if turn.model.is_none() {
                    turn.model = payload
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
                if turn.cwd.is_none() {
                    turn.cwd = payload
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
                if turn.reasoning_effort.is_none() {
                    turn.reasoning_effort = payload
                        .get("effort")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
            }
        }

        // Codex v0.140.0 (PRs #27070, #27071, #27703): /import command imports setup, project
        // config, and recent chats from external agents (e.g. Claude Code). The import flow
        // writes response_items into the session transcript describing imported threads. These
        // items carry no turn-building semantics for codex-trace — treated as pass-through.
        //
        // Codex v0.141.0 (PR #28008): external_agent_import_result is an accounting-type
        // response_item that records token/cost totals for an imported agent context.
        "external_agent_import_result" => {}

        // Pre-Codex v0.140.0 archive-only response_items from the experimental /realtime
        // voice subsystem removed in PR #27801. Will never appear in sessions recorded
        // with Codex ≥ v0.140.0, but old archives may still contain them. Silently skip.
        "speech_append" | "realtime_handoff" | "audio_transcript" => {}

        // Codex v0.142.0 (PRs #28746, #28494, #28707, #29423, #29255): token budget events.
        // token_budget_compaction_reminder is a response_item that suggests compaction
        // due to budget proximity. No turn-building semantics — silently skip.
        "token_budget_compaction_reminder" => {}

        // Codex v0.132.0 (PR #23123): `codex exec resume --output-schema` emits the final
        // model response as a "structured_output" response_item whose `content` field holds a
        // JSON object validated against the provided schema. Without this handler the item falls
        // through to `_ => {}` and the turn's final_answer is never populated.
        "structured_output" => {
            if let Some(turn) = turns.get_mut(tid) {
                if turn.final_answer.is_none() {
                    let text = extract_item_content(payload);
                    if !text.is_empty() {
                        turn.final_answer = Some(text);
                    }
                }
            }
        }

        // Codex Desktop v0.153 records user prompts as response_item messages with
        // role=user and input_text content blocks (rather than event_msg.user_message).
        // Use the last user message in a turn: the first one is often the injected
        // environment/context block, while the final one is the actual prompt.
        "message" if payload.get("role").and_then(|v| v.as_str()) == Some("user") => {
            let raw_text = extract_item_content(payload);
            if !raw_text.is_empty() {
                let (text, task_mentions) = decode_task_mentions(&raw_text);
                if let Some(turn) = turns.get_mut(tid) {
                    turn.user_message = Some(text);
                    turn.task_mentions = task_mentions;
                }
            }
        }

        // Handle assistant message response_items. This includes schema-validated JSON content
        // from `codex exec resume --output-schema` (Codex v0.132.0+, PR #23123) where `content`
        // is a JSON object rather than a plain string.
        "message" if payload.get("role").and_then(|v| v.as_str()) == Some("assistant") => {
            if let Some(turn) = turns.get_mut(tid) {
                if turn.final_answer.is_none() {
                    let text = extract_item_content(payload);
                    if !text.is_empty() {
                        turn.final_answer = Some(text);
                    }
                }
            }
        }

        _ => {}
    }
}

fn handle_turn_context(
    entry: &RawEntry,
    turns: &mut indexmap::IndexMap<String, CodexTurn>,
    current_turn_id: &Option<String>,
) {
    let payload = &entry.payload;
    let model = payload
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let cwd = payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let effort = payload
        .get("effort")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    // Codex v0.135.0 (PR #24591): memories are now stored in a dedicated SQLite DB and
    // injected into the context at turn start. The active set is written into turn_context.
    // Codex v0.132.0 (PR #23148): memory summary items are versioned objects
    // {"content":"...","version":N}; pre-v0.132.0 sessions use plain strings.
    let memories: Vec<MemorySummary> = payload
        .get("memories")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    if let Some(s) = item.as_str() {
                        // Pre-v0.132.0: plain string memory
                        Some(MemorySummary {
                            content: s.to_string(),
                            version: None,
                        })
                    } else if let Some(content) = item.get("content").and_then(|v| v.as_str()) {
                        // v0.132.0+: versioned object {"content":"...","version":N}
                        let version = item
                            .get("version")
                            .and_then(|v| v.as_u64())
                            .map(|v| v as u32);
                        Some(MemorySummary {
                            content: content.to_string(),
                            version,
                        })
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    if let Some(ref tid) = current_turn_id {
        if let Some(turn) = turns.get_mut(tid) {
            if model.is_some() {
                turn.model = model;
            }
            if cwd.is_some() {
                turn.cwd = cwd;
            }
            if effort.is_some() {
                turn.reasoning_effort = effort;
            }
            if !memories.is_empty() {
                turn.memories = memories;
            }
        }
    }
}

/// Extract text from a response_item `content` field.
/// Handles:
/// - Plain strings (standard message content)
/// - Content arrays (`[{"type":"text","text":"..."}]`, OpenAI format)
/// - JSON objects (schema-validated responses from `codex exec resume --output-schema`,
///   Codex v0.132.0+, PR #23123)
fn extract_item_content(payload: &Value) -> String {
    let content = payload.get("content").or_else(|| payload.get("output"));
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(""),
        Some(v) if !v.is_null() => serde_json::to_string(v).unwrap_or_default(),
        _ => String::new(),
    }
}

/// Extract output from a custom tool result across legacy and Desktop formats.
fn extract_custom_tool_output(payload: &Value) -> (String, Option<i32>) {
    let Some(value) = payload.get("output") else {
        return (String::new(), None);
    };

    if let Some(raw) = value.as_str() {
        let parsed = serde_json::from_str::<Value>(raw).ok();
        let output = parsed
            .as_ref()
            .and_then(|v| v.get("output"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| raw.to_owned());
        let exit_code = parsed
            .as_ref()
            .and_then(|v| v.get("metadata"))
            .and_then(|m| m.get("exit_code"))
            .and_then(Value::as_i64)
            .and_then(|code| i32::try_from(code).ok());
        return (output, exit_code);
    }

    (custom_tool_text(value).unwrap_or_default(), None)
}

/// Convert a custom-tool input/output value to displayable text. Desktop uses
/// OpenAI-style arrays of `{type, text}` blocks for structured tool output.
fn custom_tool_text(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_owned());
    }
    if let Some(items) = value.as_array() {
        return Some(
            items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(""),
        );
    }
    None
}

/// Extract plain text from a content value that may be a bare string, an array of
/// content blocks (OpenAI format: `[{"type":"text","text":"..."}]`), or an object
/// with a nested "content" field. Used to migrate pre-v0.133.0 UserTurn entries.
fn extract_content_text(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    if let Some(arr) = v.as_array() {
        return arr
            .iter()
            .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("");
    }
    if let Some(content) = v.get("content") {
        return extract_content_text(content);
    }
    String::new()
}

fn u64_field(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(|v| v.as_u64()).unwrap_or(0)
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Extract displayable text from an `agent_message` payload's `message` field.
///
/// Codex v0.142.0 (PR #28368) changed multi-agent v2 inter-agent messages from
/// plain strings to typed envelopes `{"type": "<kind>", "content": "..."}`.
/// This function handles both formats so old and new sessions parse correctly.
fn extract_message_text(message: &Value) -> String {
    // Legacy format (pre-v0.142.0): message is a plain string.
    if let Some(s) = message.as_str() {
        return s.to_string();
    }

    // v0.142.0+ typed envelope: dispatch on the "type" discriminant.
    if let Some(envelope_type) = message.get("type").and_then(|t| t.as_str()) {
        match envelope_type {
            // "text" envelope: content is in the "content" field.
            "text" => {
                return message
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
            }
            // Other envelope types: try "content" as a best-effort fallback.
            _ => {
                if let Some(content) = message.get("content").and_then(|v| v.as_str()) {
                    return content.to_string();
                }
            }
        }
    }

    String::new()
}

/// Parse the `dynamic_tools` field from a `task_started` payload (Codex v0.141.0+).
///
/// Returns a registry mapping unqualified tool names to (tool_type, server).
/// Handles two formats:
///   {"name": "my_tool", "namespace": "mcp:my-server"}  → key="my_tool", ("mcp","my-server")
///   {"name": "mcp:my-server/my_tool"}                  → key="my_tool", ("mcp","my-server")
fn parse_dynamic_tools(payload: &Value) -> HashMap<String, (String, String)> {
    let Some(tools) = payload.get("dynamic_tools").and_then(|v| v.as_array()) else {
        return HashMap::new();
    };
    let mut registry = HashMap::new();
    for item in tools {
        let Some(name) = item
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if let Some(ns) = item
            .get("namespace")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            // Explicit namespace field: "mcp:server" or "connector:plugin"
            if let Some((tool_type, server)) = ns.split_once(':') {
                if !tool_type.is_empty() && !server.is_empty() {
                    registry.insert(
                        name.to_string(),
                        (tool_type.to_string(), server.to_string()),
                    );
                }
            }
        } else if let Some((tool_type, rest)) = name.split_once(':') {
            // Qualified name format: "mcp:server/tool_name"
            if let Some((server, tool)) = rest.split_once('/') {
                if !tool_type.is_empty() && !server.is_empty() && !tool.is_empty() {
                    registry.insert(
                        tool.to_string(),
                        (tool_type.to_string(), server.to_string()),
                    );
                }
            }
        }
    }
    registry
}

/// Parse tool definitions from a `tool_search_call_output` payload (Codex v0.142.2+, PR #29486).
///
/// The `tools` array may contain entries in the same formats as `dynamic_tools`:
///   {"name": "my_tool", "server": "my-server"}           → key="my_tool", ("mcp","my-server")
///   {"name": "my_tool", "namespace": "mcp:my-server"}    → key="my_tool", ("mcp","my-server")
///   {"name": "mcp:my-server/my_tool"}                    → key="my_tool", ("mcp","my-server")
fn parse_tool_search_results(payload: &Value) -> HashMap<String, (String, String)> {
    let Some(tools) = payload.get("tools").and_then(|v| v.as_array()) else {
        return HashMap::new();
    };
    let mut registry = HashMap::new();
    for item in tools {
        let Some(name) = item
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        // Explicit server field: {"name": "my_tool", "server": "my-server"}
        if let Some(server) = item
            .get("server")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            registry.insert(name.to_string(), ("mcp".to_string(), server.to_string()));
        } else if let Some(ns) = item
            .get("namespace")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            // Namespace field: "mcp:server" or "connector:plugin"
            if let Some((tool_type, server)) = ns.split_once(':') {
                if !tool_type.is_empty() && !server.is_empty() {
                    registry.insert(
                        name.to_string(),
                        (tool_type.to_string(), server.to_string()),
                    );
                }
            }
        } else if let Some((tool_type, rest)) = name.split_once(':') {
            // Qualified name: "mcp:server/tool_name"
            if let Some((server, tool)) = rest.split_once('/') {
                if !tool_type.is_empty() && !server.is_empty() && !tool.is_empty() {
                    registry.insert(
                        tool.to_string(),
                        (tool_type.to_string(), server.to_string()),
                    );
                }
            }
        }
    }
    registry
}

fn spawn_from_function_call_output(
    builder: &ToolCallBuilder,
    call_id: &str,
    output: &str,
) -> Option<CollabSpawn> {
    let pending = builder.pending.get(call_id)?;
    if pending.name != "spawn_agent" {
        return None;
    }

    let parsed = parse_spawn_agent_output(output)?;
    let message = pending
        .arguments
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let prompt_preview = message.chars().take(200).collect();
    let agent_role = pending
        .arguments
        .get("agent_type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let model = pending
        .arguments
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let reasoning_effort = pending
        .arguments
        .get("reasoning_effort")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Some(CollabSpawn {
        call_id: call_id.to_string(),
        new_session_id: parsed.agent_id,
        agent_nickname: parsed.nickname,
        agent_role,
        model,
        reasoning_effort,
        prompt_preview,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::toolcall::ToolKind;

    fn entries(lines: &[&str]) -> Vec<RawEntry> {
        lines
            .iter()
            .filter_map(|line| RawEntry::parse(line))
            .collect()
    }

    #[test]
    fn orders_messages_and_tool_calls_by_stream_position() {
        // A tool call that happens between two agent messages must sort between them, so the
        // UI can render it inline instead of dumping all tool calls at the end of the turn.
        let entries = entries(&[
            r#"{"timestamp":"2026-04-27T04:53:00Z","type":"session_meta","payload":{"id":"s","timestamp":"2026-04-27T04:53:00Z"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:02Z","type":"event_msg","payload":{"type":"agent_message","message":"FIRST"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:03Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"echo hi\",\"workdir\":\"/tmp\"}","call_id":"call_exec"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:04Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_exec","output":"Output:\nhi\nProcess exited with code 0\n"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:05Z","type":"event_msg","payload":{"type":"agent_message","message":"SECOND"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:06Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777279986.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        let turn = &turns[0];
        assert_eq!(turn.agent_messages.len(), 2);
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_call_orders.len(), turn.tool_calls.len());

        let first_msg_order = turn.agent_messages[0].order;
        let second_msg_order = turn.agent_messages[1].order;
        let tool_order = turn.tool_call_orders[0];
        assert!(
            first_msg_order < tool_order,
            "tool call ({tool_order}) should sort after the first message ({first_msg_order})"
        );
        assert!(
            tool_order < second_msg_order,
            "tool call ({tool_order}) should sort before the second message ({second_msg_order})"
        );
    }

    #[test]
    fn links_spawn_agent_from_sdk_function_call_output() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-27T04:52:00Z","type":"session_meta","payload":{"id":"parent","timestamp":"2026-04-27T04:52:00Z"}}"#,
            r#"{"timestamp":"2026-04-27T04:52:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-27T04:52:02Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"message\":\"Collect evidence\",\"model\":\"gpt-5.4-mini\",\"reasoning_effort\":\"medium\"}","call_id":"call_spawn"}}"#,
            r#"{"timestamp":"2026-04-27T04:52:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_spawn","output":"{\"agent_id\":\"worker-session\",\"nickname\":\"Parfit\"}"}}"#,
            r#"{"timestamp":"2026-04-27T04:52:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777279924.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].collab_spawns.len(), 1);
        assert_eq!(turns[0].collab_spawns[0].call_id, "call_spawn");
        assert_eq!(turns[0].collab_spawns[0].new_session_id, "worker-session");
        assert_eq!(turns[0].collab_spawns[0].agent_nickname, "Parfit");
        assert_eq!(turns[0].collab_spawns[0].agent_role, "worker");
        assert_eq!(
            turns[0].collab_spawns[0].model.as_deref(),
            Some("gpt-5.4-mini")
        );
        assert_eq!(
            turns[0].collab_spawns[0].reasoning_effort.as_deref(),
            Some("medium")
        );
        assert_eq!(turns[0].tool_calls.len(), 1);
        assert_eq!(turns[0].tool_calls[0].kind, ToolKind::SpawnAgent);
    }

    #[test]
    fn records_context_window_tokens_from_last_token_usage() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-27T04:52:00Z","type":"session_meta","payload":{"id":"parent","timestamp":"2026-04-27T04:52:00Z"}}"#,
            r#"{"timestamp":"2026-04-27T04:52:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-27T04:52:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":38000,"cached_input_tokens":12000,"output_tokens":2000,"reasoning_output_tokens":500,"total_tokens":40000},"last_token_usage":{"input_tokens":24000,"cached_input_tokens":8000,"output_tokens":1500,"reasoning_output_tokens":200,"total_tokens":26000},"model_context_window":100000}}}"#,
            r#"{"timestamp":"2026-04-27T04:52:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777279923.0}}"#,
        ]);

        let turns = build_turns(&entries);

        let tokens = turns[0].total_tokens.as_ref().expect("token count");
        assert_eq!(tokens.total_tokens, 40000);
        assert_eq!(tokens.context_window_tokens, Some(26000));
        assert_eq!(tokens.model_context_window, 100000);
    }

    #[test]
    fn marks_failed_spawn_agent_output_without_child_link() {
        let failure =
            "Full-history forked agents inherit the parent agent type, model, and reasoning effort; omit agent_type, model, and reasoning_effort, or spawn without a full-history fork.";
        let line = format!(
            r#"{{"timestamp":"2026-04-27T04:52:03Z","type":"response_item","payload":{{"type":"function_call_output","call_id":"call_spawn","output":{failure:?}}}}}"#
        );
        let lines = [
            r#"{"timestamp":"2026-04-27T04:52:00Z","type":"session_meta","payload":{"id":"parent","timestamp":"2026-04-27T04:52:00Z"}}"#.to_string(),
            r#"{"timestamp":"2026-04-27T04:52:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#.to_string(),
            r#"{"timestamp":"2026-04-27T04:52:02Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"fork_context\":true,\"message\":\"Collect evidence\"}","call_id":"call_spawn"}}"#.to_string(),
            line,
            r#"{"timestamp":"2026-04-27T04:52:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777279924.0}}"#.to_string(),
        ];
        let entries: Vec<RawEntry> = lines
            .iter()
            .filter_map(|line| RawEntry::parse(line))
            .collect();

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert!(turns[0].collab_spawns.is_empty());
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::SpawnAgent);
        assert_eq!(tool.status, "failed");
        assert_eq!(tool.output.as_deref(), Some(failure));
    }

    #[test]
    fn classifies_sdk_exec_command_function_output() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-27T04:53:00Z","type":"session_meta","payload":{"id":"worker","timestamp":"2026-04-27T04:53:00Z"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"printf hello\",\"workdir\":\"/tmp\",\"yield_time_ms\":1000}","call_id":"call_exec"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_exec","output":"Chunk ID: abc123\nWall time: 0.2500 seconds\nProcess exited with code 0\nOriginal token count: 1\nOutput:\nhello\n"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777279984.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::ExecCommand);
        assert_eq!(tool.name, "exec_command");
        assert_eq!(tool.output.as_deref(), Some("hello\n"));
        assert_eq!(tool.exit_code, Some(0));
        assert_eq!(tool.status, "completed");
        assert_eq!(
            tool.command.as_ref().unwrap(),
            &vec!["printf hello".to_string()]
        );
        assert_eq!(tool.cwd.as_deref(), Some("/tmp"));
    }

    #[test]
    fn folds_write_stdin_output_into_running_sdk_exec_command() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-27T04:53:00Z","type":"session_meta","payload":{"id":"worker","timestamp":"2026-04-27T04:53:00Z"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"node slack.js history --channel '#ai-tools-on-call'\",\"workdir\":\"/workspace\",\"yield_time_ms\":1000}","call_id":"call_exec"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_exec","output":"Chunk ID: e6e3cc\nWall time: 1.0020 seconds\nProcess running with session ID 72266\nOriginal token count: 0\nOutput:\n"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:04Z","type":"response_item","payload":{"type":"function_call","name":"write_stdin","arguments":"{\"session_id\":72266,\"chars\":\"\",\"yield_time_ms\":1000,\"max_output_tokens\":30000}","call_id":"call_poll"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:05Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_poll","output":"Chunk ID: 507212\nWall time: 0.0000 seconds\nProcess exited with code 1\nOriginal token count: 19\nOutput:\n{\n  \"ok\": false,\n  \"error\": \"Slack API error: enterprise_is_restricted\"\n}\n"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:06Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777279986.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.call_id, "call_exec");
        assert_eq!(tool.kind, ToolKind::ExecCommand);
        assert_eq!(tool.name, "exec_command");
        assert_eq!(tool.exit_code, Some(1));
        assert_eq!(tool.status, "failed");
        assert!(tool
            .output
            .as_deref()
            .unwrap()
            .contains("Slack API error: enterprise_is_restricted"));
        assert_eq!(
            tool.command.as_ref().unwrap(),
            &vec!["node slack.js history --channel '#ai-tools-on-call'".to_string()]
        );
        assert_eq!(tool.cwd.as_deref(), Some("/workspace"));
    }

    #[test]
    fn preserves_unwrapped_sdk_exec_output() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-27T04:53:00Z","type":"session_meta","payload":{"id":"worker","timestamp":"2026-04-27T04:53:00Z"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"tool with changed output shape\",\"workdir\":\"/tmp\"}","call_id":"call_exec"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_exec","output":"plain future transport output\nstill visible\n"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777279984.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::ExecCommand);
        assert_eq!(
            tool.output.as_deref(),
            Some("plain future transport output\nstill visible\n")
        );
        assert_eq!(tool.status, "completed");
    }

    #[test]
    fn folds_single_running_exec_without_session_id_mapping() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-27T04:53:00Z","type":"session_meta","payload":{"id":"worker","timestamp":"2026-04-27T04:53:00Z"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"long command\",\"workdir\":\"/workspace\"}","call_id":"call_exec"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_exec","output":"still running under a future transport shape\n"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:04Z","type":"response_item","payload":{"type":"function_call","name":"write_stdin","arguments":"{\"session_id\":123,\"chars\":\"\"}","call_id":"call_poll"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:05Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_poll","output":"final chunk under a future transport shape\n"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:06Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777279986.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.call_id, "call_exec");
        assert_eq!(tool.kind, ToolKind::ExecCommand);
        assert!(tool
            .output
            .as_deref()
            .unwrap()
            .contains("final chunk under a future transport shape"));
        assert_eq!(tool.status, "completed");
    }

    // Codex v0.132.0 (PR #22706): the legacy shell output formatting paths were removed.
    // function_call_output for exec_command now contains the raw command output only —
    // no "Chunk ID:", "Wall time:", "Process exited with code N", "Output:" markers.
    // The parser must preserve the full raw output and not attempt marker-based extraction.
    #[test]
    fn function_call_output_v0132_plain_text_no_legacy_markers() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"v0132-session","timestamp":"2026-05-20T10:00:00Z","cli_version":"0.132.0"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"echo hello world\",\"workdir\":\"/tmp\"}","call_id":"call_exec"}}"#,
            // v0.132.0: raw output only — no "Chunk ID", "Wall time", "Process exited", "Output:" markers
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_exec","output":"hello world\n"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748606404.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::ExecCommand);
        assert_eq!(tool.name, "exec_command");
        // Raw output must be preserved in full — no marker stripping
        assert_eq!(tool.output.as_deref(), Some("hello world\n"));
        // No exit code extractable from plain text — None is correct
        assert_eq!(tool.exit_code, None);
        assert_eq!(tool.status, "completed");
    }

    // Codex v0.132.0 (PR #22706): exec_command_end events no longer include formatted_output.
    // When both function_call_output (plain text, no markers) and exec_command_end (with
    // aggregated_output) are present, the exec_command_end structured fields take precedence.
    #[test]
    fn exec_command_end_v0132_structured_output_and_exit_code() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"v0132-end-session","timestamp":"2026-05-20T10:00:00Z","cli_version":"0.132.0"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"ls /nonexistent\",\"workdir\":\"/tmp\"}","call_id":"call_ls"}}"#,
            // v0.132.0: exec_command_end carries aggregated_output + structured exit_code + duration
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"event_msg","payload":{"type":"exec_command_end","call_id":"call_ls","aggregated_output":"ls: /nonexistent: No such file or directory\n","exit_code":1,"status":"failed","duration":{"secs":0,"nanos":5000000}}}"#,
            r#"{"timestamp":"2026-05-20T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748606404.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::ExecCommand);
        assert_eq!(
            tool.output.as_deref(),
            Some("ls: /nonexistent: No such file or directory\n")
        );
        assert_eq!(tool.exit_code, Some(1));
        assert_eq!(tool.status, "failed");
        assert!(tool.duration_secs.is_some());
    }

    // Codex v0.133.0 (PR #23564): exec output is now preserved in raw form without truncation.
    // function_call_output entries may therefore contain very large blobs that include phrases
    // like "exit code" or "wall time" as part of the actual command output (compilation errors,
    // benchmark results, test runners). The parser must preserve the full output without
    // false-positive metadata extraction and without erroring.
    #[test]
    fn v0133_raw_exec_output_with_false_positive_patterns_is_handled_correctly() {
        // Raw output contains "exit code 1" and "wall time" inline (e.g., test runner output).
        // The parser must not extract these as exec metadata — the exec ended successfully
        // per the exec_command_end structured event.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-21T10:00:00Z","type":"session_meta","payload":{"id":"v0133-session","timestamp":"2026-05-21T10:00:00Z","cli_version":"0.133.0"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"cargo test\",\"workdir\":\"/project\"}","call_id":"call_cargo"}}"#,
            // v0.133.0 raw output: large blob containing "exit code" and "wall time" in content
            r#"{"timestamp":"2026-05-21T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_cargo","output":"running 3 tests\ntest_auth ... ok\ntest_billing ... FAILED: exit code 1 expected\nwall time for suite: 2.5s\ntest result: FAILED. 2 passed; 1 failed\n"}}"#,
            // exec_command_end provides authoritative structured metadata
            r#"{"timestamp":"2026-05-21T10:00:04Z","type":"event_msg","payload":{"type":"exec_command_end","call_id":"call_cargo","aggregated_output":"running 3 tests\ntest result: FAILED. 2 passed; 1 failed\n","exit_code":1,"status":"failed","duration":{"secs":2,"nanos":500000000}}}"#,
            r#"{"timestamp":"2026-05-21T10:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748606405.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        // exec_command_end takes precedence — its call_id removes the pending entry so
        // function_call_output is skipped; the structured event produces exactly one call.
        let tool_calls = &turns[0].tool_calls;
        assert_eq!(
            tool_calls.len(),
            1,
            "exactly one exec tool call expected; got {:?}",
            tool_calls.iter().map(|tc| &tc.call_id).collect::<Vec<_>>()
        );
        let tool = &tool_calls[0];
        assert_eq!(tool.kind, ToolKind::ExecCommand);
        // Exit code and status come from the structured exec_command_end event.
        assert_eq!(tool.exit_code, Some(1));
        assert_eq!(tool.status, "failed");
        assert!(tool.duration_secs.is_some());
    }

    #[test]
    fn v0133_raw_exec_output_only_no_exec_command_end_no_false_positive_metadata() {
        // v0.133.0 session where only function_call_output is present (no exec_command_end).
        // The raw output contains false-positive "exit code" and "wall time" patterns that
        // must NOT be extracted as metadata — the output must be preserved in full.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-21T10:00:00Z","type":"session_meta","payload":{"id":"v0133-raw-only","timestamp":"2026-05-21T10:00:00Z","cli_version":"0.133.0"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"npm test\",\"workdir\":\"/app\"}","call_id":"call_npm"}}"#,
            // Raw output: large blob with false-positive patterns, no "Output:" marker
            r#"{"timestamp":"2026-05-21T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_npm","output":"PASS src/auth.test.js\nFAIL src/billing.test.js\n  ● billing › returns exit code 1 on invalid input\n    wall time: 1.2s exceeded budget of 1.0s\nTest Suites: 1 failed, 1 passed\n"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748606404.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::ExecCommand);
        // No false-positive exit code from "exit code 1" inside test output
        assert_eq!(
            tool.exit_code, None,
            "exit_code must be None — raw output must not be scanned for metadata"
        );
        // No false-positive duration from "wall time: 1.2s" inside test output
        assert_eq!(
            tool.duration_secs, None,
            "duration_secs must be None — raw output must not be scanned for metadata"
        );
        assert_eq!(tool.status, "completed");
        // Full raw output must be preserved unmodified
        assert!(
            tool.output
                .as_deref()
                .unwrap_or("")
                .contains("returns exit code 1 on invalid input"),
            "full raw output must be preserved"
        );
    }

    #[test]
    fn links_spawn_agent_from_collab_spawn_end_event() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-16T11:48:00Z","type":"session_meta","payload":{"id":"parent","timestamp":"2026-04-16T11:48:00Z"}}"#,
            r#"{"timestamp":"2026-04-16T11:48:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-16T11:48:02Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"message\":\"Collect graph\"}","call_id":"call_spawn"}}"#,
            r#"{"timestamp":"2026-04-16T11:48:03Z","type":"event_msg","payload":{"type":"collab_agent_spawn_end","call_id":"call_spawn","sender_thread_id":"parent","new_thread_id":"worker-session","new_agent_nickname":"Noether","new_agent_role":"worker","prompt":"Collect graph","model":"gpt-5.4-mini","reasoning_effort":"medium","status":"pending_init"}}"#,
            r#"{"timestamp":"2026-04-16T11:48:04Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_spawn","output":"{\"agent_id\":\"worker-session\",\"nickname\":\"Noether\"}"}}"#,
            r#"{"timestamp":"2026-04-16T11:48:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1776335285.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].collab_spawns.len(), 1);
        assert_eq!(turns[0].collab_spawns[0].new_session_id, "worker-session");
        assert_eq!(turns[0].collab_spawns[0].agent_nickname, "Noether");
        assert_eq!(turns[0].tool_calls.len(), 1);
        assert_eq!(turns[0].tool_calls[0].kind, ToolKind::SpawnAgent);
    }

    // Codex v0.128.0 (#19473): task_started now includes turn_start_timestamp in the payload.
    // It should be used as started_at in preference to the outer JSONL line timestamp.
    #[test]
    fn turn_start_timestamp_in_payload_is_used_as_started_at() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"s1","timestamp":"2026-04-30T10:00:00Z"}}"#,
            // turn_start_timestamp = 1746000050.0 (earlier than outer line timestamp 1746000060)
            r#"{"timestamp":"2026-04-30T10:01:00Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","turn_start_timestamp":1746000050.0}}"#,
            r#"{"timestamp":"2026-04-30T10:02:00Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746000120.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        // started_at should come from turn_start_timestamp (1746000050), not the line timestamp
        assert_eq!(turns[0].started_at, Some(1746000050));
    }

    // Codex v0.128.0 (#19620): turn metadata headers are now ASCII-escaped JSON.
    // serde_json handles \uXXXX sequences natively; verify non-ASCII in payloads parses correctly.
    #[test]
    fn ascii_escaped_unicode_in_task_started_payload_is_parsed_correctly() {
        // Chinese characters ASCII-escaped as Codex v0.128.0 emits
        let entries = entries(&[
            r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"s1","timestamp":"2026-04-30T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-04-30T10:01:00Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","turn_start_timestamp":1746000050.0}}"#,
            "{\"timestamp\":\"2026-04-30T10:01:01Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"\\u4e2d\\u6587\"}}",
            r#"{"timestamp":"2026-04-30T10:02:00Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746000120.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        // The ASCII-escaped Unicode must be decoded to its actual UTF-8 string value
        assert_eq!(turns[0].user_message.as_deref(), Some("\u{4e2d}\u{6587}"));
    }

    #[test]
    fn reads_token_usage_from_task_complete_v0128() {
        // Codex v0.128.0 adds prompt_tokens/completion_tokens/total_tokens to task_complete.
        // These should populate turn.total_tokens when no prior token_count event exists.
        let entries = entries(&[
            r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"s1","timestamp":"2026-04-30T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:10Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746007210.0,"prompt_tokens":1500,"completion_tokens":300,"total_tokens":1800}}"#,
        ]);

        let turns = build_turns(&entries);

        let tokens = turns[0]
            .total_tokens
            .as_ref()
            .expect("token info from task_complete");
        assert_eq!(tokens.input_tokens, 1500);
        assert_eq!(tokens.output_tokens, 300);
        assert_eq!(tokens.total_tokens, 1800);
        assert_eq!(tokens.cached_input_tokens, 0);
        assert_eq!(tokens.context_window_tokens, None);
    }

    #[test]
    fn inference_stream_cancelled_marks_turn_cancelled() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"sess-1","timestamp":"2026-04-30T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:02Z","type":"event_msg","payload":{"type":"agent_message","message":"Working on it...","phase":"commentary"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:03Z","type":"event_msg","payload":{"type":"inference_stream_cancelled","turn_id":"turn-1","completed_at":1746007203.0,"duration_ms":2000}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Cancelled);
        assert_eq!(turns[0].completed_at, Some(1746007203));
        assert_eq!(turns[0].duration_ms, Some(2000));
    }

    #[test]
    fn inference_stream_cancelled_falls_back_to_entry_timestamp() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"sess-2","timestamp":"2026-04-30T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:05Z","type":"event_msg","payload":{"type":"inference_stream_cancelled","turn_id":"turn-2"}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Cancelled);
        assert!(turns[0].completed_at.is_some());
    }

    #[test]
    fn inference_stream_cancelled_uses_current_turn_when_no_turn_id() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"sess-3","timestamp":"2026-04-30T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-3"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:04Z","type":"event_msg","payload":{"type":"inference_stream_cancelled"}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Cancelled);
    }

    // Codex v0.130.0 (PR #21566): multi-page thread completeness.
    // The thread turns endpoint now paginates large threads and writes "unloaded"
    // stub entries as placeholders between pages.  build_turns must ignore all
    // non-full stubs so every real turn is present in the parsed output.
    #[test]
    fn multi_page_thread_all_turns_present_stubs_ignored() {
        let entries = entries(&[
            // session header
            r#"{"timestamp":"2026-05-08T10:00:00Z","type":"session_meta","payload":{"id":"long-session","timestamp":"2026-05-08T10:00:00Z"}}"#,
            // page 1 — turn 1 (full entries, no view_mode = legacy compat)
            r#"{"timestamp":"2026-05-08T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","turn_start_timestamp":1746691201.0}}"#,
            r#"{"timestamp":"2026-05-08T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746691202.0}}"#,
            // unloaded stub that would appear between pages (view_mode:unloaded) — must be skipped
            r#"{"timestamp":"2026-05-08T10:00:03Z","type":"event_msg","view_mode":"unloaded","payload":{"type":"task_started","turn_id":"turn-2"}}"#,
            // summary stub — also must be skipped
            r#"{"timestamp":"2026-05-08T10:00:04Z","type":"event_msg","view_mode":"summary","payload":{"type":"task_started","turn_id":"turn-2"}}"#,
            // page 2 — turn 2 (full view_mode explicit)
            r#"{"timestamp":"2026-05-08T10:00:05Z","type":"event_msg","view_mode":"full","payload":{"type":"task_started","turn_id":"turn-2","turn_start_timestamp":1746691205.0}}"#,
            r#"{"timestamp":"2026-05-08T10:00:06Z","type":"event_msg","view_mode":"full","payload":{"type":"task_complete","turn_id":"turn-2","completed_at":1746691206.0}}"#,
            // page 3 — turn 3 (legacy, no view_mode)
            r#"{"timestamp":"2026-05-08T10:00:07Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-3","turn_start_timestamp":1746691207.0}}"#,
            r#"{"timestamp":"2026-05-08T10:00:08Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-3","completed_at":1746691208.0}}"#,
        ]);

        let turns = build_turns(&entries);

        // All three real turns must be present; stubs must not create phantom turns
        assert_eq!(
            turns.len(),
            3,
            "expected exactly 3 complete turns, got {}",
            turns.len()
        );
        assert_eq!(turns[0].turn_id, "turn-1");
        assert_eq!(turns[1].turn_id, "turn-2");
        assert_eq!(turns[2].turn_id, "turn-3");
        assert!(turns.iter().all(|t| t.status == TurnStatus::Complete));
    }

    // Codex v0.129.0 (PR #20677): mcp_tool_call + mcp_tool_call_output are now emitted
    // as first-class response_item turn entries. Verify they are classified as McpTool.
    #[test]
    fn mcp_tool_call_turn_items_classified_as_mcp_tool() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-07T10:00:00Z","type":"session_meta","payload":{"id":"s-mcp","timestamp":"2026-05-07T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:02Z","type":"response_item","payload":{"type":"mcp_tool_call","call_id":"mcp-1","server":"github","tool":"get_pr_info","arguments":{"pr_number":42}}}"#,
            r#"{"timestamp":"2026-05-07T10:00:03Z","type":"response_item","payload":{"type":"mcp_tool_call_output","call_id":"mcp-1","output":[{"type":"text","text":"PR #42: Fix the bug"}]}}"#,
            r#"{"timestamp":"2026-05-07T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746612004.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::McpTool);
        assert_eq!(tool.call_id, "mcp-1");
        assert_eq!(tool.name, "get_pr_info");
        assert_eq!(tool.mcp_server.as_deref(), Some("github"));
        assert_eq!(tool.mcp_tool.as_deref(), Some("get_pr_info"));
        assert_eq!(tool.output.as_deref(), Some("PR #42: Fix the bug"));
        assert_eq!(tool.status, "completed");
    }

    #[test]
    fn mcp_tool_call_turn_items_with_string_output() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-07T10:00:00Z","type":"session_meta","payload":{"id":"s-mcp2","timestamp":"2026-05-07T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:02Z","type":"response_item","payload":{"type":"mcp_tool_call","call_id":"mcp-2","server":"jira","tool":"create_issue","arguments":{"summary":"Fix login"}}}"#,
            r#"{"timestamp":"2026-05-07T10:00:03Z","type":"response_item","payload":{"type":"mcp_tool_call_output","call_id":"mcp-2","output":"Issue created: PROJ-123"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746612004.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::McpTool);
        assert_eq!(tool.name, "create_issue");
        assert_eq!(tool.mcp_server.as_deref(), Some("jira"));
        assert_eq!(tool.output.as_deref(), Some("Issue created: PROJ-123"));
    }

    #[test]
    fn mcp_tool_call_turn_items_with_stringified_arguments() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-07T10:00:00Z","type":"session_meta","payload":{"id":"s-mcp3","timestamp":"2026-05-07T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:02Z","type":"response_item","payload":{"type":"mcp_tool_call","call_id":"mcp-3","server":"slack","tool":"post_message","arguments":"{\"channel\":\"general\",\"text\":\"hello\"}"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:03Z","type":"response_item","payload":{"type":"mcp_tool_call_output","call_id":"mcp-3","output":"ok"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746612004.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::McpTool);
        assert_eq!(tool.mcp_server.as_deref(), Some("slack"));
        assert_eq!(tool.mcp_tool.as_deref(), Some("post_message"));
    }

    #[test]
    fn unknown_response_item_types_are_silently_skipped() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-07T10:00:00Z","type":"session_meta","payload":{"id":"s-ri","timestamp":"2026-05-07T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:02Z","type":"response_item","payload":{"type":"future_unknown_item_type_v999","call_id":"x","data":"whatever"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746612003.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Complete);
        assert_eq!(turns[0].tool_calls.len(), 0);
    }

    #[test]
    fn unknown_event_types_are_ignored_gracefully() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"sess-4","timestamp":"2026-04-30T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-4"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:02Z","type":"event_msg","payload":{"type":"some_future_unknown_event","data":"whatever"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-4","completed_at":1746007203.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Complete);
    }

    // Codex v0.129.0 (PR #20471) removed `item/fileChange` and `outputDelta` notification
    // events from the app-server event stream. codex-trace is unaffected because it reads
    // JSONL session files from disk — it never connects to the Codex app-server. Even if
    // these types appeared as event_msg entries in older JSONL files, they would be silently
    // dropped by the wildcard arm, and file-change data is already read from patch_apply_end
    // events via patch_changes. This test guards against regressions that would re-introduce
    // a dependency on these removed notification types.
    #[test]
    fn v0129_removed_item_file_change_and_output_delta_events_ignored_gracefully() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-07T10:00:00Z","type":"session_meta","payload":{"id":"sess-v0129","timestamp":"2026-05-07T10:00:00Z","cli_version":"0.129.0"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:02Z","type":"event_msg","payload":{"type":"item/fileChange","path":"/tmp/foo.txt","action":"modified"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:03Z","type":"event_msg","payload":{"type":"outputDelta","delta":"partial output text"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746604804.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Complete);
        assert!(turns[0].tool_calls.is_empty());
    }

    // Codex v0.138.0 (PR #26447): `response.processed` WebSocket request type was removed
    // from the app-server/exec-server WebSocket protocol. codex-trace is unaffected because
    // it reads JSONL session files from disk — it never connects to the Codex app-server.
    // Even if `response.processed` appeared as an event_msg entry in a JSONL file written by
    // an older Codex version, it would be silently dropped by the wildcard arm in
    // handle_event_msg. This test guards against regressions that would re-introduce a
    // dependency on this removed message type.
    #[test]
    fn v0138_removed_response_processed_event_ignored_gracefully() {
        let entries = entries(&[
            r#"{"timestamp":"2026-06-08T10:00:00Z","type":"session_meta","payload":{"id":"sess-v0138","timestamp":"2026-06-08T10:00:00Z","cli_version":"0.138.0"}}"#,
            r#"{"timestamp":"2026-06-08T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-08T10:00:02Z","type":"event_msg","payload":{"type":"response.processed","response_id":"resp-abc","status":"complete"}}"#,
            r#"{"timestamp":"2026-06-08T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1749376803.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Complete);
        assert!(turns[0].tool_calls.is_empty());
    }

    // Codex v0.129.0 (PR #20540): apply_patch file changes moved from patch_apply_end
    // event into an apply_patch_end response_item (turn item). The PatchApply call is
    // finalized by custom_tool_call_output (exit_code, output) and the file changes are
    // backfilled by the subsequent apply_patch_end turn item.
    #[test]
    fn apply_patch_end_turn_item_backfills_file_changes_onto_finalized_call() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-07T10:00:00Z","type":"session_meta","payload":{"id":"v0129","timestamp":"2026-05-07T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:02Z","type":"response_item","payload":{"type":"custom_tool_call","call_id":"call_patch","name":"apply_patch","input":"*** Begin Patch\n*** Update File: src/main.rs\n..."}}"#,
            r#"{"timestamp":"2026-05-07T10:00:03Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call_patch","output":"{\"output\":\"Applied patch successfully\",\"metadata\":{\"exit_code\":0}}"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:04Z","type":"response_item","payload":{"type":"apply_patch_end","call_id":"call_patch","success":true,"changes":[{"path":"src/main.rs","type":"modified"}]}}"#,
            r#"{"timestamp":"2026-05-07T10:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746614405.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tc = &turns[0].tool_calls[0];
        assert_eq!(tc.kind, ToolKind::PatchApply);
        assert_eq!(tc.name, "apply_patch");
        assert_eq!(tc.status, "completed");
        assert_eq!(tc.patch_success, Some(true));
        let changes = tc
            .patch_changes
            .as_ref()
            .expect("patch_changes should be set");
        assert!(changes.is_array());
        assert_eq!(changes.as_array().unwrap().len(), 1);
        assert_eq!(changes[0]["path"], "src/main.rs");
        assert_eq!(changes[0]["type"], "modified");
    }

    // Codex v0.129.0 (PR #20463): ApplyPatchEnd is now explicitly stored in limited
    // history mode, so a patch_apply_end event_msg may coexist with custom_tool_call_output
    // in the same session. Verify we get exactly one PatchApply entry (no duplicate) and
    // that patch_changes are backfilled from the event rather than lost.
    #[test]
    fn patch_apply_end_event_after_custom_tool_call_output_does_not_create_duplicate() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-07T10:00:00Z","type":"session_meta","payload":{"id":"v0129b","timestamp":"2026-05-07T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:02Z","type":"response_item","payload":{"type":"custom_tool_call","call_id":"call_patch","name":"apply_patch","input":"*** Begin Patch\n..."}}"#,
            r#"{"timestamp":"2026-05-07T10:00:03Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call_patch","output":"{\"output\":\"Patch applied\",\"metadata\":{\"exit_code\":0}}"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:04Z","type":"event_msg","payload":{"type":"patch_apply_end","call_id":"call_patch","success":true,"changes":[{"path":"lib.rs","type":"modified"}],"status":"completed"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746614405.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        // Must have exactly one tool call — no duplicate from the event.
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tc = &turns[0].tool_calls[0];
        assert_eq!(tc.kind, ToolKind::PatchApply);
        assert_eq!(tc.patch_success, Some(true));
        let changes = tc
            .patch_changes
            .as_ref()
            .expect("patch_changes backfilled from event");
        assert_eq!(changes[0]["path"], "lib.rs");
    }

    // Old-format sessions (pre-v0.129.0): custom_tool_call + patch_apply_end event with no
    // custom_tool_call_output. The event still finalizes the call normally.
    #[test]
    fn patch_apply_end_event_finalizes_pending_call_in_old_format() {
        let entries = entries(&[
            r#"{"timestamp":"2026-01-01T10:00:00Z","type":"session_meta","payload":{"id":"old-fmt","timestamp":"2026-01-01T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-01-01T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-01-01T10:00:02Z","type":"response_item","payload":{"type":"custom_tool_call","call_id":"call_old","name":"apply_patch","input":"*** Begin Patch\n..."}}"#,
            r#"{"timestamp":"2026-01-01T10:00:03Z","type":"event_msg","payload":{"type":"patch_apply_end","call_id":"call_old","success":true,"changes":[{"path":"old.rs","type":"created"}],"stdout":"Applied 1 hunk","status":"completed"}}"#,
            r#"{"timestamp":"2026-01-01T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1735725604.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tc = &turns[0].tool_calls[0];
        assert_eq!(tc.kind, ToolKind::PatchApply);
        assert_eq!(tc.name, "apply_patch");
        assert_eq!(tc.patch_success, Some(true));
        assert_eq!(tc.output.as_deref(), Some("Applied 1 hunk"));
        let changes = tc.patch_changes.as_ref().expect("patch_changes from event");
        assert_eq!(changes[0]["path"], "old.rs");
    }

    // Codex v0.129.0 (PRs #20502/#20682): persist_extended_history disabled; app-server
    // always returns a limited history window. codex-trace reads JSONL session files from
    // disk — it never fetches history from the app-server — so all turns recorded in the
    // rollout file are available regardless of the server-side history window. When Codex
    // compacts context in response to the limited window it writes a `compacted` entry,
    // which codex-trace detects via has_compaction. No turns are silently dropped.
    #[test]
    fn v0129_persist_extended_history_disabled_all_turns_captured_compaction_flagged() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-07T10:00:00Z","type":"session_meta","payload":{"id":"long-session","timestamp":"2026-05-07T10:00:00Z","cli_version":"0.129.0"}}"#,
            // Turn 1
            r#"{"timestamp":"2026-05-07T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:10Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746597610.0}}"#,
            // Turn 2
            r#"{"timestamp":"2026-05-07T10:01:00Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2"}}"#,
            r#"{"timestamp":"2026-05-07T10:01:10Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-2","completed_at":1746597670.0}}"#,
            // Turn 3 — Codex hits the limited history window and compacts context mid-turn.
            // The compacted entry records that history was summarised; all three turns are
            // still fully present in the JSONL file and captured by codex-trace.
            r#"{"timestamp":"2026-05-07T10:02:00Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-3"}}"#,
            r#"{"timestamp":"2026-05-07T10:02:01Z","type":"compacted","payload":{"summary":"Summarised previous turns due to history window limit"}}"#,
            r#"{"timestamp":"2026-05-07T10:02:10Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-3","completed_at":1746597730.0}}"#,
        ]);

        let turns = build_turns(&entries);

        // All three turns must be present — no silent truncation even though
        // the app-server only returned a limited history window to Codex CLI.
        assert_eq!(
            turns.len(),
            3,
            "all turns captured despite app-server history limit"
        );
        assert_eq!(turns[0].status, TurnStatus::Complete);
        assert_eq!(turns[1].status, TurnStatus::Complete);
        assert_eq!(turns[2].status, TurnStatus::Complete);

        // Compaction is correctly attributed to turn-3 (where the compacted entry appeared).
        assert!(!turns[0].has_compaction);
        assert!(!turns[1].has_compaction);
        assert!(
            turns[2].has_compaction,
            "turn-3 has_compaction set from compacted entry"
        );
    }

    // Codex v0.130.0 (PR #21454): string-keyed MCP tool maps removed.
    // function_call entries for MCP tools now carry tool_id: { server, tool }
    // instead of a flat namespace string. Verify the tool is still classified as McpTool.
    #[test]
    fn function_call_with_tool_id_classified_as_mcp_tool() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-08T10:00:00Z","type":"session_meta","payload":{"id":"s-v130","timestamp":"2026-05-08T10:00:00Z","cli_version":"0.130.0"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:02Z","type":"response_item","payload":{"type":"function_call","call_id":"mcp-tc1","name":"get_pr_info","tool_id":{"server":"github","tool":"get_pr_info"},"arguments":"{\"pr_number\":42}"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"mcp-tc1","output":"PR #42: Fix the bug"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746698404.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::McpTool);
        assert_eq!(tool.call_id, "mcp-tc1");
        assert_eq!(tool.name, "get_pr_info");
        assert_eq!(tool.mcp_server.as_deref(), Some("github"));
        assert_eq!(tool.mcp_tool.as_deref(), Some("get_pr_info"));
        assert_eq!(tool.output.as_deref(), Some("PR #42: Fix the bug"));
        assert_eq!(tool.status, "completed");
    }

    #[test]
    fn function_call_with_tool_id_multi_segment_server() {
        // tool_id.server may contain __ separators (e.g. "codex_apps__slack")
        let entries = entries(&[
            r#"{"timestamp":"2026-05-08T10:00:00Z","type":"session_meta","payload":{"id":"s-v130b","timestamp":"2026-05-08T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:02Z","type":"response_item","payload":{"type":"function_call","call_id":"mcp-tc2","name":"post_message","tool_id":{"server":"codex_apps__slack","tool":"post_message"},"arguments":"{\"channel\":\"general\",\"text\":\"hello\"}"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"mcp-tc2","output":"ok"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746698404.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::McpTool);
        assert_eq!(tool.mcp_server.as_deref(), Some("codex_apps__slack"));
        assert_eq!(tool.mcp_tool.as_deref(), Some("post_message"));
    }

    // Codex v0.129.0 (PR #21170): experimental `list_dir` tool removed.
    // Sessions captured before v0.129.0 may contain `list_dir` function_call entries.
    // These must parse correctly as Unknown tool calls — not crash, not be silently dropped.
    // Do not add assertions that `list_dir` must exist in new sessions.
    #[test]
    fn list_dir_tool_from_pre_v0129_session_parsed_gracefully() {
        let entries = entries(&[
            r#"{"timestamp":"2025-01-01T10:00:00Z","type":"session_meta","payload":{"id":"old-sess","timestamp":"2025-01-01T10:00:00Z"}}"#,
            r#"{"timestamp":"2025-01-01T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2025-01-01T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"list_dir","arguments":"{\"path\":\"/workspace\"}","call_id":"call_list"}}"#,
            r#"{"timestamp":"2025-01-01T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_list","output":"file1.txt\nfile2.txt\n"}}"#,
            r#"{"timestamp":"2025-01-01T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1735725604.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.name, "list_dir");
        assert_eq!(tool.kind, ToolKind::Unknown);
        assert_eq!(tool.output.as_deref(), Some("file1.txt\nfile2.txt\n"));
        assert_eq!(tool.status, "completed");
    }

    #[test]
    fn function_call_namespace_still_works_without_tool_id() {
        // Pre-v0.130.0 sessions with namespace field must continue to work unchanged.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-08T10:00:00Z","type":"session_meta","payload":{"id":"s-pre130","timestamp":"2026-05-08T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:02Z","type":"response_item","payload":{"type":"function_call","call_id":"mcp-old1","name":"_get_pr_info","namespace":"mcp__codex_apps__github","arguments":"{\"pr_number\":7}"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"mcp-old1","output":"PR #7"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746698404.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::McpTool);
        assert_eq!(tool.mcp_server.as_deref(), Some("codex_apps__github"));
        assert_eq!(tool.mcp_tool.as_deref(), Some("github_get_pr_info"));
        assert_eq!(tool.output.as_deref(), Some("PR #7"));
    }

    // Codex v0.129.0 (PR #21034): /approvals retired; /autoreview renamed to /approve.
    // codex-trace stores user_message verbatim — no pattern matching on command names —
    // so the legacy /autoreview and the new /approve are captured identically, and
    // /approvals entries from older sessions parse without errors or special treatment.
    #[test]
    fn slash_command_rename_autoreview_to_approve_stored_verbatim() {
        let make_entries = |cmd: &str| -> Vec<RawEntry> {
            let user_msg_line = format!(
                r#"{{"timestamp":"2026-05-07T10:00:02Z","type":"event_msg","payload":{{"type":"user_message","message":"{cmd}"}}}}"#
            );
            entries(&[
                r#"{"timestamp":"2026-05-07T10:00:00Z","type":"session_meta","payload":{"id":"s-cmd","timestamp":"2026-05-07T10:00:00Z","cli_version":"0.129.0"}}"#,
                r#"{"timestamp":"2026-05-07T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                &user_msg_line,
                r#"{"timestamp":"2026-05-07T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746604803.0}}"#,
            ])
        };

        for cmd in &["/autoreview", "/approve", "/approvals"] {
            let turns = build_turns(&make_entries(cmd));
            assert_eq!(turns.len(), 1, "expected 1 turn for command {cmd}");
            assert_eq!(
                turns[0].user_message.as_deref(),
                Some(*cmd),
                "user_message must equal the raw command string for {cmd}"
            );
            assert_eq!(turns[0].status, TurnStatus::Complete);
        }
    }

    // Codex v0.130.0 (PR #21356): built-in MCPs promoted to first-class runtime servers.
    // After this change built-in servers (e.g. computer_use) appear in session logs with
    // the same event structure and tool_id fields as user-configured MCP servers.
    // codex-trace must parse them identically — no origin-based filtering or exclusion.

    #[test]
    fn builtin_mcp_via_tool_id_classified_as_mcp_tool() {
        // Built-in server "computer_use" arriving via v0.130.0 tool_id format.
        // It must be treated identically to a user-configured server.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-08T10:00:00Z","type":"session_meta","payload":{"id":"s-builtin-v130","timestamp":"2026-05-08T10:00:00Z","cli_version":"0.130.0"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:02Z","type":"response_item","payload":{"type":"function_call","call_id":"builtin-1","name":"screenshot","tool_id":{"server":"computer_use","tool":"screenshot"},"arguments":"{}"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"builtin-1","output":"screenshot taken"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746698404.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::McpTool);
        assert_eq!(tool.call_id, "builtin-1");
        assert_eq!(tool.name, "screenshot");
        assert_eq!(tool.mcp_server.as_deref(), Some("computer_use"));
        assert_eq!(tool.mcp_tool.as_deref(), Some("screenshot"));
        assert_eq!(tool.output.as_deref(), Some("screenshot taken"));
        assert_eq!(tool.status, "completed");
    }

    #[test]
    fn builtin_mcp_via_mcp_tool_call_response_item_classified_correctly() {
        // Built-in server "computer_use" arriving via v0.129.0+ mcp_tool_call response_item.
        // PR #21356 promotes built-in MCPs so they emit the same mcp_tool_call entries
        // as user-configured servers. The parser must not exclude them.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-08T10:00:00Z","type":"session_meta","payload":{"id":"s-builtin-mcp","timestamp":"2026-05-08T10:00:00Z","cli_version":"0.130.0"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:02Z","type":"response_item","payload":{"type":"mcp_tool_call","call_id":"builtin-mcp-1","server":"computer_use","tool":"click","arguments":{"x":100,"y":200}}}"#,
            r#"{"timestamp":"2026-05-08T10:00:03Z","type":"response_item","payload":{"type":"mcp_tool_call_output","call_id":"builtin-mcp-1","output":[{"type":"text","text":"clicked at (100,200)"}]}}"#,
            r#"{"timestamp":"2026-05-08T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746698404.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::McpTool);
        assert_eq!(tool.call_id, "builtin-mcp-1");
        assert_eq!(tool.name, "click");
        assert_eq!(tool.mcp_server.as_deref(), Some("computer_use"));
        assert_eq!(tool.mcp_tool.as_deref(), Some("click"));
        assert_eq!(tool.output.as_deref(), Some("clicked at (100,200)"));
        assert_eq!(tool.status, "completed");
    }

    #[test]
    fn builtin_mcp_and_user_mcp_in_same_session_both_classified() {
        // A session with both a built-in server (computer_use) and a user-configured server
        // (github) in the same turn. Both must be classified as McpTool with correct server names.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-08T10:00:00Z","type":"session_meta","payload":{"id":"s-mixed-mcp","timestamp":"2026-05-08T10:00:00Z","cli_version":"0.130.0"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:02Z","type":"response_item","payload":{"type":"function_call","call_id":"builtin-c1","name":"screenshot","tool_id":{"server":"computer_use","tool":"screenshot"},"arguments":"{}"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"builtin-c1","output":"screenshot.png"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:04Z","type":"response_item","payload":{"type":"function_call","call_id":"user-c1","name":"get_pr_info","tool_id":{"server":"github","tool":"get_pr_info"},"arguments":"{\"pr_number\":1}"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:05Z","type":"response_item","payload":{"type":"function_call_output","call_id":"user-c1","output":"PR #1 info"}}"#,
            r#"{"timestamp":"2026-05-08T10:00:06Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746698406.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 2);

        let builtin_tool = turns[0]
            .tool_calls
            .iter()
            .find(|t| t.call_id == "builtin-c1")
            .expect("built-in tool call missing");
        assert_eq!(builtin_tool.kind, ToolKind::McpTool);
        assert_eq!(builtin_tool.mcp_server.as_deref(), Some("computer_use"));
        assert_eq!(builtin_tool.mcp_tool.as_deref(), Some("screenshot"));

        let user_tool = turns[0]
            .tool_calls
            .iter()
            .find(|t| t.call_id == "user-c1")
            .expect("user-configured tool call missing");
        assert_eq!(user_tool.kind, ToolKind::McpTool);
        assert_eq!(user_tool.mcp_server.as_deref(), Some("github"));
        assert_eq!(user_tool.mcp_tool.as_deref(), Some("get_pr_info"));
    }

    // Codex v0.133.0 (PRs #23353, #23737): plugin_id added to MCP tool call items.

    #[test]
    fn mcp_tool_call_with_plugin_id_is_captured() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-21T10:00:00Z","type":"session_meta","payload":{"id":"s-pid1","timestamp":"2026-05-21T10:00:00Z","cli_version":"0.133.0"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:02Z","type":"response_item","payload":{"type":"mcp_tool_call","call_id":"mcp-pid1","server":"github","tool":"get_pr_info","plugin_id":"plugin-abc","arguments":{"pr_number":1}}}"#,
            r#"{"timestamp":"2026-05-21T10:00:03Z","type":"response_item","payload":{"type":"mcp_tool_call_output","call_id":"mcp-pid1","output":"PR info"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747821604.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::McpTool);
        assert_eq!(tool.mcp_server.as_deref(), Some("github"));
        assert_eq!(tool.plugin_id.as_deref(), Some("plugin-abc"));
    }

    #[test]
    fn function_call_with_tool_id_plugin_id_is_captured() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-21T10:00:00Z","type":"session_meta","payload":{"id":"s-pid2","timestamp":"2026-05-21T10:00:00Z","cli_version":"0.133.0"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"get_pr_info","call_id":"fc-pid1","arguments":"{}","tool_id":{"server":"github","tool":"get_pr_info","plugin_id":"plugin-xyz"}}}"#,
            r#"{"timestamp":"2026-05-21T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"fc-pid1","output":"result"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747821604.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::McpTool);
        assert_eq!(tool.mcp_server.as_deref(), Some("github"));
        assert_eq!(tool.plugin_id.as_deref(), Some("plugin-xyz"));
    }

    #[test]
    fn mcp_tool_call_without_plugin_id_has_none() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-07T10:00:00Z","type":"session_meta","payload":{"id":"s-nopid","timestamp":"2026-05-07T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:02Z","type":"response_item","payload":{"type":"mcp_tool_call","call_id":"mcp-nopid","server":"slack","tool":"list_channels","arguments":{}}}"#,
            r#"{"timestamp":"2026-05-07T10:00:03Z","type":"response_item","payload":{"type":"mcp_tool_call_output","call_id":"mcp-nopid","output":"[]"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747821604.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::McpTool);
        assert!(
            tool.plugin_id.is_none(),
            "pre-v0.133.0 MCP call must have no plugin_id"
        );
    }

    // Codex v0.133.0 compat: PR #23075 removed the UserTurn response_item variant.
    // Pre-v0.133.0 transcripts contain response_items with type "user_turn"; codex-trace
    // must extract the user message so the turn is not left with no user_message.

    #[test]
    fn user_turn_response_item_string_content_is_migrated() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"s-ut1","timestamp":"2026-05-20T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"response_item","payload":{"type":"user_turn","content":"Hello from old Codex"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747734003.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].user_message.as_deref(),
            Some("Hello from old Codex")
        );
    }

    #[test]
    fn user_turn_response_item_content_array_is_migrated() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"s-ut2","timestamp":"2026-05-20T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"response_item","payload":{"type":"user_turn","content":[{"type":"text","text":"Multi-block "},{"type":"text","text":"user input"}]}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747734003.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].user_message.as_deref(),
            Some("Multi-block user input")
        );
    }

    #[test]
    fn user_turn_does_not_overwrite_existing_user_message() {
        // If a user_message event_msg already set the message, user_turn must not overwrite it.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"s-ut3","timestamp":"2026-05-20T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"Primary message"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"response_item","payload":{"type":"user_turn","content":"Should be ignored"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747734004.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user_message.as_deref(), Some("Primary message"));
    }

    // Codex v0.133.0 compat: PR #23081 removed UserInputWithTurnContext.
    // Pre-v0.133.0 transcripts may contain response_items with type
    // "user_input_with_turn_context" bundling user text and context metadata.

    #[test]
    fn user_input_with_turn_context_extracts_message_and_context() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"s-uitc1","timestamp":"2026-05-20T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"response_item","payload":{"type":"user_input_with_turn_context","input":{"content":"Fix the bug"},"context":{"cwd":"/project","model":"gpt-5","effort":"high"}}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747734003.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user_message.as_deref(), Some("Fix the bug"));
        assert_eq!(turns[0].cwd.as_deref(), Some("/project"));
        assert_eq!(turns[0].model.as_deref(), Some("gpt-5"));
        assert_eq!(turns[0].reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn user_input_with_turn_context_input_as_plain_string() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"s-uitc2","timestamp":"2026-05-20T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"response_item","payload":{"type":"user_input_with_turn_context","input":"Plain string input","context":{"cwd":"/home/user"}}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747734003.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user_message.as_deref(), Some("Plain string input"));
        assert_eq!(turns[0].cwd.as_deref(), Some("/home/user"));
    }

    // Codex v0.133.0+ (PRs #23080, #22508): UserTurn and UserInputWithTurnContext were
    // replaced by a split UserInput + ThreadSettings model.

    #[test]
    fn user_input_response_item_string_content_is_captured() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-21T10:00:00Z","type":"session_meta","payload":{"id":"s-ui1","timestamp":"2026-05-21T10:00:00Z","cli_version":"0.133.0"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:02Z","type":"response_item","payload":{"type":"user_input","content":"Hello from new Codex"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748167203.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].user_message.as_deref(),
            Some("Hello from new Codex")
        );
    }

    #[test]
    fn user_input_response_item_content_array_is_captured() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-21T10:00:00Z","type":"session_meta","payload":{"id":"s-ui2","timestamp":"2026-05-21T10:00:00Z","cli_version":"0.133.0"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:02Z","type":"response_item","payload":{"type":"user_input","content":[{"type":"text","text":"Fix "},{"type":"text","text":"the bug"}]}}"#,
            r#"{"timestamp":"2026-05-21T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748167203.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user_message.as_deref(), Some("Fix the bug"));
    }

    #[test]
    fn v0153_desktop_messages_and_structured_custom_exec_are_parsed() {
        // Codex Desktop v0.153 records prompts as response_item/message entries and
        // its `exec` tool as custom_tool_call with an array-valued output.
        let entries = entries(&[
            r#"{"timestamp":"2026-09-06T10:00:00Z","type":"session_meta","payload":{"id":"desktop-v153","timestamp":"2026-09-06T10:00:00Z","cli_version":"0.153.3"}}"#,
            r#"{"timestamp":"2026-09-06T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-desktop"}}"#,
            r#"{"timestamp":"2026-09-06T10:00:02Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Injected context"}],"internal_chat_message_metadata_passthrough":{"turn_id":"turn-desktop"}}}"#,
            r#"{"timestamp":"2026-09-06T10:00:03Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Actual request"}],"internal_chat_message_metadata_passthrough":{"turn_id":"turn-desktop"}}}"#,
            r#"{"timestamp":"2026-09-06T10:00:04Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"custom-exec","input":"const result = await run();","internal_chat_message_metadata_passthrough":{"turn_id":"turn-desktop"}}}"#,
            r#"{"timestamp":"2026-09-06T10:00:05Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"custom-exec","output":[{"type":"input_text","text":"Script completed\n"},{"type":"input_text","text":"result output"}],"internal_chat_message_metadata_passthrough":{"turn_id":"turn-desktop"}}}"#,
            r#"{"timestamp":"2026-09-06T10:00:06Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-desktop","completed_at":1788688806.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user_message.as_deref(), Some("Actual request"));
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::Unknown);
        assert_eq!(tool.name, "exec");
        assert_eq!(
            tool.input_text.as_deref(),
            Some("const result = await run();")
        );
        assert_eq!(
            tool.output.as_deref(),
            Some("Script completed\nresult output")
        );
        assert_eq!(tool.exit_code, None);
        assert_eq!(tool.status, "completed");
    }

    #[test]
    fn user_input_does_not_overwrite_existing_user_message() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-21T10:00:00Z","type":"session_meta","payload":{"id":"s-ui3","timestamp":"2026-05-21T10:00:00Z","cli_version":"0.133.0"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"Primary message"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:03Z","type":"response_item","payload":{"type":"user_input","content":"Should be ignored"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748167204.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user_message.as_deref(), Some("Primary message"));
    }

    // Codex v0.150.0 (PRs #40308, #40315): user_message task-mention encoding is decoded to
    // readable text and the referenced tasks are captured on the turn.

    #[test]
    fn user_message_task_mention_is_decoded_and_captured() {
        let entries = entries(&[
            r#"{"timestamp":"2026-08-01T10:00:00Z","type":"session_meta","payload":{"id":"s-tm1","timestamp":"2026-08-01T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-08-01T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-08-01T10:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"Please check [@Review database migration](thread://abc123) before merging."}}"#,
            r#"{"timestamp":"2026-08-01T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1754042403.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].user_message.as_deref(),
            Some("Please check @Review database migration before merging.")
        );
        assert_eq!(turns[0].task_mentions.len(), 1);
        assert_eq!(turns[0].task_mentions[0].title, "Review database migration");
        assert_eq!(turns[0].task_mentions[0].thread_id, "abc123");
    }

    #[test]
    fn user_message_without_task_mention_leaves_task_mentions_empty() {
        let entries = entries(&[
            r#"{"timestamp":"2026-08-01T10:00:00Z","type":"session_meta","payload":{"id":"s-tm2","timestamp":"2026-08-01T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-08-01T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-08-01T10:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"Just a normal message."}}"#,
            r#"{"timestamp":"2026-08-01T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1754042403.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].user_message.as_deref(),
            Some("Just a normal message.")
        );
        assert!(turns[0].task_mentions.is_empty());
    }

    #[test]
    fn thread_settings_response_item_captures_context_fields() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-21T10:00:00Z","type":"session_meta","payload":{"id":"s-ts1","timestamp":"2026-05-21T10:00:00Z","cli_version":"0.133.0"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:02Z","type":"response_item","payload":{"type":"user_input","content":"Run tests"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:03Z","type":"response_item","payload":{"type":"thread_settings","model":"gpt-5","cwd":"/workspace","effort":"high"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748167204.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user_message.as_deref(), Some("Run tests"));
        assert_eq!(turns[0].model.as_deref(), Some("gpt-5"));
        assert_eq!(turns[0].cwd.as_deref(), Some("/workspace"));
        assert_eq!(turns[0].reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn thread_settings_partial_fields_are_applied() {
        // thread_settings may omit some fields; only present fields should be applied.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-21T10:00:00Z","type":"session_meta","payload":{"id":"s-ts2","timestamp":"2026-05-21T10:00:00Z","cli_version":"0.133.0"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:02Z","type":"response_item","payload":{"type":"thread_settings","model":"gpt-5"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748167203.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].model.as_deref(), Some("gpt-5"));
        assert!(turns[0].cwd.is_none());
        assert!(turns[0].reasoning_effort.is_none());
    }

    #[test]
    fn v0133_full_session_with_user_input_and_thread_settings() {
        // Full v0.133.0+ session: user_input + thread_settings replace the old user_turn.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-21T10:00:00Z","type":"session_meta","payload":{"id":"v0133-session","timestamp":"2026-05-21T10:00:00Z","cwd":"/project","cli_version":"0.133.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:02Z","type":"response_item","payload":{"type":"user_input","content":"Write a test for the parser"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:03Z","type":"response_item","payload":{"type":"thread_settings","model":"gpt-5","cwd":"/project","effort":"medium"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:04Z","type":"event_msg","payload":{"type":"agent_message","message":"I'll write a test for the parser.","phase":"main"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748167205.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Complete);
        assert_eq!(
            turns[0].user_message.as_deref(),
            Some("Write a test for the parser")
        );
        assert_eq!(turns[0].model.as_deref(), Some("gpt-5"));
        assert_eq!(turns[0].cwd.as_deref(), Some("/project"));
        assert_eq!(turns[0].reasoning_effort.as_deref(), Some("medium"));
        assert_eq!(turns[0].agent_messages.len(), 1);
        assert_eq!(
            turns[0].agent_messages[0].text,
            "I'll write a test for the parser."
        );
    }

    // Codex v0.133.0 compat: PR #22709 trimmed unused TurnContextItem fields.
    // Pre-v0.133.0 transcripts have extra fields in turn_context payloads; new transcripts
    // have fewer. The parser must handle both without panicking or losing data.

    #[test]
    fn turn_context_with_extra_legacy_fields_does_not_panic() {
        // Old transcripts may include fields that were trimmed in v0.133.0.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"s-tc","timestamp":"2026-05-20T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/tmp","effort":"medium","legacy_field_a":"ignored","legacy_field_b":42,"context_window":128000}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747734003.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].model.as_deref(), Some("gpt-5"));
        assert_eq!(turns[0].cwd.as_deref(), Some("/tmp"));
        assert_eq!(turns[0].reasoning_effort.as_deref(), Some("medium"));
    }

    #[test]
    fn turn_context_with_missing_trimmed_fields_does_not_panic() {
        // New transcripts (v0.133.0+) may omit fields that older transcripts had.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"s-tc2","timestamp":"2026-05-20T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"turn_context","payload":{"model":"gpt-5"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747734003.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].model.as_deref(), Some("gpt-5"));
        assert!(turns[0].cwd.is_none());
        assert!(turns[0].reasoning_effort.is_none());
    }

    // Codex v0.149.0 (PR #39662): "max" and "ultra" were added as valid reasoning-effort
    // values alongside minimal/low/medium/high. reasoning_effort is stored as a plain
    // Option<String> with no enum validation, so these new values must pass through
    // unchanged from both turn_context and thread_settings payloads.
    #[test]
    fn turn_context_accepts_max_reasoning_effort() {
        let entries = entries(&[
            r#"{"timestamp":"2026-08-20T10:00:00Z","type":"session_meta","payload":{"id":"s-effort-max","timestamp":"2026-08-20T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-08-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-08-20T10:00:02Z","type":"turn_context","payload":{"model":"gpt-5.4","cwd":"/tmp","effort":"max"}}"#,
            r#"{"timestamp":"2026-08-20T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1755684003.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].reasoning_effort.as_deref(), Some("max"));
    }

    #[test]
    fn thread_settings_accepts_ultra_reasoning_effort() {
        let entries = entries(&[
            r#"{"timestamp":"2026-08-20T10:00:00Z","type":"session_meta","payload":{"id":"s-effort-ultra","timestamp":"2026-08-20T10:00:00Z","cli_version":"0.149.0"}}"#,
            r#"{"timestamp":"2026-08-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-08-20T10:00:02Z","type":"response_item","payload":{"type":"user_input","content":"Ship the feature"}}"#,
            r#"{"timestamp":"2026-08-20T10:00:03Z","type":"response_item","payload":{"type":"thread_settings","model":"gpt-5.4","cwd":"/workspace","effort":"ultra"}}"#,
            r#"{"timestamp":"2026-08-20T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1755684004.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].reasoning_effort.as_deref(), Some("ultra"));
    }

    // Codex v0.131.0 (PR #22268): collab_agent_spawn_end event payload field renamed
    // new_thread_id → new_session_id. Verify the parser reads new_session_id as a fallback.
    #[test]
    fn links_spawn_agent_from_collab_spawn_end_with_new_session_id() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-18T10:00:00Z","type":"session_meta","payload":{"id":"parent-sess","timestamp":"2026-05-18T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-18T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-18T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"message\":\"Do work\"}","call_id":"call_spawn_v131"}}"#,
            r#"{"timestamp":"2026-05-18T10:00:03Z","type":"event_msg","payload":{"type":"collab_agent_spawn_end","call_id":"call_spawn_v131","sender_session_id":"parent-sess","new_session_id":"worker-sess-v131","new_agent_nickname":"Turing","new_agent_role":"worker","prompt":"Do work","status":"pending_init"}}"#,
            r#"{"timestamp":"2026-05-18T10:00:04Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_spawn_v131","output":"{\"agent_id\":\"worker-sess-v131\",\"nickname\":\"Turing\"}"}}"#,
            r#"{"timestamp":"2026-05-18T10:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747562405.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].collab_spawns.len(), 1);
        assert_eq!(turns[0].collab_spawns[0].new_session_id, "worker-sess-v131");
        assert_eq!(turns[0].collab_spawns[0].agent_nickname, "Turing");
        assert_eq!(turns[0].tool_calls.len(), 1);
        assert_eq!(turns[0].tool_calls[0].kind, ToolKind::SpawnAgent);
    }

    // Codex v0.133.0 (PRs #23300, #23685, #23696, #23732): Goals feature is now on by
    // default. Goal lifecycle events are emitted as event_msg turn items interleaved with
    // normal session events. Verify they are gracefully skipped and do not corrupt turns.

    #[test]
    fn goal_created_event_interleaved_in_turn_is_skipped() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-21T10:00:00Z","type":"session_meta","payload":{"id":"goal-session","timestamp":"2026-05-21T10:00:00Z","cwd":"/tmp","cli_version":"0.133.0"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:02Z","type":"event_msg","payload":{"type":"goal_created","goal_id":"goal-abc","title":"Write tests","status":"active"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:03Z","type":"event_msg","payload":{"type":"agent_message","message":"I'll write the tests now.","phase":"main"}}"#,
            r#"{"timestamp":"2026-05-21T10:00:04Z","type":"event_msg","payload":{"type":"goal_updated","goal_id":"goal-abc","progress":0.5}}"#,
            r#"{"timestamp":"2026-05-21T10:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748167205.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Complete);
        assert_eq!(turns[0].agent_messages.len(), 1);
        assert_eq!(turns[0].agent_messages[0].text, "I'll write the tests now.");
    }

    #[test]
    fn all_goal_lifecycle_events_are_skipped_gracefully() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-21T10:01:00Z","type":"session_meta","payload":{"id":"goal-session-2","timestamp":"2026-05-21T10:01:00Z","cwd":"/tmp","cli_version":"0.133.0"}}"#,
            r#"{"timestamp":"2026-05-21T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-21T10:01:02Z","type":"event_msg","payload":{"type":"goal_created","goal_id":"g1","title":"Goal 1"}}"#,
            r#"{"timestamp":"2026-05-21T10:01:03Z","type":"event_msg","payload":{"type":"goal_updated","goal_id":"g1","progress":0.3}}"#,
            r#"{"timestamp":"2026-05-21T10:01:04Z","type":"event_msg","payload":{"type":"goal_paused","goal_id":"g1","reason":"waiting"}}"#,
            r#"{"timestamp":"2026-05-21T10:01:05Z","type":"event_msg","payload":{"type":"goal_completed","goal_id":"g1","outcome":"success"}}"#,
            r#"{"timestamp":"2026-05-21T10:01:06Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748167266.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Complete);
        // Goal events must not appear as agent messages or tool calls
        assert!(turns[0].agent_messages.is_empty());
        assert!(turns[0].tool_calls.is_empty());
    }

    #[test]
    fn goal_events_across_multiple_turns_are_all_skipped() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-21T10:02:00Z","type":"session_meta","payload":{"id":"goal-session-3","timestamp":"2026-05-21T10:02:00Z","cli_version":"0.133.0"}}"#,
            r#"{"timestamp":"2026-05-21T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-21T10:02:02Z","type":"event_msg","payload":{"type":"goal_created","goal_id":"g1","title":"First goal"}}"#,
            r#"{"timestamp":"2026-05-21T10:02:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748167323.0}}"#,
            r#"{"timestamp":"2026-05-21T10:02:04Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2"}}"#,
            r#"{"timestamp":"2026-05-21T10:02:05Z","type":"event_msg","payload":{"type":"goal_updated","goal_id":"g1","progress":0.8}}"#,
            r#"{"timestamp":"2026-05-21T10:02:06Z","type":"event_msg","payload":{"type":"goal_completed","goal_id":"g1","outcome":"done"}}"#,
            r#"{"timestamp":"2026-05-21T10:02:07Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-2","completed_at":1748167327.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].status, TurnStatus::Complete);
        assert_eq!(turns[1].status, TurnStatus::Complete);
    }

    // Codex v0.134.0 (PR #23980): trace_id added to TurnStartedEvent for OTel correlation.

    #[test]
    fn v0134_trace_id_in_task_started_is_captured() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-26T10:00:00Z","type":"session_meta","payload":{"id":"v0134-sess","timestamp":"2026-05-26T10:00:00Z","cli_version":"0.134.0"}}"#,
            r#"{"timestamp":"2026-05-26T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","trace_id":"abc-trace-xyz-123"}}"#,
            r#"{"timestamp":"2026-05-26T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748254802.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].trace_id.as_deref(), Some("abc-trace-xyz-123"));
    }

    #[test]
    fn v0134_absent_trace_id_is_none_for_older_sessions() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-25T10:00:00Z","type":"session_meta","payload":{"id":"pre-v0134","timestamp":"2026-05-25T10:00:00Z","cli_version":"0.133.0"}}"#,
            r#"{"timestamp":"2026-05-25T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-25T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748168402.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert!(
            turns[0].trace_id.is_none(),
            "pre-v0.134.0 sessions must have no trace_id"
        );
    }

    // Codex v0.135.0 (PR #24160): forked_from_thread_id added to turn metadata.

    #[test]
    fn v0135_forked_from_thread_id_in_task_started_is_captured() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-28T10:00:00Z","type":"session_meta","payload":{"id":"v0135-fork","timestamp":"2026-05-28T10:00:00Z","cli_version":"0.135.0"}}"#,
            r#"{"timestamp":"2026-05-28T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","forked_from_thread_id":"parent-thread-abc"}}"#,
            r#"{"timestamp":"2026-05-28T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748426402.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].forked_from_thread_id.as_deref(),
            Some("parent-thread-abc")
        );
    }

    #[test]
    fn v0135_absent_forked_from_thread_id_is_none_for_non_forked_turns() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-28T10:00:00Z","type":"session_meta","payload":{"id":"v0135-nofork","timestamp":"2026-05-28T10:00:00Z","cli_version":"0.135.0"}}"#,
            r#"{"timestamp":"2026-05-28T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-28T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748426402.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert!(
            turns[0].forked_from_thread_id.is_none(),
            "non-forked turn must have no forked_from_thread_id"
        );
    }

    // Codex v0.135.0 (PR #24368): compaction metadata added to turn headers.

    #[test]
    fn v0135_compaction_meta_in_task_started_is_captured() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-28T11:00:00Z","type":"session_meta","payload":{"id":"v0135-cmeta","timestamp":"2026-05-28T11:00:00Z","cli_version":"0.135.0"}}"#,
            r#"{"timestamp":"2026-05-28T11:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","compaction":{"tokens_before":120000,"tokens_after":45000,"summary":"Summarised earlier turns"}}}"#,
            r#"{"timestamp":"2026-05-28T11:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748430002.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        let meta = turns[0]
            .compaction_meta
            .as_ref()
            .expect("compaction_meta must be present");
        assert_eq!(meta.tokens_before, Some(120000));
        assert_eq!(meta.tokens_after, Some(45000));
        assert_eq!(meta.summary.as_deref(), Some("Summarised earlier turns"));
        assert!(
            meta.compaction_trigger.is_none(),
            "compaction_trigger absent from payload must be None"
        );
    }

    #[test]
    fn v0135_compaction_trigger_auto_is_captured() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-28T11:00:00Z","type":"session_meta","payload":{"id":"v0135-ctrigger-auto","timestamp":"2026-05-28T11:00:00Z","cli_version":"0.135.0"}}"#,
            r#"{"timestamp":"2026-05-28T11:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","compaction":{"tokens_before":200000,"tokens_after":60000,"compaction_trigger":"auto"}}}"#,
            r#"{"timestamp":"2026-05-28T11:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748430002.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        let meta = turns[0]
            .compaction_meta
            .as_ref()
            .expect("compaction_meta must be present");
        assert_eq!(meta.compaction_trigger.as_deref(), Some("auto"));
    }

    #[test]
    fn v0135_compaction_trigger_manual_is_captured() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-28T11:00:00Z","type":"session_meta","payload":{"id":"v0135-ctrigger-manual","timestamp":"2026-05-28T11:00:00Z","cli_version":"0.135.0"}}"#,
            r#"{"timestamp":"2026-05-28T11:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","compaction":{"tokens_before":150000,"tokens_after":50000,"summary":"User-requested compaction","compaction_trigger":"manual"}}}"#,
            r#"{"timestamp":"2026-05-28T11:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748430002.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        let meta = turns[0]
            .compaction_meta
            .as_ref()
            .expect("compaction_meta must be present");
        assert_eq!(meta.compaction_trigger.as_deref(), Some("manual"));
        assert_eq!(meta.summary.as_deref(), Some("User-requested compaction"));
        assert_eq!(meta.tokens_before, Some(150000));
        assert_eq!(meta.tokens_after, Some(50000));
    }

    #[test]
    fn v0135_absent_compaction_meta_is_none_for_uncompacted_turns() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-28T11:00:00Z","type":"session_meta","payload":{"id":"v0135-nocomp","timestamp":"2026-05-28T11:00:00Z","cli_version":"0.135.0"}}"#,
            r#"{"timestamp":"2026-05-28T11:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-28T11:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748430002.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert!(
            turns[0].compaction_meta.is_none(),
            "turns without compaction header must have no compaction_meta"
        );
    }

    #[test]
    fn v0135_all_three_new_fields_in_same_task_started() {
        // All three v0.134.0/v0.135.0 fields may appear together in a single task_started.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-28T12:00:00Z","type":"session_meta","payload":{"id":"v0135-all","timestamp":"2026-05-28T12:00:00Z","cli_version":"0.135.0"}}"#,
            r#"{"timestamp":"2026-05-28T12:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","trace_id":"otel-trace-001","forked_from_thread_id":"parent-thread-xyz","compaction":{"tokens_before":80000,"tokens_after":30000}}}"#,
            r#"{"timestamp":"2026-05-28T12:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748433602.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].trace_id.as_deref(), Some("otel-trace-001"));
        assert_eq!(
            turns[0].forked_from_thread_id.as_deref(),
            Some("parent-thread-xyz")
        );
        let meta = turns[0].compaction_meta.as_ref().expect("compaction_meta");
        assert_eq!(meta.tokens_before, Some(80000));
        assert_eq!(meta.tokens_after, Some(30000));
        assert!(meta.summary.is_none());
    }

    // Codex v0.135.0 (PR #24591): memory state moved from file-based storage to a dedicated
    // SQLite DB. Active memories are now injected into context at turn start and written into
    // the turn_context JSONL event. codex-trace must parse and expose them on CodexTurn.

    #[test]
    fn turn_context_with_memories_parsed_correctly() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-28T10:00:00Z","type":"session_meta","payload":{"id":"v0135-mem","timestamp":"2026-05-28T10:00:00Z","cwd":"/project","cli_version":"0.135.0"}}"#,
            r#"{"timestamp":"2026-05-28T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-28T10:00:02Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project","memories":["User prefers terse output","Project uses TypeScript strict mode"]}}"#,
            r#"{"timestamp":"2026-05-28T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748426403.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].memories.len(), 2);
        assert_eq!(turns[0].memories[0].content, "User prefers terse output");
        assert_eq!(turns[0].memories[0].version, None);
        assert_eq!(
            turns[0].memories[1].content,
            "Project uses TypeScript strict mode"
        );
        assert_eq!(turns[0].memories[1].version, None);
        assert_eq!(turns[0].model.as_deref(), Some("gpt-5"));
    }

    #[test]
    fn turn_context_without_memories_produces_empty_vec() {
        // Pre-v0.135.0 sessions: turn_context has no memories field → empty Vec, not None/panic.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"v0134-nomem","timestamp":"2026-05-20T10:00:00Z","cli_version":"0.134.0"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/tmp","effort":"medium"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747734003.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert!(turns[0].memories.is_empty());
    }

    #[test]
    fn memories_preserved_across_multiple_turns() {
        // Each turn_context carries its own memories snapshot; last one wins per turn.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-28T10:00:00Z","type":"session_meta","payload":{"id":"v0135-multiturn","timestamp":"2026-05-28T10:00:00Z","cli_version":"0.135.0"}}"#,
            r#"{"timestamp":"2026-05-28T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-28T10:00:02Z","type":"turn_context","payload":{"model":"gpt-5","memories":["Initial memory"]}}"#,
            r#"{"timestamp":"2026-05-28T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748426403.0}}"#,
            r#"{"timestamp":"2026-05-28T10:00:04Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2"}}"#,
            r#"{"timestamp":"2026-05-28T10:00:05Z","type":"turn_context","payload":{"model":"gpt-5","memories":["Initial memory","New memory added"]}}"#,
            r#"{"timestamp":"2026-05-28T10:00:06Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-2","completed_at":1748426406.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].memories.len(), 1);
        assert_eq!(turns[0].memories[0].content, "Initial memory");
        assert_eq!(turns[0].memories[0].version, None);
        assert_eq!(turns[1].memories.len(), 2);
        assert_eq!(turns[1].memories[0].content, "Initial memory");
        assert_eq!(turns[1].memories[1].content, "New memory added");
    }

    // Codex v0.132.0 (PR #23148): memory summaries are now versioned and rebuilt when the
    // stored format is stale. Memory items in turn_context are now objects
    // {"content":"...","version":N} instead of plain strings. The parser must handle both
    // formats for backward compatibility.

    #[test]
    fn v0132_versioned_memory_summary_parsed_with_version_field() {
        // v0.132.0+: memory items are objects {"content":"...","version":N}
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"v0132-mem","timestamp":"2026-05-20T10:00:00Z","cwd":"/project","cli_version":"0.132.0"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"turn_context","payload":{"model":"gpt-5","memories":[{"content":"User prefers terse output","version":1},{"content":"Project uses TypeScript","version":2}]}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748253603.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].memories.len(), 2);
        assert_eq!(turns[0].memories[0].content, "User prefers terse output");
        assert_eq!(turns[0].memories[0].version, Some(1));
        assert_eq!(turns[0].memories[1].content, "Project uses TypeScript");
        assert_eq!(turns[0].memories[1].version, Some(2));
    }

    #[test]
    fn v0132_versioned_memory_summary_without_version_field_is_tolerated() {
        // Object format but missing the version field — version must be None, not a panic.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"v0132-mem-noversion","timestamp":"2026-05-20T10:00:00Z","cli_version":"0.132.0"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"turn_context","payload":{"model":"gpt-5","memories":[{"content":"Remember to be concise"}]}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748253603.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].memories.len(), 1);
        assert_eq!(turns[0].memories[0].content, "Remember to be concise");
        assert_eq!(turns[0].memories[0].version, None);
    }

    #[test]
    fn v0132_memory_summaries_backward_compatible_with_plain_strings() {
        // Pre-v0.132.0 sessions use plain strings — must still parse without error.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"pre-v0132-mem","timestamp":"2026-05-20T10:00:00Z","cli_version":"0.131.0"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"turn_context","payload":{"model":"gpt-5","memories":["Plain string memory"]}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748253603.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].memories.len(), 1);
        assert_eq!(turns[0].memories[0].content, "Plain string memory");
        assert_eq!(turns[0].memories[0].version, None);
    }

    // Codex v0.135.0 (PR #24652): plain image wrapper spans removed from session output.
    // image_generation function calls must be classified as ImageGeneration with image_prompt
    // extracted from arguments. The output array may contain bare image_url items (v0.135.0+)
    // rather than image_span wrappers — the parser must not look for image_span.

    #[test]
    fn v0135_image_generation_tool_call_classified_as_image_generation() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-28T10:00:00Z","type":"session_meta","payload":{"id":"v0135-img","timestamp":"2026-05-28T10:00:00Z","cli_version":"0.135.0"}}"#,
            r#"{"timestamp":"2026-05-28T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-28T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"image_generation","call_id":"call_img","arguments":"{\"prompt\":\"a sunset over mountains\"}"}}"#,
            // v0.135.0+: output is a bare image_url item — no image_span wrapper.
            r#"{"timestamp":"2026-05-28T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_img","output":[{"type":"image_url","image_url":{"url":"data:image/png;base64,abc123"}}]}}"#,
            r#"{"timestamp":"2026-05-28T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748426404.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);

        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::ImageGeneration);
        assert_eq!(
            tool.image_prompt.as_deref(),
            Some("a sunset over mountains")
        );
        assert_eq!(tool.status, "completed");
    }

    #[test]
    fn v0135_image_generation_without_wrapper_span_does_not_yield_unknown_kind() {
        // Before v0.135.0, an image_span wrapper might have been present. With v0.135.0,
        // it is absent. The kind must be ImageGeneration regardless of output format.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-28T10:00:00Z","type":"session_meta","payload":{"id":"v0135-img2","timestamp":"2026-05-28T10:00:00Z","cli_version":"0.135.0"}}"#,
            r#"{"timestamp":"2026-05-28T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-28T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"image_generation","call_id":"call_img2","arguments":"{\"prompt\":\"a mountain lake\",\"size\":\"1024x1024\"}"}}"#,
            r#"{"timestamp":"2026-05-28T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_img2","output":[{"type":"image_url","image_url":{"url":"data:image/png;base64,xyz"}}]}}"#,
            r#"{"timestamp":"2026-05-28T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748426404.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);

        let tool = &turns[0].tool_calls[0];
        assert_ne!(
            tool.kind,
            ToolKind::Unknown,
            "image_generation must not be Unknown"
        );
        assert_eq!(tool.kind, ToolKind::ImageGeneration);
        assert_eq!(tool.image_prompt.as_deref(), Some("a mountain lake"));
    }

    // Codex v0.136.0 (PR #24962): shell hook output events with tightened schemas.
    //
    // PR #24962 enforced a strict contract on hook output event payloads. Previously the
    // payload could carry nullable extra fields (e.g. `metadata`, `stderr`); the new schema
    // removes those fields entirely (absent, not null). codex-trace must parse these events
    // as ShellHook ToolCall entries and must not panic when the previously-nullable fields
    // are absent.

    #[test]
    fn v0136_shell_hook_output_pre_exec_parsed_as_shell_hook_tool_call() {
        let entries = entries(&[
            r#"{"timestamp":"2026-06-01T10:00:00Z","type":"session_meta","payload":{"id":"v0136-hook","timestamp":"2026-06-01T10:00:00Z","cli_version":"0.136.0"}}"#,
            r#"{"timestamp":"2026-06-01T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-01T10:00:02Z","type":"event_msg","payload":{"type":"shell_hook_output","call_id":"hook-abc","hook_type":"pre_exec","stdout":"pre-hook ran\n","exit_code":0,"duration":{"secs":0,"nanos":4000000}}}"#,
            r#"{"timestamp":"2026-06-01T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748779203.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tc = &turns[0].tool_calls[0];
        assert_eq!(tc.kind, ToolKind::ShellHook);
        assert_eq!(tc.call_id, "hook-abc");
        assert_eq!(tc.name, "pre_exec");
        assert_eq!(tc.output.as_deref(), Some("pre-hook ran\n"));
        assert_eq!(tc.exit_code, Some(0));
        assert_eq!(tc.status, "completed");
        assert!(tc.duration_secs.is_some());
    }

    #[test]
    fn v0136_shell_hook_output_post_exec_failed_is_status_failed() {
        let entries = entries(&[
            r#"{"timestamp":"2026-06-01T10:01:00Z","type":"session_meta","payload":{"id":"v0136-hook-fail","timestamp":"2026-06-01T10:01:00Z","cli_version":"0.136.0"}}"#,
            r#"{"timestamp":"2026-06-01T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-01T10:01:02Z","type":"event_msg","payload":{"type":"shell_hook_output","call_id":"hook-fail-1","hook_type":"post_exec","stdout":"hook error output\n","exit_code":1,"duration":{"secs":0,"nanos":2000000}}}"#,
            r#"{"timestamp":"2026-06-01T10:01:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748779263.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tc = &turns[0].tool_calls[0];
        assert_eq!(tc.kind, ToolKind::ShellHook);
        assert_eq!(tc.name, "post_exec");
        assert_eq!(tc.exit_code, Some(1));
        assert_eq!(tc.status, "failed");
    }

    #[test]
    fn v0136_shell_hook_output_absent_nullable_fields_does_not_panic() {
        // v0.136.0 tightening: metadata and stderr are absent (not null). Verify the parser
        // handles the strictly-shaped payload without panicking or producing garbage data.
        let entries = entries(&[
            r#"{"timestamp":"2026-06-01T10:02:00Z","type":"session_meta","payload":{"id":"v0136-hook-tight","timestamp":"2026-06-01T10:02:00Z","cli_version":"0.136.0"}}"#,
            r#"{"timestamp":"2026-06-01T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            // strict v0.136.0 schema: no metadata, no stderr, no duration
            r#"{"timestamp":"2026-06-01T10:02:02Z","type":"event_msg","payload":{"type":"shell_hook_output","call_id":"hook-tight","hook_type":"pre_mcp","stdout":"","exit_code":0}}"#,
            r#"{"timestamp":"2026-06-01T10:02:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748779323.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tc = &turns[0].tool_calls[0];
        assert_eq!(tc.kind, ToolKind::ShellHook);
        assert_eq!(tc.name, "pre_mcp");
        assert!(tc.output.is_none()); // empty stdout → None
        assert!(tc.duration_secs.is_none());
        assert_eq!(tc.status, "completed");
    }

    #[test]
    fn v0136_all_standard_entry_types_parse_correctly_with_shell_hook() {
        // Regression guard: all four standard JSONL entry types plus shell_hook_output must
        // parse correctly for a v0.136.0 session.
        let lines = [
            r#"{"timestamp":"2026-06-01T10:03:00Z","type":"session_meta","payload":{"id":"v0136-session","timestamp":"2026-06-01T10:03:00Z","cwd":"/project","cli_version":"0.136.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-01T10:03:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-01T10:03:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
            r#"{"timestamp":"2026-06-01T10:03:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project"}}"#,
            r#"{"timestamp":"2026-06-01T10:03:04Z","type":"event_msg","payload":{"type":"shell_hook_output","call_id":"hook-v0136","hook_type":"pre_exec","stdout":"hook ok\n","exit_code":0,"duration":{"secs":0,"nanos":3000000}}}"#,
            r#"{"timestamp":"2026-06-01T10:03:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748779385.0}}"#,
        ];
        let expected_entry_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "turn_context",
            "event_msg",
            "event_msg",
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        for (entry, expected) in parsed.iter().zip(expected_entry_types.iter()) {
            assert_eq!(
                entry.entry_type, *expected,
                "wrong type for: {}",
                entry.entry_type
            );
        }

        let turns = build_turns(&parsed);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Complete);
        assert_eq!(turns[0].tool_calls.len(), 1);
        assert_eq!(turns[0].tool_calls[0].kind, ToolKind::ShellHook);
    }

    // Codex v0.150.0 (PR #40511): new `Interrupt` HookEventName variant fires for an active
    // top-level turn just before its interrupted-abort event (turn_aborted), flushing the
    // turn transcript first. The hook's shell_hook_output event must still be attached to the
    // turn's tool_calls even though the turn ends as Aborted rather than Complete.

    #[test]
    fn v0150_interrupt_hook_flushes_transcript_before_turn_aborted() {
        let entries = entries(&[
            r#"{"timestamp":"2026-08-01T10:00:00Z","type":"session_meta","payload":{"id":"v0150-interrupt","timestamp":"2026-08-01T10:00:00Z","cli_version":"0.150.0"}}"#,
            r#"{"timestamp":"2026-08-01T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-08-01T10:00:02Z","type":"event_msg","payload":{"type":"shell_hook_output","call_id":"hook-interrupt-1","hook_type":"interrupt","stdout":"turn interrupted\n","exit_code":0}}"#,
            r#"{"timestamp":"2026-08-01T10:00:03Z","type":"event_msg","payload":{"type":"turn_aborted","turn_id":"turn-1","reason":"user_interrupt"}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Aborted);
        assert_eq!(turns[0].aborted_reason.as_deref(), Some("user_interrupt"));
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tc = &turns[0].tool_calls[0];
        assert_eq!(tc.kind, ToolKind::ShellHook);
        assert_eq!(tc.name, "interrupt");
        assert_eq!(tc.output.as_deref(), Some("turn interrupted\n"));
    }

    // Codex v0.132.0 (PR #23123): `codex exec resume --output-schema` produces structured
    // JSON output items. The final model response is emitted as a "structured_output"
    // response_item with a JSON object in `content`. codex-trace must capture this as the
    // turn's final_answer so exec sessions with --output-schema display correctly.

    #[test]
    fn v0132_structured_output_response_item_sets_final_answer() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"v0132-schema","timestamp":"2026-05-20T10:00:00Z","cwd":"/tmp","cli_version":"0.132.0"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"response_item","payload":{"type":"structured_output","content":{"result":"success","count":42}}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748606403.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        let turn = &turns[0];
        assert!(
            turn.final_answer.is_some(),
            "structured_output response_item must populate final_answer"
        );
        let answer = turn.final_answer.as_ref().unwrap();
        let v: serde_json::Value =
            serde_json::from_str(answer).expect("final_answer must be valid JSON");
        assert_eq!(v["result"], "success");
        assert_eq!(v["count"], 42);
    }

    #[test]
    fn v0132_structured_output_with_string_content_sets_final_answer() {
        // structured_output may carry a plain string in the content field rather than a
        // JSON object when the schema resolves to a primitive type.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"v0132-str-output","timestamp":"2026-05-20T10:00:00Z","cwd":"/tmp","cli_version":"0.132.0"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"response_item","payload":{"type":"structured_output","content":"schema-validated plain text"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748606403.0}}"#,
        ]);
        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].final_answer.as_deref(),
            Some("schema-validated plain text"),
            "string-typed structured_output content must be stored verbatim"
        );
    }

    #[test]
    fn v0132_structured_output_does_not_overwrite_existing_final_answer() {
        // task_complete.last_agent_message takes precedence; a structured_output response_item
        // that arrives before task_complete must not clobber the answer set by agent_message.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"v0132-priority","timestamp":"2026-05-20T10:00:00Z","cwd":"/tmp","cli_version":"0.132.0"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"event_msg","payload":{"type":"agent_message","message":"prior final answer","phase":"final_answer"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"response_item","payload":{"type":"structured_output","content":{"should":"not overwrite"}}}"#,
            r#"{"timestamp":"2026-05-20T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748606404.0}}"#,
        ]);
        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].final_answer.as_deref(),
            Some("prior final answer"),
            "structured_output must not overwrite an already-set final_answer"
        );
    }

    #[test]
    fn v0132_structured_output_output_field_fallback_sets_final_answer() {
        // Some structured_output items may use `output` instead of `content`.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"v0132-output-field","timestamp":"2026-05-20T10:00:00Z","cwd":"/tmp","cli_version":"0.132.0"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"response_item","payload":{"type":"structured_output","output":{"status":"ok","value":99}}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748606403.0}}"#,
        ]);
        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        let answer = turns[0]
            .final_answer
            .as_deref()
            .expect("final_answer must be set from output field of structured_output");
        assert!(answer.contains("ok"));
        assert!(answer.contains("99"));
    }

    #[test]
    fn v0132_message_response_item_with_json_object_content_sets_final_answer() {
        // Codex v0.132.0+ (PR #23123): `--output-schema` sessions may emit the structured
        // response as a "message" response_item where `content` is a JSON object rather than
        // a plain string. This must populate final_answer so the turn is not left empty.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"v0132-msg-schema","timestamp":"2026-05-20T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":{"status":"done","items":["a","b"]}}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748606403.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        let turn = &turns[0];
        assert!(
            turn.final_answer.is_some(),
            "message response_item with JSON content must populate final_answer"
        );
        let answer = turn.final_answer.as_ref().unwrap();
        let v: serde_json::Value =
            serde_json::from_str(answer).expect("final_answer must be valid JSON");
        assert_eq!(v["status"], "done");
        assert_eq!(v["items"][0], "a");
    }

    #[test]
    fn v0132_message_response_item_with_plain_string_sets_final_answer() {
        // Plain-text assistant message response_items (common in exec sessions) must
        // also populate final_answer.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"v0132-msg-plain","timestamp":"2026-05-20T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Task completed successfully."}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748606403.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].final_answer.as_deref(),
            Some("Task completed successfully.")
        );
    }

    #[test]
    fn v0132_task_complete_last_agent_message_overrides_message_item_final_answer() {
        // task_complete.last_agent_message always overwrites final_answer (fires last in
        // the stream). This test ensures the priority order is preserved.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"v0132-override","timestamp":"2026-05-20T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"preliminary answer"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748606403.0,"last_agent_message":"definitive answer from task_complete"}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].final_answer.as_deref(),
            Some("definitive answer from task_complete")
        );
    }

    #[test]
    fn v0132_user_message_response_item_does_not_set_final_answer() {
        // Only "assistant" role message response_items should populate final_answer;
        // user-role items must not.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"v0132-user-msg","timestamp":"2026-05-20T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"response_item","payload":{"type":"message","role":"user","content":"user request here"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748606403.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        assert!(
            turns[0].final_answer.is_none(),
            "user-role message must not set final_answer"
        );
    }

    #[test]
    fn v0132_all_standard_entry_types_plus_structured_output_parse_correctly() {
        // Regression guard: a v0.132.0 --output-schema session must parse all four
        // standard entry types plus the structured_output response_item without error,
        // and the turn must reach Complete status with final_answer populated.
        let entries = entries(&[
            r#"{"timestamp":"2026-05-20T10:00:00Z","type":"session_meta","payload":{"id":"v0132-schema-full","timestamp":"2026-05-20T10:00:00Z","cwd":"/tmp","cli_version":"0.132.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:02Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/tmp"}}"#,
            r#"{"timestamp":"2026-05-20T10:00:03Z","type":"response_item","payload":{"type":"structured_output","content":{"status":"ok","value":99}}}"#,
            r#"{"timestamp":"2026-05-20T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748606404.0}}"#,
        ]);

        let turns = build_turns(&entries);
        assert_eq!(turns.len(), 1);
        let turn = &turns[0];
        assert_eq!(turn.status, TurnStatus::Complete);
        assert_eq!(turn.model.as_deref(), Some("gpt-5"));
        let answer = turn
            .final_answer
            .as_ref()
            .expect("structured_output must set final_answer");
        let v: serde_json::Value = serde_json::from_str(answer).expect("must be valid JSON");
        assert_eq!(v["status"], "ok");
        assert_eq!(v["value"], 99);
    }

    // Codex v0.139.0 (PRs #24118, #27084): function_call arguments may arrive as a JSON
    // object rather than a stringified-JSON string. Verify the parser handles both.

    #[test]
    fn v0139_function_call_with_object_arguments_preserves_arguments() {
        let lines = [
            r#"{"timestamp":"2026-06-09T10:00:00Z","type":"session_meta","payload":{"id":"v0139-obj-args","timestamp":"2026-06-09T10:00:00Z","cwd":"/project","cli_version":"0.139.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-09T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-09T10:00:02Z","type":"response_item","payload":{"type":"function_call","call_id":"call-obj","name":"exec_command","arguments":{"cmd":"echo hello","workdir":"/tmp"}}}"#,
            r#"{"timestamp":"2026-06-09T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-obj","output":"hello\n"}}"#,
            r#"{"timestamp":"2026-06-09T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1749466804.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.name, "exec_command");
        assert!(
            !tool.arguments.is_null(),
            "JSON-object arguments must not be silently dropped"
        );
        assert_eq!(
            tool.arguments.get("cmd").and_then(|v| v.as_str()),
            Some("echo hello"),
        );
        assert_eq!(tool.output.as_deref(), Some("hello\n"));
    }

    #[test]
    fn v0139_function_call_with_oneof_allof_arguments_parses_without_error() {
        let lines = [
            r#"{"timestamp":"2026-06-09T10:01:00Z","type":"session_meta","payload":{"id":"v0139-oneof","timestamp":"2026-06-09T10:01:00Z","cwd":"/project","cli_version":"0.139.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-09T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-09T10:01:02Z","type":"response_item","payload":{"type":"function_call","call_id":"call-oneof","name":"submit_form","arguments":{"field":{"type":"text","value":"hello"},"options":{"oneOf":[{"label":"A","val":1},{"label":"B","val":2}]}}}}"#,
            r#"{"timestamp":"2026-06-09T10:01:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-oneof","output":"submitted"}}"#,
            r#"{"timestamp":"2026-06-09T10:01:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1749466864.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert!(!tool.arguments.is_null());
        assert!(tool.arguments.get("options").is_some());
        assert_eq!(tool.output.as_deref(), Some("submitted"));
    }

    // --- Codex v0.141.0+ dynamic tool namespace tests (PRs #27365, #27371) ---

    #[test]
    fn parse_dynamic_tools_with_explicit_namespace_field() {
        use super::parse_dynamic_tools;
        use serde_json::json;
        let payload = json!({
            "type": "task_started",
            "turn_id": "turn-1",
            "dynamic_tools": [
                {"name": "my_tool", "namespace": "mcp:my-server"},
                {"name": "other_tool", "namespace": "connector:plugin-1"}
            ]
        });
        let registry = parse_dynamic_tools(&payload);
        assert_eq!(
            registry.get("my_tool"),
            Some(&("mcp".to_string(), "my-server".to_string()))
        );
        assert_eq!(
            registry.get("other_tool"),
            Some(&("connector".to_string(), "plugin-1".to_string()))
        );
    }

    #[test]
    fn parse_dynamic_tools_with_qualified_name_in_name_field() {
        use super::parse_dynamic_tools;
        use serde_json::json;
        let payload = json!({
            "type": "task_started",
            "turn_id": "turn-1",
            "dynamic_tools": [
                {"name": "mcp:my-server/my_tool"}
            ]
        });
        let registry = parse_dynamic_tools(&payload);
        // Key is the unqualified tool name extracted from the qualified name
        assert_eq!(
            registry.get("my_tool"),
            Some(&("mcp".to_string(), "my-server".to_string()))
        );
    }

    #[test]
    fn parse_dynamic_tools_absent_returns_empty() {
        use super::parse_dynamic_tools;
        use serde_json::json;
        let payload = json!({"type": "task_started", "turn_id": "turn-1"});
        assert!(parse_dynamic_tools(&payload).is_empty());
    }

    #[test]
    fn v0141_task_started_dynamic_tools_classifies_mcp_tool_via_registry() {
        // v0.141.0+: task_started has dynamic_tools; function_call has unqualified name.
        // Registry lookup must classify the tool as McpTool.
        let lines = [
            r#"{"timestamp":"2026-06-18T10:00:00Z","type":"session_meta","payload":{"session_id":"v0141-dyn","timestamp":"2026-06-18T10:00:00Z","cwd":"/project","cli_version":"0.141.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-18T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","dynamic_tools":[{"name":"my_tool","namespace":"mcp:my-server"}]}}"#,
            r#"{"timestamp":"2026-06-18T10:00:02Z","type":"response_item","payload":{"type":"function_call","call_id":"call-dyn-1","name":"my_tool","arguments":"{}"}}"#,
            r#"{"timestamp":"2026-06-18T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-dyn-1","output":"ok"}}"#,
            r#"{"timestamp":"2026-06-18T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1750240804.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(
            tool.kind,
            crate::parser::toolcall::ToolKind::McpTool,
            "tool should be classified as McpTool via dynamic_tools registry"
        );
        assert_eq!(tool.mcp_server.as_deref(), Some("my-server"));
        assert_eq!(tool.mcp_tool.as_deref(), Some("my_tool"));
    }

    #[test]
    fn v0141_task_started_dynamic_tools_classifies_mcp_tool_via_qualified_name() {
        // v0.141.0+: function_call name is "mcp:server/tool" (namespace-qualified).
        let lines = [
            r#"{"timestamp":"2026-06-18T10:01:00Z","type":"session_meta","payload":{"session_id":"v0141-qual","timestamp":"2026-06-18T10:01:00Z","cwd":"/project","cli_version":"0.141.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-18T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2"}}"#,
            r#"{"timestamp":"2026-06-18T10:01:02Z","type":"response_item","payload":{"type":"function_call","call_id":"call-qual-1","name":"mcp:my-server/my_tool","arguments":"{}"}}"#,
            r#"{"timestamp":"2026-06-18T10:01:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-qual-1","output":"done"}}"#,
            r#"{"timestamp":"2026-06-18T10:01:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-2","completed_at":1750240864.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(
            tool.kind,
            crate::parser::toolcall::ToolKind::McpTool,
            "namespace-qualified tool name must be classified as McpTool"
        );
        assert_eq!(tool.mcp_server.as_deref(), Some("my-server"));
        assert_eq!(tool.mcp_tool.as_deref(), Some("my_tool"));
    }

    // Codex v0.141.0 (PR #28355): ResponseItem gains a new optional top-level `metadata` field.

    #[test]
    fn v0141_response_item_with_metadata_does_not_affect_turn_building() {
        let lines = [
            r#"{"timestamp":"2026-06-18T10:00:00Z","type":"session_meta","payload":{"id":"v0141-turn-meta","timestamp":"2026-06-18T10:00:00Z","cwd":"/project","cli_version":"0.141.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-18T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-18T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call-v141","arguments":"{\"cmd\":\"echo hi\"}","metadata":{"priority":"normal","request_id":"req-v141"}}}"#,
            r#"{"timestamp":"2026-06-18T10:00:03Z","type":"event_msg","payload":{"type":"exec_command_end","call_id":"call-v141","aggregated_output":"hi\n","exit_code":0,"status":"completed","duration":{"secs":0,"nanos":10000000}}}"#,
            r#"{"timestamp":"2026-06-18T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1750240804.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.output.as_deref(), Some("hi\n"));
        assert_eq!(tool.exit_code, Some(0));
    }

    #[test]
    fn v0141_message_response_item_with_metadata_populates_final_answer() {
        let lines = [
            r#"{"timestamp":"2026-06-18T10:01:00Z","type":"session_meta","payload":{"id":"v0141-msg-meta","timestamp":"2026-06-18T10:01:00Z","cwd":"/project","cli_version":"0.141.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-18T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2"}}"#,
            r#"{"timestamp":"2026-06-18T10:01:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Done!","metadata":{"server_key":"srv-v141","usage":{"prompt_tokens":10,"completion_tokens":5}}}}"#,
            r#"{"timestamp":"2026-06-18T10:01:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-2","completed_at":1750240863.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].final_answer.as_deref(),
            Some("Done!"),
            "message with metadata must populate final_answer"
        );
    }

    // Codex v0.142.0 (PR #28968): `metadata` on chat message response_items renamed to
    // `internal_chat_message_metadata_passthrough`. Turn building must be unaffected (content
    // is still read from the `content` field), and sessions using the new field name must parse.

    #[test]
    fn v0142_message_response_item_with_internal_passthrough_populates_final_answer() {
        let lines = [
            r#"{"timestamp":"2026-06-22T10:01:00Z","type":"session_meta","payload":{"id":"v0142-msg-meta","timestamp":"2026-06-22T10:01:00Z","cwd":"/project","cli_version":"0.142.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-22T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-22T10:01:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Done v0142!","internal_chat_message_metadata_passthrough":{"server_key":"srv-v142","usage":{"prompt_tokens":10,"completion_tokens":5}}}}"#,
            r#"{"timestamp":"2026-06-22T10:01:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1750327263.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].final_answer.as_deref(),
            Some("Done v0142!"),
            "message with internal_chat_message_metadata_passthrough must still populate final_answer"
        );
    }

    #[test]
    fn v0142_function_call_with_internal_passthrough_does_not_affect_turn_building() {
        let lines = [
            r#"{"timestamp":"2026-06-22T10:02:00Z","type":"session_meta","payload":{"id":"v0142-fc-meta","timestamp":"2026-06-22T10:02:00Z","cwd":"/project","cli_version":"0.142.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-22T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2"}}"#,
            r#"{"timestamp":"2026-06-22T10:02:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call-v142","arguments":"{\"cmd\":\"echo hi\"}","internal_chat_message_metadata_passthrough":{"priority":"normal","request_id":"req-v142"}}}"#,
            r#"{"timestamp":"2026-06-22T10:02:03Z","type":"event_msg","payload":{"type":"exec_command_end","call_id":"call-v142","aggregated_output":"hi\n","exit_code":0,"status":"completed","duration":{"secs":0,"nanos":10000000}}}"#,
            r#"{"timestamp":"2026-06-22T10:02:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-2","completed_at":1750327324.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.output.as_deref(), Some("hi\n"));
        assert_eq!(tool.exit_code, Some(0));
    }

    // Codex v0.140.0 (PRs #27070, #27071, #27703): /import command adds external-agent
    // context import event_msg entries. Codex v0.141.0 (PR #28008):
    // external_agent_import_result accounting response_items may appear inside turns.

    #[test]
    fn v0140_import_event_before_first_turn_does_not_corrupt_turns() {
        let lines = [
            r#"{"timestamp":"2026-06-15T10:00:00Z","type":"session_meta","payload":{"id":"v0140-pre-turn-import","timestamp":"2026-06-15T10:00:00Z","cwd":"/project","cli_version":"0.140.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-15T10:00:01Z","type":"event_msg","payload":{"type":"agent_context_imported","source":"claude-code","thread_count":2}}"#,
            r#"{"timestamp":"2026-06-15T10:00:02Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-15T10:00:03Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Imported context loaded, continuing session."}}"#,
            r#"{"timestamp":"2026-06-15T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1750000004.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(
            turns.len(),
            1,
            "import event must not create spurious turns"
        );
        assert_eq!(turns[0].turn_id, "turn-1");
        assert_eq!(turns[0].status, super::TurnStatus::Complete);
    }

    #[test]
    fn v0141_external_agent_import_result_inside_turn_does_not_corrupt_turn() {
        let lines = [
            r#"{"timestamp":"2026-06-15T10:00:00Z","type":"session_meta","payload":{"id":"v0141-import-result","timestamp":"2026-06-15T10:00:00Z","cwd":"/project","cli_version":"0.141.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-15T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-15T10:00:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"echo hi\"}","call_id":"call-1"}}"#,
            r#"{"timestamp":"2026-06-15T10:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-1","output":"hi\n"}}"#,
            r#"{"timestamp":"2026-06-15T10:00:04Z","type":"response_item","payload":{"type":"external_agent_import_result","source":"claude-code","total_tokens":8200}}"#,
            r#"{"timestamp":"2026-06-15T10:00:05Z","type":"event_msg","payload":{"type":"agent_message","message":"Done."}}"#,
            r#"{"timestamp":"2026-06-15T10:00:06Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1750000006.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(turns.len(), 1);
        let turn = &turns[0];
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].name, "exec_command");
        assert_eq!(turn.tool_calls[0].output.as_deref(), Some("hi\n"));
        assert_eq!(turn.agent_messages.len(), 1);
        assert_eq!(turn.agent_messages[0].text, "Done.");
        assert_eq!(turn.status, super::TurnStatus::Complete);
    }

    // Codex v0.142.0 (PR #28368): multi-agent v2 inter-agent messages now use typed
    // envelopes instead of plain string payloads. The `message` field in `agent_message`
    // events is now an object `{"type": "<kind>", "content": "..."}` rather than a raw
    // string. The parser must read the type discriminant and dispatch to the correct decoder.

    #[test]
    fn v0142_agent_message_with_typed_text_envelope_extracts_content() {
        // v0.142.0 typed envelope: {"type": "text", "content": "..."}.
        // The parser must read the type discriminant and return the content string.
        let lines = [
            r#"{"timestamp":"2026-06-22T10:00:00Z","type":"session_meta","payload":{"id":"v0142-session","timestamp":"2026-06-22T10:00:00Z","cwd":"/project","cli_version":"0.142.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:02Z","type":"event_msg","payload":{"type":"agent_message","message":{"type":"text","content":"Hello from the subagent."},"phase":"main"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1750593603.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(turns.len(), 1);
        let turn = &turns[0];
        assert_eq!(turn.agent_messages.len(), 1);
        assert_eq!(turn.agent_messages[0].text, "Hello from the subagent.");
        assert_eq!(turn.agent_messages[0].phase.as_deref(), Some("main"));
    }

    #[test]
    fn v0142_agent_message_plain_string_still_parses_correctly() {
        // Pre-v0.142.0 sessions with a plain-string `message` must continue to parse.
        // The backward-compatible path must not break when upgrading.
        let lines = [
            r#"{"timestamp":"2026-06-22T10:00:00Z","type":"session_meta","payload":{"id":"pre-v0142","timestamp":"2026-06-22T10:00:00Z","cwd":"/project","cli_version":"0.141.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:02Z","type":"event_msg","payload":{"type":"agent_message","message":"Plain text message.","phase":"main"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1750593603.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].agent_messages.len(), 1);
        assert_eq!(turns[0].agent_messages[0].text, "Plain text message.");
    }

    #[test]
    fn v0142_agent_message_typed_envelope_final_answer_phase_sets_final_answer() {
        // Typed envelope with phase "final_answer" must populate turn.final_answer.
        let lines = [
            r#"{"timestamp":"2026-06-22T10:00:00Z","type":"session_meta","payload":{"id":"v0142-final","timestamp":"2026-06-22T10:00:00Z","cwd":"/project","cli_version":"0.142.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:02Z","type":"event_msg","payload":{"type":"agent_message","message":{"type":"text","content":"The answer is 42."},"phase":"final_answer"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1750593603.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(turns.len(), 1);
        let turn = &turns[0];
        assert_eq!(turn.agent_messages.len(), 1);
        assert_eq!(turn.agent_messages[0].text, "The answer is 42.");
        assert_eq!(turn.final_answer.as_deref(), Some("The answer is 42."));
    }

    #[test]
    fn v0142_agent_message_unknown_envelope_type_falls_back_to_content_field() {
        // Unknown envelope types must fall back to the "content" field rather than silently
        // dropping the message. This ensures forward-compatibility with future envelope types.
        let lines = [
            r#"{"timestamp":"2026-06-22T10:00:00Z","type":"session_meta","payload":{"id":"v0142-unk","timestamp":"2026-06-22T10:00:00Z","cwd":"/project","cli_version":"0.142.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:02Z","type":"event_msg","payload":{"type":"agent_message","message":{"type":"rich_text","content":"Rendered output."},"phase":"commentary"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1750593603.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].agent_messages.len(), 1);
        assert_eq!(turns[0].agent_messages[0].text, "Rendered output.");
    }

    #[test]
    fn v0142_all_standard_entry_types_parse_correctly() {
        // Regression guard: all four standard JSONL entry types from a v0.142.0 session
        // must parse correctly. Includes a typed-envelope agent_message.
        let lines = [
            r#"{"timestamp":"2026-06-22T10:00:00Z","type":"session_meta","payload":{"id":"v0142-full","timestamp":"2026-06-22T10:00:00Z","cwd":"/project","cli_version":"0.142.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:04Z","type":"event_msg","payload":{"type":"agent_message","message":{"type":"text","content":"Processing your request."},"phase":"commentary"}}"#,
            r#"{"timestamp":"2026-06-22T10:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1750593605.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(turns.len(), 1);
        let turn = &turns[0];
        assert_eq!(turn.status, super::TurnStatus::Complete);
        assert_eq!(turn.agent_messages.len(), 1);
        assert_eq!(turn.agent_messages[0].text, "Processing your request.");
        assert_eq!(turn.model.as_deref(), Some("gpt-5"));
    }

    // Codex v0.142.2 (PR #28360): turn_id field added to ResponseItem metadata.

    #[test]
    fn v0142_response_item_with_metadata_turn_id_attributed_to_correct_turn() {
        let lines = [
            r#"{"timestamp":"2026-06-25T10:00:00Z","type":"session_meta","payload":{"id":"v0142-meta-tid","timestamp":"2026-06-25T10:00:00Z","cwd":"/project","cli_version":"0.142.2","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-25T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-25T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1751000002.0}}"#,
            r#"{"timestamp":"2026-06-25T10:00:03Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2"}}"#,
            r#"{"timestamp":"2026-06-25T10:00:04Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Answer from turn 1","metadata":{"turn_id":"turn-1"}}}"#,
            r#"{"timestamp":"2026-06-25T10:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-2","completed_at":1751000005.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(turns.len(), 2);
        let turn1 = turns.iter().find(|t| t.turn_id == "turn-1").unwrap();
        let turn2 = turns.iter().find(|t| t.turn_id == "turn-2").unwrap();
        assert_eq!(
            turn1.final_answer.as_deref(),
            Some("Answer from turn 1"),
            "metadata.turn_id must override positional current_turn_id"
        );
        assert_eq!(turn2.final_answer, None);
    }

    #[test]
    fn v0142_response_item_without_metadata_turn_id_falls_back_to_current_turn_id() {
        let lines = [
            r#"{"timestamp":"2026-06-25T10:00:00Z","type":"session_meta","payload":{"id":"v0142-no-meta-tid","timestamp":"2026-06-25T10:00:00Z","cwd":"/project","cli_version":"0.141.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-06-25T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-06-25T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello from legacy turn"}}"#,
            r#"{"timestamp":"2026-06-25T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1751000003.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].final_answer.as_deref(),
            Some("Hello from legacy turn")
        );
    }

    // Codex v0.140.0 (PR #27801): /realtime voice subsystem removed. Sessions recorded
    // before that may contain `speech_append`, `realtime_handoff`, `audio_transcript`
    // response_items. They carry no turn-building semantics and must be silently skipped.

    #[test]
    fn pre_v0140_realtime_voice_items_in_archive_session_are_silently_skipped() {
        let lines = [
            r#"{"timestamp":"2026-03-01T10:00:00Z","type":"session_meta","payload":{"id":"v0139-rt","timestamp":"2026-03-01T10:00:00Z","cwd":"/project","cli_version":"0.139.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-03-01T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-03-01T10:00:02Z","type":"response_item","payload":{"type":"speech_append","audio_b64":"AAAA"}}"#,
            r#"{"timestamp":"2026-03-01T10:00:03Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call-rt","arguments":"{\"cmd\":\"echo ok\"}"}}"#,
            r#"{"timestamp":"2026-03-01T10:00:04Z","type":"response_item","payload":{"type":"realtime_handoff","handoff_id":"h1"}}"#,
            r#"{"timestamp":"2026-03-01T10:00:05Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-rt","output":"ok\n"}}"#,
            r#"{"timestamp":"2026-03-01T10:00:06Z","type":"response_item","payload":{"type":"audio_transcript","transcript":"hello there"}}"#,
            r#"{"timestamp":"2026-03-01T10:00:07Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1740825607.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        // Exactly one turn — voice items must not create synthetic turns
        assert_eq!(turns.len(), 1);
        let turn = &turns[0];
        // The real exec_command tool call must survive intact
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].name, "exec_command");
        assert_eq!(turn.tool_calls[0].output.as_deref(), Some("ok\n"));
        assert_eq!(turn.status, super::TurnStatus::Complete);
    }

    // Codex v0.144.0 (PR #28772): MCP tools can now request interactive authentication by
    // default (no longer behind an experimental flag). Sessions that include MCP auth flows
    // emit mcp_auth_request and mcp_auth_result event_msg entries. These must be skipped
    // without corrupting turn data.

    #[test]
    fn v0144_mcp_auth_events_do_not_corrupt_turn_data() {
        let lines = [
            r#"{"timestamp":"2026-07-01T10:00:00Z","type":"session_meta","payload":{"id":"v0144-auth-turns","timestamp":"2026-07-01T10:00:00Z","cwd":"/project","cli_version":"0.144.0"}}"#,
            r#"{"timestamp":"2026-07-01T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-07-01T10:00:02Z","type":"event_msg","payload":{"type":"mcp_auth_request","server":"github","call_id":"auth-1","auth_url":"https://github.com/login/oauth/authorize?client_id=abc","instructions":"Visit the URL to grant access."}}"#,
            r#"{"timestamp":"2026-07-01T10:00:05Z","type":"event_msg","payload":{"type":"mcp_auth_result","server":"github","call_id":"auth-1","status":"authenticated"}}"#,
            r#"{"timestamp":"2026-07-01T10:00:06Z","type":"response_item","payload":{"type":"mcp_tool_call","call_id":"mcp-1","server":"github","tool":"get_repo","arguments":{"owner":"openai","repo":"codex"}}}"#,
            r#"{"timestamp":"2026-07-01T10:00:07Z","type":"response_item","payload":{"type":"mcp_tool_call_output","call_id":"mcp-1","output":"Repository info..."}}"#,
            r#"{"timestamp":"2026-07-01T10:00:08Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1751360408.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(
            turns.len(),
            1,
            "auth events must not create synthetic turns"
        );
        let turn = &turns[0];
        assert_eq!(
            turn.tool_calls.len(),
            1,
            "MCP tool call must survive auth events intact"
        );
        assert_eq!(turn.tool_calls[0].name, "get_repo");
        assert_eq!(
            turn.tool_calls[0].output.as_deref(),
            Some("Repository info...")
        );
        assert_eq!(turn.status, super::TurnStatus::Complete);
    }

    #[test]
    fn v0144_mcp_auth_request_failed_result_does_not_corrupt_turn_data() {
        let lines = [
            r#"{"timestamp":"2026-07-01T10:00:00Z","type":"session_meta","payload":{"id":"v0144-auth-fail","timestamp":"2026-07-01T10:00:00Z","cwd":"/project","cli_version":"0.144.0"}}"#,
            r#"{"timestamp":"2026-07-01T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-07-01T10:00:02Z","type":"event_msg","payload":{"type":"mcp_auth_request","server":"slack","call_id":"auth-2","auth_url":"https://slack.com/oauth/v2/authorize","instructions":"Authenticate with Slack."}}"#,
            r#"{"timestamp":"2026-07-01T10:00:03Z","type":"event_msg","payload":{"type":"mcp_auth_result","server":"slack","call_id":"auth-2","status":"failed","error":"User cancelled authentication"}}"#,
            r#"{"timestamp":"2026-07-01T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1751360404.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(
            turns.len(),
            1,
            "auth events must not create synthetic turns"
        );
        let turn = &turns[0];
        assert_eq!(turn.tool_calls.len(), 0);
        assert_eq!(turn.status, super::TurnStatus::Complete);
    }

    // Codex v0.144.0 (PR #28772): also covers the mcp_auth_challenge/mcp_auth_complete
    // naming variant. Both naming conventions must be recognised.

    #[test]
    fn v0144_mcp_auth_events_during_turn_do_not_corrupt_turn_state() {
        let lines = [
            r#"{"timestamp":"2026-07-09T10:00:00Z","type":"session_meta","payload":{"id":"v0144-auth-flow","timestamp":"2026-07-09T10:00:00Z","cwd":"/project","cli_version":"0.144.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-07-09T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-07-09T10:00:02Z","type":"response_item","payload":{"type":"mcp_tool_call","call_id":"mcp-1","server":"github","tool":"list_repos","arguments":{}}}"#,
            r#"{"timestamp":"2026-07-09T10:00:03Z","type":"event_msg","payload":{"type":"mcp_auth_challenge","server":"github","auth_url":"https://github.com/login/oauth/authorize?client_id=abc123","call_id":"mcp-1"}}"#,
            r#"{"timestamp":"2026-07-09T10:00:07Z","type":"event_msg","payload":{"type":"mcp_auth_complete","server":"github","call_id":"mcp-1","success":true}}"#,
            r#"{"timestamp":"2026-07-09T10:00:08Z","type":"response_item","payload":{"type":"mcp_tool_call_output","call_id":"mcp-1","output":[{"type":"text","text":"[\"my-repo\",\"other-repo\"]"}]}}"#,
            r#"{"timestamp":"2026-07-09T10:00:09Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1752055209.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(turns.len(), 1);
        let turn = &turns[0];
        assert_eq!(turn.turn_id, "turn-1");
        assert_eq!(turn.status, super::TurnStatus::Complete);
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].name, "list_repos");
        assert_eq!(turn.tool_calls[0].mcp_server.as_deref(), Some("github"));
    }

    #[test]
    fn v0144_mcp_auth_challenge_without_tool_call_does_not_crash() {
        let lines = [
            r#"{"timestamp":"2026-07-09T10:00:00Z","type":"session_meta","payload":{"id":"v0144-proactive-auth","timestamp":"2026-07-09T10:00:00Z","cwd":"/project","cli_version":"0.144.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-07-09T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-07-09T10:00:02Z","type":"event_msg","payload":{"type":"mcp_auth_challenge","server":"slack","auth_url":"https://slack.com/oauth/v2/authorize?client_id=xyz"}}"#,
            r#"{"timestamp":"2026-07-09T10:00:06Z","type":"event_msg","payload":{"type":"mcp_auth_complete","server":"slack","success":true}}"#,
            r#"{"timestamp":"2026-07-09T10:00:07Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Authenticated with Slack."}}"#,
            r#"{"timestamp":"2026-07-09T10:00:08Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1752055208.0}}"#,
        ];
        let parsed: Vec<_> = lines
            .iter()
            .filter_map(|line| crate::parser::entry::RawEntry::parse(line))
            .collect();
        let turns = build_turns(&parsed);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, super::TurnStatus::Complete);
        assert_eq!(turns[0].tool_calls.len(), 0);
    }

    #[test]
    fn v0144_mcp_auth_events_interleaved_with_agent_message_do_not_corrupt_turn() {
        let entries = entries(&[
            r#"{"timestamp":"2026-07-01T10:02:00Z","type":"session_meta","payload":{"id":"v0144-mcp-auth-msg","timestamp":"2026-07-01T10:02:00Z","cwd":"/project","cli_version":"0.144.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-07-01T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-07-01T10:02:02Z","type":"event_msg","payload":{"type":"mcp_auth_request","server":"github","call_id":"auth-3","auth_url":"https://github.com/login/oauth/authorize?client_id=abc","instructions":"Visit the URL."}}"#,
            r#"{"timestamp":"2026-07-01T10:02:03Z","type":"event_msg","payload":{"type":"agent_message","message":"Waiting for GitHub authentication...","phase":"main"}}"#,
            r#"{"timestamp":"2026-07-01T10:02:04Z","type":"event_msg","payload":{"type":"mcp_auth_result","server":"github","call_id":"auth-3","status":"authenticated"}}"#,
            r#"{"timestamp":"2026-07-01T10:02:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1751364125.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Complete);
        // Auth events must not appear as agent messages — only the real agent_message survives
        assert_eq!(turns[0].agent_messages.len(), 1);
        assert_eq!(
            turns[0].agent_messages[0].text,
            "Waiting for GitHub authentication..."
        );
        assert!(turns[0].tool_calls.is_empty());
    }

    #[test]
    fn v0144_multiple_mcp_auth_attempts_are_all_skipped_gracefully() {
        let entries = entries(&[
            r#"{"timestamp":"2026-07-01T10:03:00Z","type":"session_meta","payload":{"id":"v0144-mcp-auth-retry","timestamp":"2026-07-01T10:03:00Z","cwd":"/project","cli_version":"0.144.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-07-01T10:03:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-07-01T10:03:02Z","type":"event_msg","payload":{"type":"mcp_auth_request","server":"jira","call_id":"auth-4","auth_url":"https://auth.jira.com/oauth","instructions":"Auth with Jira."}}"#,
            r#"{"timestamp":"2026-07-01T10:03:03Z","type":"event_msg","payload":{"type":"mcp_auth_result","server":"jira","call_id":"auth-4","status":"failed","error":"timeout"}}"#,
            r#"{"timestamp":"2026-07-01T10:03:04Z","type":"event_msg","payload":{"type":"mcp_auth_request","server":"jira","call_id":"auth-5","auth_url":"https://auth.jira.com/oauth","instructions":"Auth with Jira."}}"#,
            r#"{"timestamp":"2026-07-01T10:03:05Z","type":"event_msg","payload":{"type":"mcp_auth_result","server":"jira","call_id":"auth-5","status":"authenticated"}}"#,
            r#"{"timestamp":"2026-07-01T10:03:06Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1751364186.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Complete);
        // Multiple auth request/result pairs must all be silently skipped
        assert!(turns[0].agent_messages.is_empty());
        assert!(turns[0].tool_calls.is_empty());
    }

    // Codex v0.145.0: contextual branch fix — attachments and mention bindings are now
    // preserved when a message is edited and retried, rather than being dropped on the
    // retried branch. The user_message event for the new branch now carries the same
    // attachment/mention data as the original. codex-trace's generic field extraction
    // already tolerates these extra fields; these tests confirm no regression: preserved
    // attachment/mention data on a retried turn does not crash the parser, and the
    // turn-branching logic correctly attributes each user_message to its own turn.

    #[test]
    fn v0145_retried_branch_with_preserved_attachment_is_parsed_correctly() {
        let entries = entries(&[
            r#"{"timestamp":"2026-07-26T10:00:00Z","type":"session_meta","payload":{"id":"v0145-ctx-branch-attach","timestamp":"2026-07-26T10:00:00Z","cwd":"/project","cli_version":"0.145.0","model_provider":"openai"}}"#,
            // Original turn: user message includes an image attachment.
            r#"{"timestamp":"2026-07-26T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-07-26T10:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"Analyze this screenshot","attachments":[{"type":"image","path":"/home/user/.codex/images/screen.png"}],"mention_bindings":[]}}"#,
            r#"{"timestamp":"2026-07-26T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1785060003.0}}"#,
            // Retried branch: user edited the message; v0.145.0 preserves the image
            // attachment from the original turn in this branch's user_message.
            r#"{"timestamp":"2026-07-26T10:00:04Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2","forked_from_thread_id":"turn-1-thread"}}"#,
            r#"{"timestamp":"2026-07-26T10:00:05Z","type":"event_msg","payload":{"type":"user_message","message":"Analyze this updated screenshot","attachments":[{"type":"image","path":"/home/user/.codex/images/screen.png"}],"mention_bindings":[]}}"#,
            r#"{"timestamp":"2026-07-26T10:00:06Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"I can see the screenshot."}}"#,
            r#"{"timestamp":"2026-07-26T10:00:07Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-2","completed_at":1785060007.0}}"#,
        ]);

        let turns = build_turns(&entries);

        // Both turns must be parsed — the retried branch does not replace the original.
        assert_eq!(turns.len(), 2);

        // Original turn retains its own user message.
        assert_eq!(turns[0].turn_id, "turn-1");
        assert_eq!(turns[0].status, TurnStatus::Complete);
        assert_eq!(
            turns[0].user_message.as_deref(),
            Some("Analyze this screenshot"),
            "original turn user_message must not be overwritten by retried branch"
        );
        assert!(
            turns[0].forked_from_thread_id.is_none(),
            "original turn must have no forked_from_thread_id"
        );

        // Retried branch captures its own edited message and fork origin.
        assert_eq!(turns[1].turn_id, "turn-2");
        assert_eq!(turns[1].status, TurnStatus::Complete);
        assert_eq!(
            turns[1].user_message.as_deref(),
            Some("Analyze this updated screenshot"),
            "retried branch must carry its own edited user_message text"
        );
        assert_eq!(
            turns[1].forked_from_thread_id.as_deref(),
            Some("turn-1-thread"),
            "retried branch must record its fork origin"
        );
    }

    #[test]
    fn v0145_retried_branch_with_preserved_mention_binding_is_parsed_correctly() {
        // Edge case: the retried branch preserves a mention_binding (e.g. @shell) that was
        // originally lost before the v0.145.0 contextual branch fix.
        let entries = entries(&[
            r#"{"timestamp":"2026-07-26T11:00:00Z","type":"session_meta","payload":{"id":"v0145-ctx-branch-mention","timestamp":"2026-07-26T11:00:00Z","cwd":"/project","cli_version":"0.145.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-07-26T11:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-07-26T11:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"Fix the bug using @shell","mention_bindings":[]}}"#,
            r#"{"timestamp":"2026-07-26T11:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1785063603.0}}"#,
            // Retried branch: mention_binding now preserved by v0.145.0.
            r#"{"timestamp":"2026-07-26T11:00:04Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2","forked_from_thread_id":"turn-1-thread"}}"#,
            r#"{"timestamp":"2026-07-26T11:00:05Z","type":"event_msg","payload":{"type":"user_message","message":"Fix the bug using @shell","mention_bindings":[{"mention":"@shell","binding":"shell"}]}}"#,
            r#"{"timestamp":"2026-07-26T11:00:06Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-2","completed_at":1785063606.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 2);
        // Retried branch correctly receives its own user_message (not a stale None from
        // turn-1 — the mention_bindings field in the payload must not confuse the parser).
        assert_eq!(
            turns[1].user_message.as_deref(),
            Some("Fix the bug using @shell"),
            "retried branch user_message must be correctly attributed despite mention_bindings"
        );
        assert_eq!(
            turns[1].forked_from_thread_id.as_deref(),
            Some("turn-1-thread")
        );
    }

    // Codex v0.147.0 (PR #36356, issue #221): syncing further edits into an already-imported
    // Claude/Cursor conversation appends new turns after the existing `<EXTERNAL SESSION
    // IMPORTED>` marker rather than creating a duplicate thread. The marker itself predates
    // v0.147.0 (external-agent import, v0.140.0) but previously only ever trailed a finished
    // import; it can now sit mid-transcript, so it must never surface as a real message.

    #[test]
    fn v0147_external_session_imported_marker_is_not_rendered_as_agent_message() {
        let entries = entries(&[
            r#"{"timestamp":"2026-08-05T10:00:00Z","type":"session_meta","payload":{"id":"v0147-import-marker","timestamp":"2026-08-05T10:00:00Z","cwd":"/project","cli_version":"0.147.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-08-05T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"external-import-turn-1"}}"#,
            r#"{"timestamp":"2026-08-05T10:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"first imported request"}}"#,
            r#"{"timestamp":"2026-08-05T10:00:03Z","type":"event_msg","payload":{"type":"agent_message","message":"first imported answer"}}"#,
            r#"{"timestamp":"2026-08-05T10:00:04Z","type":"event_msg","payload":{"type":"agent_message","message":"<EXTERNAL SESSION IMPORTED>"}}"#,
            r#"{"timestamp":"2026-08-05T10:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"external-import-turn-1","completed_at":1785926405.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0]
                .agent_messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            vec!["first imported answer"],
            "the <EXTERNAL SESSION IMPORTED> marker must not appear as a rendered message"
        );
        assert_eq!(turns[0].final_answer, None);
    }

    #[test]
    fn v0147_synced_turn_after_import_marker_parses_as_its_own_turn() {
        let entries = entries(&[
            r#"{"timestamp":"2026-08-05T11:00:00Z","type":"session_meta","payload":{"id":"v0147-synced-thread","timestamp":"2026-08-05T11:00:00Z","cwd":"/project","cli_version":"0.147.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-08-05T11:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"external-import-turn-1"}}"#,
            r#"{"timestamp":"2026-08-05T11:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"imported request"}}"#,
            r#"{"timestamp":"2026-08-05T11:00:03Z","type":"event_msg","payload":{"type":"agent_message","message":"imported answer"}}"#,
            r#"{"timestamp":"2026-08-05T11:00:04Z","type":"event_msg","payload":{"type":"agent_message","message":"<EXTERNAL SESSION IMPORTED>"}}"#,
            r#"{"timestamp":"2026-08-05T11:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"external-import-turn-1","completed_at":1785930005.0}}"#,
            // Sync appends a genuinely new turn after the marker once the source conversation
            // gains more messages upstream.
            r#"{"timestamp":"2026-08-05T11:00:06Z","type":"event_msg","payload":{"type":"task_started","turn_id":"external-import-turn-2"}}"#,
            r#"{"timestamp":"2026-08-05T11:00:07Z","type":"event_msg","payload":{"type":"user_message","message":"follow-up request added after sync"}}"#,
            r#"{"timestamp":"2026-08-05T11:00:08Z","type":"event_msg","payload":{"type":"agent_message","message":"follow-up answer"}}"#,
            r#"{"timestamp":"2026-08-05T11:00:09Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"external-import-turn-2","completed_at":1785930009.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 2);
        assert_eq!(
            turns[0]
                .agent_messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            vec!["imported answer"]
        );
        assert_eq!(
            turns[1].user_message.as_deref(),
            Some("follow-up request added after sync")
        );
        assert_eq!(
            turns[1]
                .agent_messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            vec!["follow-up answer"]
        );
    }

    // Codex v0.149.0 (PR #39081, upstream openai/codex): the TUI's in-memory
    // per-thread replay buffer now caps buffered `AgentMessageDelta` bytes at
    // 256 KiB per thread (`MAX_BUFFERED_AGENT_MESSAGE_DELTA_BYTES`), evicting the
    // oldest buffered deltas once that total is exceeded, and coalesces
    // consecutive same-item deltas into merged buffered entries capped at 4 KiB
    // each (`MAX_COALESCED_AGENT_MESSAGE_DELTA_BYTES`). This bound lives entirely
    // in `codex-rs/tui/src/app/thread_event_buffer.rs` — it governs only what the
    // live TUI keeps in memory to replay when a user switches back to an inactive
    // thread. codex-trace never reads that in-memory buffer; it parses the
    // on-disk rollout JSONL directly, where Codex core still writes the
    // fully-assembled `agent_message` event with the complete text once
    // streaming finishes. Any intermediate `agent_message_delta` events recorded
    // in the JSONL are already ignored by the wildcard arm in `handle_event_msg`
    // (same as `outputDelta` in v0.129.0), so they can never truncate the final
    // message either. Verify a message exceeding the 256 KiB TUI eviction
    // threshold still parses in full, with no truncation, even when interleaved
    // with delta events that would have overflowed the TUI's live buffer.
    #[test]
    fn v0149_agent_message_delta_replay_buffer_cap_does_not_truncate_full_message() {
        let full_text = "a".repeat(300 * 1024); // 300 KiB, exceeds the 256 KiB TUI cap
        let delta_chunk = "b".repeat(8 * 1024); // exceeds the 4 KiB per-entry coalesce cap

        let session_meta = r#"{"timestamp":"2026-08-20T10:00:00Z","type":"session_meta","payload":{"id":"v0149-session","timestamp":"2026-08-20T10:00:00Z","cwd":"/project","cli_version":"0.149.0","model_provider":"openai"}}"#.to_string();
        let task_started = r#"{"timestamp":"2026-08-20T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#.to_string();
        // Oversized delta events, as would have been evicted from the TUI's
        // in-memory replay buffer once their combined size passed 256 KiB.
        let delta_lines: Vec<String> = (0..40)
            .map(|i| {
                let mut line = String::from(
                    r#"{"timestamp":"2026-08-20T10:00:02Z","type":"event_msg","payload":{"type":"agent_message_delta","item_id":"item-1","delta":""#,
                );
                line.push_str(&delta_chunk);
                line.push_str(r#"","seq":"#);
                line.push_str(&i.to_string());
                line.push_str("}}");
                line
            })
            .collect();
        let agent_message = {
            let mut line = String::from(
                r#"{"timestamp":"2026-08-20T10:00:03Z","type":"event_msg","payload":{"type":"agent_message","message":""#,
            );
            line.push_str(&full_text);
            line.push_str(r#"","phase":"final_answer"}}"#);
            line
        };
        let task_complete = r#"{"timestamp":"2026-08-20T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1755683044.0}}"#.to_string();

        let mut lines: Vec<&str> = vec![&session_meta, &task_started];
        lines.extend(delta_lines.iter().map(|s| s.as_str()));
        lines.push(&agent_message);
        lines.push(&task_complete);

        let entries = entries(&lines);
        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].agent_messages.len(),
            1,
            "delta events must not produce separate agent messages"
        );
        assert_eq!(turns[0].agent_messages[0].text.len(), 300 * 1024);
        assert_eq!(turns[0].agent_messages[0].text, full_text);
        assert!(turns[0].agent_messages[0].text.len() > 256 * 1024);
        assert_eq!(turns[0].final_answer.as_deref(), Some(full_text.as_str()));
    }
}
