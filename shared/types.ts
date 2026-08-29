export interface GitInfo {
  commit_hash?: string;
  branch?: string;
  repository_url?: string;
}

/** Codex v0.144.0 (PR #30488): a single selectable usage-limit reset credit entry. */
export interface RateLimitCredit {
  /** Credit category (e.g. "monthly", "trial"). Null when absent. */
  type: string | null;
  /** ISO-8601 expiration timestamp for this credit. Null when absent. */
  expiration: string | null;
}

/** Codex v0.144.0 (PR #30488): rate-limit reset credit data from `token_count` events. */
export interface RateLimitsInfo {
  /** Selectable credit options. Empty for pre-v0.144.0 sessions. */
  credits: RateLimitCredit[];
}

export interface TokenInfo {
  input_tokens: number;
  cached_input_tokens: number;
  output_tokens: number;
  reasoning_output_tokens: number;
  total_tokens: number;
  context_window_tokens: number | null;
  model_context_window: number;
  /** Codex v0.144.0 (PR #30488): rate-limit reset credit data from the same token_count event. Null for pre-v0.144.0 sessions. */
  rate_limits: RateLimitsInfo | null;
}

export interface AgentMessage {
  text: string;
  phase: "commentary" | "final_answer" | null;
  timestamp: string;
  is_reasoning: boolean;
  /** Position in the raw entry stream. `CodexTurn.tool_call_orders` uses the same scale, so
   * the UI can interleave messages and tool calls chronologically. Absent for old cached data. */
  order?: number;
}

/** Codex v0.132.0 (PR #23148): memory summaries are now versioned.
 * Pre-v0.132.0 sessions use plain strings; `version` is absent for those. */
export interface MemorySummary {
  content: string;
  /** Format version. Absent for pre-v0.132.0 sessions (plain-string format). */
  version?: number;
}

/** Codex v0.144.0 (PR #30488): a single credit option for resetting usage limits. */
export interface ResetCredit {
  /** Credit kind, e.g. `"subscription"` or `"purchased"`. Null for pre-v0.144.0 sessions. */
  type: string | null;
  /** ISO-8601 expiration timestamp, or null if the credit does not expire. */
  expiration: string | null;
}

/** Codex v0.144.0 (PR #30488): rate-limit data from a `token_count` event.
 * The `rate_limits` field is a sibling of `info` on `token_count` payloads.
 * Null in pre-v0.144.0 sessions or when the API returns no rate-limit data. */
export interface RateLimitInfo {
  /** ISO-8601 timestamp when the usage limit resets. */
  reset_at: string | null;
  /** All credits available for redeeming the reset. Populated by Codex v0.144.0+. */
  reset_credits: ResetCredit[];
  /** The credit selected for redemption when multiple are available (Codex v0.144.0+). */
  selected_reset_credit: ResetCredit | null;
}

/** Codex v0.135.0 (PR #24368): compaction metadata from turn headers. */
export interface CompactionMeta {
  /** Context-window tokens present before compaction. */
  tokens_before: number | null;
  /** Context-window tokens remaining after compaction. */
  tokens_after: number | null;
  /** Optional human-readable summary of what was compacted. */
  summary: string | null;
  /** What triggered the compaction: `"auto"` (threshold-based) or `"manual"` (user-requested). Null for sessions that predate this field. */
  compaction_trigger: string | null;
  /** Codex v0.142.0 (PR #29256): opaque ID linking this context window to its compaction
   * ancestor, enabling lineage reconstruction across compaction boundaries.
   * Null for sessions predating v0.142.0. Context window IDs use UUIDv7 format (PR #28953). */
  lineage_id: string | null;
}

export interface CollabSpawn {
  call_id: string;
  new_session_id: string;
  agent_nickname: string;
  agent_role: string;
  model?: string | null;
  reasoning_effort?: string | null;
  prompt_preview: string;
}

/** Codex v0.150.0 (PRs #40308, #40315): a task `@`-mentioned from the TUI composer, decoded
 * out of a user message's task-mention encoding. */
export interface TaskMentionRef {
  title: string;
  thread_id: string;
}

