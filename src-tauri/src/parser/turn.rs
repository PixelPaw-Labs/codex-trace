use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use super::entry::{parse_timestamp_secs, RawEntry};
use super::spawn::parse_spawn_agent_output;
use super::toolcall::{ToolCall, ToolCallBuilder};

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
    /// Empty for sessions from older Codex versions.
    pub memories: Vec<String>,
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
            });
            turns.insert(turn_id.clone(), turn);
            *current_turn_id = Some(turn_id.clone());
            tool_builders
                .entry(turn_id)
                .or_insert_with(ToolCallBuilder::new);
        }

        "user_message" => {
            let message = payload
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if !has_task_started {
                // Old format: each user_message starts a new turn
                *synthetic_counter += 1;
                let turn_id = format!("turn-{synthetic_counter}");
                let started_at = entry.timestamp.as_deref().and_then(parse_timestamp_secs);
                let mut turn = CodexTurn::new(turn_id.clone());
                turn.started_at = started_at;
                turn.user_message = Some(message.clone());
                turns.insert(turn_id.clone(), turn);
                *current_turn_id = Some(turn_id.clone());
                tool_builders
                    .entry(turn_id)
                    .or_insert_with(ToolCallBuilder::new);
            } else if let Some(ref tid) = current_turn_id {
                if let Some(turn) = turns.get_mut(tid) {
                    if turn.user_message.is_none() {
                        turn.user_message = Some(message);
                    }
                }
            }
        }

        "agent_message" => {
            if let Some(ref tid) = current_turn_id {
                if let Some(turn) = turns.get_mut(tid) {
                    let text = payload
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if !text.is_empty() {
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

    let tid = match current_turn_id {
        Some(t) => t,
        None => return,
    };

    let builder = tool_builders
        .entry(tid.clone())
        .or_insert_with(ToolCallBuilder::new);

    match item_type {
        "function_call" => {
            let call_id = str_field(payload, "call_id");
            let name = str_field(payload, "name");
            let arguments_str = payload
                .get("arguments")
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
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
                arguments_str,
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
            if let Some(spawn) = spawn_from_function_call_output(builder, &call_id, &output) {
                if let Some(turn) = turns.get_mut(tid) {
                    turn.collab_spawns.push(spawn);
                }
            }
            builder.add_function_call_output(&call_id, &output);
        }

        "custom_tool_call" => {
            let call_id = str_field(payload, "call_id");
            let name = str_field(payload, "name");
            let input = payload
                .get("input")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            builder.add_custom_tool_call(call_id, name, input);
        }

        "custom_tool_call_output" => {
            let call_id = str_field(payload, "call_id");
            // output field is a JSON string: {"output":"...","metadata":{"exit_code":N,...}}
            let raw_output = payload.get("output").and_then(|v| v.as_str()).unwrap_or("");
            let output = serde_json::from_str::<Value>(raw_output)
                .ok()
                .and_then(|v| {
                    v.get("output")
                        .and_then(|o| o.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| raw_output.to_string());
            let exit_code = serde_json::from_str::<Value>(raw_output)
                .ok()
                .and_then(|v| {
                    v.get("metadata")
                        .and_then(|m| m.get("exit_code"))
                        .and_then(|c| c.as_i64())
                        .map(|c| c as i32)
                });
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
            builder.add_function_call_output(&call_id, &output);
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
    let memories: Vec<String> = payload
        .get("memories")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(|s| s.to_string()))
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
        assert_eq!(turns[0].memories[0], "User prefers terse output");
        assert_eq!(turns[0].memories[1], "Project uses TypeScript strict mode");
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
        assert_eq!(turns[0].memories, vec!["Initial memory"]);
        assert_eq!(
            turns[1].memories,
            vec!["Initial memory", "New memory added"]
        );
    }
}
