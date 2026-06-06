export interface GitInfo {
  commit_hash?: string;
  branch?: string;
  repository_url?: string;
}

export interface TokenInfo {
  input_tokens: number;
  cached_input_tokens: number;
  output_tokens: number;
  reasoning_output_tokens: number;
  total_tokens: number;
  context_window_tokens: number | null;
  model_context_window: number;
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

/** Codex v0.135.0 (PR #24368): compaction metadata from turn headers. */
export interface CompactionMeta {
  /** Context-window tokens present before compaction. */
  tokens_before: number | null;
  /** Context-window tokens remaining after compaction. */
  tokens_after: number | null;
  /** Optional human-readable summary of what was compacted. */
  summary: string | null;
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

export type ToolKind =
  | "exec_command"
  | "mcp_tool"
  | "patch_apply"
  | "web_search"
  | "image_generation"
  | "spawn_agent"
  | "wait_agent"
  | "close_agent"
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
  /** Codex v0.133.0+: identifies which plugin the MCP tool belongs to. Null for pre-v0.133.0 sessions. */
  plugin_id: string | null;
  patch_success: boolean | null;
  patch_changes: Record<string, { type: string; content?: string; unified_diff?: string }> | null;
  web_query: string | null;
  web_url: string | null;
  image_prompt: string | null;
  worker_session: CodexSession | null;
  status: string;
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
  /** Active memories injected at turn start (Codex v0.135.0+, PR #24591). Empty for older sessions. */
  memories?: string[];
}

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
}

export interface SettingsResponse {
  sessions_dir: string | null;
  default_dir: string;
}

export type ViewState = "picker" | "list" | "detail";