export type ToolKind =
  | "exec_command"
  | "mcp_tool"
  | "patch_apply"
  | "web_search"
  | "image_generation"
  | "spawn_agent"
  | "wait_agent"
  /** Codex < v0.139.0 used `close_agent`; renamed to `interrupt_agent` in v0.139.0 (PR #26994). */
  | "interrupt_agent"
  /** multi-agent v2: assign_task (Codex < v0.136.0) or followup_task (≥ v0.136.0) */
  | "followup_task"
  /** Codex v0.136.0 (PR #24962): shell hook outputs from pre/post-tool lifecycle hooks. */
  | "shell_hook"
  /** Codex v0.140.0 (PRs #27438, #27488, #27518): built-in runtime tools for querying the
   * remaining context budget (`token_budget_context`, `context_remaining`, `context_window`). */
  | "context_query"
  /** Codex's Agent Plugins subsystem (issue #223): built-in tools for searching and
   * installing plugin/connector catalogs (`list_available_plugins_to_install`,
   * `request_plugin_install`). */
  | "agent_plugin"
  | "unknown";

export interface CodexToolCall {
  call_id: string;
  kind: ToolKind;
  name: string;
  arguments: Record<string, unknown>;
  input_text: string | null;
  output: string | null;
  exit_code: number | null;
  command: string[] | null;
  cwd: string | null;
  duration_secs: number | null;
  mcp_server: string | null;
  mcp_tool: string | null;
  /** Codex v0.133.0+: identifies which plugin the MCP tool belongs to. Codex v0.146.0+ also
   * sets this on ExecCommand calls attributed to a trusted plugin script (issue #223). Null
   * for pre-v0.133.0 sessions and non-plugin-attributed calls. */
  plugin_id: string | null;
  /** Codex v0.146.0+: safe plugin-relative script path attributed to an ExecCommand call run
   * by a plugin script (issue #223). Null for non-plugin-attributed exec calls and
   * pre-v0.146.0 sessions. */
  script_path: string | null;
  /** Codex v0.134.0+ (PR #22882): subagent session ID from hook input identity fields. Null for parent-agent calls and pre-v0.134.0 sessions. */
  subagent_id: string | null;
  /** Codex v0.134.0+ (PR #22882): subagent human-readable name from hook input identity fields. Null for parent-agent calls and pre-v0.134.0 sessions. */
  subagent_name: string | null;
  patch_success: boolean | null;
  patch_changes: Record<string, { type: string; content?: string; unified_diff?: string }> | null;
  web_query: string | null;
  web_url: string | null;
  image_prompt: string | null;
  /** Codex v0.138.0 (PRs #25944, #25947): saved file path for image_generation and local image attachment results. Null for pre-v0.138.0 sessions and non-image calls. */
  image_file_path: string | null;
  worker_session: CodexSession | null;
  status: string;
  /** Codex v0.145.0: true when exec_command_end signals that aggregated_output was clipped by the runtime's output limit. Null for pre-v0.145.0 sessions and non-exec tool calls. */
  output_truncated: boolean | null;
}

export interface CodexTurn {
  turn_id: string;
  started_at: number | null;
  completed_at: number | null;
  duration_ms: number | null;
  status: "complete" | "aborted" | "cancelled" | "ongoing" | "error";
  user_message: string | null;
  agent_messages: AgentMessage[];
  tool_calls: CodexToolCall[];
  /** Display-order index for each tool call, parallel to `tool_calls` (same length/order).
   * Same scale as `AgentMessage.order`. Absent for old cached data. */
  tool_call_orders?: number[];
  final_answer: string | null;
  total_tokens: TokenInfo | null;
  model: string | null;
  cwd: string | null;
  reasoning_effort: string | null;
  error: string | null;
  has_compaction: boolean;
  thread_name: string | null;
  collab_spawns: CollabSpawn[];
  /** Codex v0.134.0 (PR #23980): OTel trace ID from TurnStartedEvent. Null for pre-v0.134.0 sessions. */
  trace_id: string | null;
  /** Codex v0.135.0 (PR #24160): thread ID this turn was forked from. Null for non-forked turns. */
  forked_from_thread_id: string | null;
  /** Codex v0.135.0 (PR #24368): compaction metadata at turn start. Null for pre-v0.135.0 sessions. */
  compaction_meta: CompactionMeta | null;
  /** Active memories injected at turn start (Codex v0.135.0+, PR #24591).
   * Items carry an optional version field (Codex v0.132.0+, PR #23148). Empty for older sessions. */
  memories?: MemorySummary[];
  /** Rate-limit data from the most recent `token_count` event (Codex v0.144.0+, PR #30488).
   * Null for pre-v0.144.0 sessions or when the API returns no rate-limit data.
   * Absent for cached data serialized before this field was added. */
  rate_limit_info?: RateLimitInfo | null;
  /** Audio transcript segments from realtime voice turns (Codex v0.143.0+ trailing events;
   * Codex v0.145.0+ live mid-turn V3 streaming audio). Empty for non-voice sessions.
   * Absent for cached data serialized before this field was added. */
  audio_transcript?: string[];
  /** Warning messages emitted during the turn (`EventMsg::Warning`), e.g. skill catalog
   * budget/truncation notices (Codex v0.146.0+). Empty when no warnings occurred.
   * Absent for cached data serialized before this field was added. */
  warnings?: string[];
  /** Codex v0.150.0 (PRs #40308, #40315): tasks `@`-mentioned in `user_message` via the TUI
   * composer's task-mention encoding, decoded back to readable `@Title` text in `user_message`.
   * Empty for turns with no task mentions and for pre-v0.150.0 sessions.
   * Absent for cached data serialized before this field was added. */
  task_mentions?: TaskMentionRef[];
}

/**
 * Session JSONL response_item types that appear only in archive sessions recorded before
 * Codex v0.140.0 (PR #27801 removed the experimental /realtime voice subsystem from the TUI):
 *   - `speech_append`      — raw audio bytes appended during a voice turn
 *   - `realtime_handoff`   — handoff event from text to realtime voice session
 *   - `audio_transcript`   — server-side transcript of recognised speech
 * These item types are never produced by Codex ≥ v0.140.0 and carry no turn-building
 * semantics for codex-trace. The Rust parser silently skips them so that old session
 * archives continue to open without error.
 */

export interface CodexSession {
  id: string;
  timestamp: string;
  cwd: string | null;
  originator: string | null;
  cli_version: string | null;
  model_provider: string | null;
  git: GitInfo | null;
  instructions: string | null;
  turns: CodexTurn[];
  is_ongoing: boolean;
  total_tokens: TokenInfo | null;
  thread_name: string | null;
  spawned_worker_ids: string[];
  ai_title: string | null;
  path: string;
  /** true when the session was started via `codex remote-control` (Codex v0.130.0+) */
  is_headless: boolean;
  /**
   * true when the session contains spawn_agent calls whose output metadata was hidden.
   * Codex v0.137.0 (PR #26114) changed hide_spawn_agent_metadata to default true.
   * When true, multi-agent subagent lineage is absent — set hide_spawn_agent_metadata = false
   * in Codex config to restore full trace coverage.
   */
  has_missing_spawn_metadata: boolean;
  /** true when the session has been archived via `codex archive` (Codex v0.136.0+). */
  is_archived: boolean;
  /** Approval mode from session_meta.ask_for_approval (Codex v0.144.0+, PR #30482).
   * Known values: "suggest", "auto-edit", "full-auto", "writes" (new in v0.144.0).
   * Checked against upstream (PR #36373, #37057): v0.147.0's `--approve-for-me`
   * flag and v0.146.1's cyber-model auto-review defaults do NOT add a new value
   * here — both route through the existing "on-request" policy plus a separate
   * auto-review-reviewer setting, not a new ask_for_approval string. Parsing is
   * already a permissive passthrough, so no new value is expected.
   * Null for sessions predating v0.144.0 or when the field is absent. */
  approval_mode: string | null;
  /** Codex v0.146.0 (PRs #34621, #35220): thread ID this rollout's paginated history
   * inherits from (session_meta.payload.history_base.thread_id). Distinct from the
   * per-turn `forked_from_thread_id` and compaction `lineage_id` fields — this one marks
   * the whole rollout file as a continuation of another paginated thread's history.
   * Null for legacy-history sessions or paginated threads with no inherited prefix. */
  history_base_thread_id: string | null;
  /** Thread ID this session was forked from, read from `session_meta.payload.forked_from_id`
   * — the app-server's `SessionConfiguredEvent.forked_from_id`, persisted verbatim into the
   * rollout's `session_meta` line. This field predates Codex v0.148.0, but `codex exec fork`
   * (new subcommand) and `codex exec resume` (Codex v0.148.0+, PR #37367, issue #238) are the
   * first `codex exec` code paths to populate it — previously hardcoded to `null` there.
   * Session-level signal, distinct from the per-turn `forked_from_thread_id` on `CodexTurn`.
   * Null for non-forked sessions. */
  forked_from_thread_id: string | null;
}

export interface CodexSessionInfo {
  id: string;
  path: string;
  cwd: string | null;
  git_branch: string | null;
  originator: string | null;
  model: string | null;
  cli_version: string | null;
  thread_name: string | null;
  turn_count: number;
  start_time: string;
  end_time: string | null;
  total_tokens: number | null;
  is_ongoing: boolean;
  /** true when session_meta.source.subagent is set (system-spawned: review, memory_consolidation) */
  is_external_worker: boolean;
  /** true when this session's id appears in another session's spawned_worker_ids */
  is_inline_worker: boolean;
  worker_nickname: string | null;
  worker_role: string | null;
  spawned_worker_ids: string[];
  date_group: string;
  ai_title: string | null;
  /** true when the session was started via `codex remote-control` (Codex v0.130.0+) */
  is_headless: boolean;
  /** true when the session has been archived via `codex archive` (Codex v0.136.0+). */
  is_archived: boolean;
  /** Approval mode from session_meta.ask_for_approval (Codex v0.144.0+, PR #30482).
   * Known values: "suggest", "auto-edit", "full-auto", "writes" (new in v0.144.0).
   * Checked against upstream (PR #36373, #37057): v0.147.0's `--approve-for-me`
   * flag and v0.146.1's cyber-model auto-review defaults do NOT add a new value
   * here — both route through the existing "on-request" policy plus a separate
   * auto-review-reviewer setting, not a new ask_for_approval string. Parsing is
   * already a permissive passthrough, so no new value is expected.
   * Null for sessions predating v0.144.0 or when the field is absent. */
  approval_mode: string | null;
  /** Codex v0.146.0 (PRs #34621, #35220): thread ID this rollout's paginated history
   * inherits from (session_meta.payload.history_base.thread_id). Distinct from the
   * per-turn `forked_from_thread_id` and compaction `lineage_id` fields — this one marks
   * the whole rollout file as a continuation of another paginated thread's history.
   * Null for legacy-history sessions or paginated threads with no inherited prefix. */
  history_base_thread_id: string | null;
  /** Thread ID this session was forked from, read from `session_meta.payload.forked_from_id`
   * — the app-server's `SessionConfiguredEvent.forked_from_id`, persisted verbatim into the
   * rollout's `session_meta` line. This field predates Codex v0.148.0, but `codex exec fork`
   * (new subcommand) and `codex exec resume` (Codex v0.148.0+, PR #37367, issue #238) are the
   * first `codex exec` code paths to populate it — previously hardcoded to `null` there.
   * Session-level signal, distinct from the per-turn `forked_from_thread_id` on `CodexTurn`.
   * Null for non-forked sessions. */
  forked_from_thread_id: string | null;
  /** Codex v0.150.0 (PRs #40308, #40315): thread IDs of other Codex tasks `@`-mentioned from
   * the TUI composer in this session's `user_message` events. Distinct from
   * `spawned_worker_ids` — a task mention is a live reference to an existing, independently
   * spawned task, not a child spawned by this session. Empty for sessions with no task
   * mentions and for pre-v0.150.0 sessions. */
  mentioned_thread_ids: string[];
}

export interface SettingsResponse {
  sessions_dir: string | null;
  default_dir: string;
}

export type ViewState = "picker" | "list" | "detail";
