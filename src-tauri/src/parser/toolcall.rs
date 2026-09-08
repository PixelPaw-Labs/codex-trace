use super::redact::redact_secrets;
use super::spawn::parse_spawn_agent_output;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    ExecCommand,
    McpTool,
    PatchApply,
    WebSearch,
    ImageGeneration,
    SpawnAgent,
    WaitAgent,
    /// Codex < v0.139.0 used `close_agent`; renamed to `interrupt_agent` in v0.139.0 (PR #26994).
    /// Both old transcripts (close_agent) and new (interrupt_agent) map to this variant.
    InterruptAgent,
    /// multi-agent v2 task assignment: `assign_task` (Codex < v0.136.0) or `followup_task` (≥ v0.136.0)
    FollowupTask,
    /// Codex v0.136.0 (PR #24962): shell hook outputs from pre/post-tool lifecycle hooks.
    ShellHook,
    /// Codex v0.140.0 (PRs #27438, #27488, #27518): built-in runtime tools that the model
    /// invokes to query the remaining context budget. Covers `token_budget_context`,
    /// `context_remaining`, and `context_window`.
    ContextQuery,
    /// Codex's Agent Plugins subsystem (issue #223): built-in tools for searching and
    /// installing plugin/connector catalogs. Covers `list_available_plugins_to_install`
    /// and `request_plugin_install`.
    AgentPlugin,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub call_id: String,
    pub kind: ToolKind,
    pub name: String,
    pub arguments: Value,
    pub input_text: Option<String>,
    pub output: Option<String>,
    pub exit_code: Option<i32>,
    pub command: Option<Vec<String>>,
    pub cwd: Option<String>,
    pub duration_secs: Option<f64>,
    pub mcp_server: Option<String>,
    pub mcp_tool: Option<String>,
    /// Codex v0.133.0 (PRs #23353, #23737): plugin_id identifies which plugin
    /// an MCP tool belongs to. Absent for pre-v0.133.0 sessions and non-MCP calls.
    /// Codex v0.146.0 also sets this on ExecCommand calls attributed to a trusted
    /// plugin script (issue #223).
    pub plugin_id: Option<String>,
    /// Codex v0.146.0: safe plugin-relative script path attributed to an ExecCommand
    /// call run by a plugin script. Absent for non-plugin-attributed exec calls and
    /// pre-v0.146.0 sessions (issue #223).
    pub script_path: Option<String>,
    pub patch_success: Option<bool>,
    pub patch_changes: Option<Value>,
    pub web_query: Option<String>,
    pub web_url: Option<String>,
    pub image_prompt: Option<String>,
    /// Codex v0.138.0 (PRs #25944, #25947): saved file path exposed by image_generation and
    /// local image attachment results. Absent for pre-v0.138.0 sessions and non-image calls.
    pub image_file_path: Option<String>,
    pub worker_session: Option<Box<super::session::CodexSession>>,
    pub status: String,
    /// Codex v0.134.0 (PR #22882): subagent session ID from hook input identity fields.
    /// Null for parent-agent tool calls and sessions from pre-v0.134.0.
    pub subagent_id: Option<String>,
    /// Codex v0.134.0 (PR #22882): subagent human-readable name from hook input identity fields.
    /// Null for parent-agent tool calls and sessions from pre-v0.134.0.
    pub subagent_name: Option<String>,
    /// Codex v0.145.0: exec_command_end may include a truncation indicator when the runtime
    /// bounded command output to fix an unbounded-exec bug. True means `aggregated_output`
    /// is partial; None means no truncation field was present (pre-v0.145.0 or unaffected run).
    #[serde(default)]
    pub output_truncated: Option<bool>,
}

/// A pending (not yet finalized) tool call — waiting for its end event.
#[derive(Debug, Clone)]
pub struct PendingCall {
    pub name: String,
    pub arguments: Value,
    pub input_text: Option<String>,
    /// Raw namespace from the function_call payload (e.g. "mcp__codex_apps__github").
    pub namespace: Option<String>,
    /// v0.130.0+: direct MCP server name from tool_id.server (bypasses namespace parsing).
    pub mcp_server: Option<String>,
    /// v0.133.0+: plugin_id from tool_id.plugin_id or mcp_tool_call.plugin_id.
    pub plugin_id: Option<String>,
}

/// Builder that collects function_call / custom_tool_call entries and finalizes
/// them when the corresponding end event arrives.
pub struct ToolCallBuilder {
    pub pending: HashMap<String, PendingCall>,
    pub finalized: Vec<ToolCall>,
    pty_sessions: HashMap<String, String>,
    running_exec_call_ids: Vec<String>,
    /// Codex v0.141.0+ (PRs #27365, #27371): maps tool names to (tool_type, server).
    /// Populated from the `dynamic_tools` field in `task_started` events.
    /// Used to classify MCP/connector tools that arrive without a namespace field.
    dynamic_tool_registry: HashMap<String, (String, String)>,
}

impl ToolCallBuilder {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
            finalized: Vec::new(),
            pty_sessions: HashMap::new(),
            running_exec_call_ids: Vec::new(),
            dynamic_tool_registry: HashMap::new(),
        }
    }

    /// Set the dynamic tool registry built from `task_started.dynamic_tools` (Codex v0.141.0+).
    pub fn set_dynamic_tool_registry(&mut self, registry: HashMap<String, (String, String)>) {
        self.dynamic_tool_registry = registry;
    }

    /// Extend the dynamic tool registry with tools discovered via tool-search (Codex v0.142.2+).
    /// Called when a `tool_search_call_output` response item is encountered mid-turn.
    pub fn extend_dynamic_tool_registry(&mut self, extra: HashMap<String, (String, String)>) {
        self.dynamic_tool_registry.extend(extra);
    }

    /// Register a function_call (response_item).
    pub fn add_function_call(
        &mut self,
        call_id: String,
        name: String,
        arguments_str: &str,
        namespace: Option<String>,
        mcp_server_direct: Option<String>,
        plugin_id: Option<String>,
    ) {
        let arguments = serde_json::from_str(arguments_str).unwrap_or(Value::Null);
        self.pending.insert(
            call_id,
            PendingCall {
                name,
                arguments,
                input_text: None,
                namespace,
                mcp_server: mcp_server_direct,
                plugin_id,
            },
        );
    }

    /// Register a custom_tool_call (apply_patch, the desktop `exec` tool, etc.).
    pub fn add_custom_tool_call(&mut self, call_id: String, name: String, input: Option<String>) {
        self.pending.insert(
            call_id,
            PendingCall {
                name,
                arguments: Value::Object(serde_json::Map::new()),
                input_text: input,
                namespace: None,
                mcp_server: None,
                plugin_id: None,
            },
        );
    }

    /// Finalize a custom_tool_call (apply_patch, the desktop `exec` tool, etc.) with its output.
    pub fn finalize_custom_tool_output(
        &mut self,
        call_id: &str,
        output: &str,
        exit_code: Option<i32>,
    ) {
        if let Some(pending) = self.pending.remove(call_id) {
            let is_patch = pending.name == "apply_patch";
            self.finalized.push(ToolCall {
                call_id: call_id.to_string(),
                // Codex Desktop v0.153+ records its JavaScript/desktop execution
                // tool as `custom_tool_call` named `exec`. It is not an apply-patch
                // call; preserve it as Unknown so its input/output remains visible
                // without presenting arbitrary JavaScript as a patch.
                kind: if is_patch {
                    ToolKind::PatchApply
                } else {
                    ToolKind::Unknown
                },
                name: pending.name,
                arguments: pending.arguments,
                input_text: pending.input_text,
                output: Some(output.to_string()),
                exit_code,
                command: None,
                cwd: None,
                duration_secs: None,
                mcp_server: None,
                mcp_tool: None,
                plugin_id: None,
                script_path: None,
                patch_success: if is_patch {
                    exit_code.map(|c| c == 0)
                } else {
                    None
                },
                patch_changes: None,
                web_query: None,
                web_url: None,
                image_prompt: None,
                image_file_path: None,
                worker_session: None,
                // Structured custom-tool outputs do not carry an exit code.
                // Missing an exit code means normal completion, not failure.
                status: match exit_code {
                    Some(code) if code != 0 => "failed".to_string(),
                    _ => "completed".to_string(),
                },
                subagent_id: None,
                subagent_name: None,
                output_truncated: None,
            });
        }
    }

    /// Register a function_call_output (no typed end event).
    /// If the pending call has an MCP namespace, classify as McpTool. Built-in
    /// collaboration calls are also typed here because newer Codex SDK logs do
    /// not emit the older collab_*_end events.
    ///
    /// `file_path` carries the optional saved-file path added in Codex v0.138.0
    /// (PRs #25944, #25947) for image_generation and local image attachment results.
    pub fn add_function_call_output(
        &mut self,
        call_id: &str,
        output: &str,
        file_path: Option<&str>,
    ) {
        if let Some(pending) = self.pending.remove(call_id) {
            if pending.name == "exec_command" {
                let parsed_output = parse_exec_function_output(output);
                if let Some(session_id) = &parsed_output.running_session_id {
                    self.pty_sessions
                        .insert(session_id.clone(), call_id.to_string());
                }
                if parsed_output.status == "running" {
                    push_unique(&mut self.running_exec_call_ids, call_id.to_string());
                }
                self.finalized.push(exec_tool_call_from_pending(
                    call_id.to_string(),
                    pending,
                    parsed_output,
                ));
                return;
            }

            if pending.name == "write_stdin" {
                let parsed_output = parse_exec_function_output(output);
                let original_call_id = session_id_from_arguments(&pending.arguments)
                    .and_then(|session_id| {
                        self.pty_sessions
                            .get(&session_id)
                            .cloned()
                            .map(|call_id| (call_id, Some(session_id)))
                    })
                    .or_else(|| {
                        self.single_running_exec_call_id()
                            .map(|call_id| (call_id, None))
                    });

                if let Some((original_call_id, session_id)) = original_call_id {
                    self.merge_pty_output(&original_call_id, parsed_output);
                    if !self
                        .finalized
                        .iter()
                        .any(|tc| tc.call_id == original_call_id && tc.status == "running")
                    {
                        self.running_exec_call_ids
                            .retain(|call_id| call_id != &original_call_id);
                        if let Some(session_id) = session_id {
                            self.pty_sessions.remove(&session_id);
                        }
                    }
                    return;
                }

                self.finalized.push(exec_tool_call_from_pending(
                    call_id.to_string(),
                    pending,
                    parsed_output,
                ));
                return;
            }

            // Codex v0.135.0 (PR #24652): plain image wrapper spans removed from session
            // output. Image content is now emitted bare (e.g. {"type":"image_url",...})
            // rather than wrapped in {"type":"image_span","content":[...]}. Detect
            // image_generation by function name and extract the prompt from arguments —
            // never rely on the wrapper span type, which no longer exists in v0.135.0+.
            if pending.name == "image_generation" {
                let image_prompt = pending
                    .arguments
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let image_file_path = file_path.filter(|s| !s.is_empty()).map(|s| s.to_string());
                self.finalized.push(ToolCall {
                    call_id: call_id.to_string(),
                    kind: ToolKind::ImageGeneration,
                    name: pending.name,
                    arguments: pending.arguments,
                    input_text: pending.input_text,
                    output: if output.is_empty() {
                        None
                    } else {
                        Some(output.to_string())
                    },
                    exit_code: None,
                    command: None,
                    cwd: None,
                    duration_secs: None,
                    mcp_server: None,
                    mcp_tool: None,
                    plugin_id: None,
                    script_path: None,
                    patch_success: None,
                    patch_changes: None,
                    web_query: None,
                    web_url: None,
                    image_prompt,
                    image_file_path,
                    worker_session: None,
                    status: "completed".to_string(),
                    subagent_id: None,
                    subagent_name: None,
                    output_truncated: None,
                });
                return;
            }

            if pending.name == "spawn_agent" {
                self.finalized.push(ToolCall {
                    call_id: call_id.to_string(),
                    kind: ToolKind::SpawnAgent,
                    name: pending.name,
                    arguments: pending.arguments,
                    input_text: pending.input_text,
                    output: Some(output.to_string()),
                    exit_code: None,
                    command: None,
                    cwd: None,
                    duration_secs: None,
                    mcp_server: None,
                    mcp_tool: None,
                    plugin_id: None,
                    script_path: None,
                    patch_success: None,
                    patch_changes: None,
                    web_query: None,
                    web_url: None,
                    image_prompt: None,
                    image_file_path: None,
                    worker_session: None,
                    status: spawn_agent_status(output),
                    subagent_id: None,
                    subagent_name: None,
                    output_truncated: None,
                });
                return;
            }

            // Codex v0.140.0 (PRs #27438, #27488, #27518): built-in context-budget query tools.
            // token_budget_context, context_remaining, and context_window are invoked by the
            // model during turns; classify as ContextQuery for enriched display.
            if matches!(
                pending.name.as_str(),
                "token_budget_context" | "context_remaining" | "context_window"
            ) {
                self.finalized.push(ToolCall {
                    call_id: call_id.to_string(),
                    kind: ToolKind::ContextQuery,
                    name: pending.name,
                    arguments: pending.arguments,
                    input_text: pending.input_text,
                    output: if output.is_empty() {
                        None
                    } else {
                        Some(output.to_string())
                    },
                    exit_code: None,
                    command: None,
                    cwd: None,
                    duration_secs: None,
                    mcp_server: None,
                    mcp_tool: None,
                    plugin_id: None,
                    script_path: None,
                    patch_success: None,
                    patch_changes: None,
                    web_query: None,
                    web_url: None,
                    image_prompt: None,
                    image_file_path: None,
                    worker_session: None,
                    status: "completed".to_string(),
                    subagent_id: None,
                    subagent_name: None,
                    output_truncated: None,
                });
                return;
            }

            // Codex's Agent Plugins subsystem (issue #223): built-in tools for searching
            // and installing plugin/connector catalogs. Classify as AgentPlugin instead of
            // falling through to Unknown so the trace view can surface which plugin was
            // searched for or requested per turn.
            if matches!(
                pending.name.as_str(),
                "list_available_plugins_to_install" | "request_plugin_install"
            ) {
                self.finalized.push(ToolCall {
                    call_id: call_id.to_string(),
                    kind: ToolKind::AgentPlugin,
                    name: pending.name,
                    arguments: pending.arguments,
                    input_text: pending.input_text,
                    output: if output.is_empty() {
                        None
                    } else {
                        Some(output.to_string())
                    },
                    exit_code: None,
                    command: None,
                    cwd: None,
                    duration_secs: None,
                    mcp_server: None,
                    mcp_tool: None,
                    plugin_id: None,
                    script_path: None,
                    patch_success: None,
                    patch_changes: None,
                    web_query: None,
                    web_url: None,
                    image_prompt: None,
                    image_file_path: None,
                    worker_session: None,
                    status: "completed".to_string(),
                    subagent_id: None,
                    subagent_name: None,
                    output_truncated: None,
                });
                return;
            }

            // assign_task (Codex < v0.136.0, PR #25267) was renamed to followup_task
            // (Codex ≥ v0.136.0, PR #25636). Both represent the multi-agent v2 task
            // assignment tool and are classified as FollowupTask.
            if pending.name == "assign_task" || pending.name == "followup_task" {
                self.finalized.push(ToolCall {
                    call_id: call_id.to_string(),
                    kind: ToolKind::FollowupTask,
                    name: pending.name,
                    arguments: pending.arguments,
                    input_text: pending.input_text,
                    output: Some(output.to_string()),
                    exit_code: None,
                    command: None,
                    cwd: None,
                    duration_secs: None,
                    mcp_server: None,
                    mcp_tool: None,
                    plugin_id: None,
                    script_path: None,
                    patch_success: None,
                    patch_changes: None,
                    web_query: None,
                    web_url: None,
                    image_prompt: None,
                    image_file_path: None,
                    worker_session: None,
                    status: "completed".to_string(),
                    subagent_id: None,
                    subagent_name: None,
                    output_truncated: None,
                });
                return;
            }

            // v0.130.0+: direct mcp_server from tool_id takes precedence over namespace parsing.
            let (kind, mcp_server, mcp_tool) = if let Some(ref server) = pending.mcp_server {
                (
                    ToolKind::McpTool,
                    Some(server.clone()),
                    Some(pending.name.clone()),
                )
            } else {
                match &pending.namespace {
                    Some(ns) if ns.starts_with("mcp__") => {
                        let (server, tool) = parse_mcp_namespace(ns, &pending.name);
                        (ToolKind::McpTool, server, tool)
                    }
                    _ if pending.name == "wait_agent" => (ToolKind::WaitAgent, None, None),
                    // close_agent (Codex < v0.139.0) was renamed to interrupt_agent (≥ v0.139.0, PR #26994).
                    // Accept both for backward compatibility with existing transcripts.
                    _ if pending.name == "close_agent" || pending.name == "interrupt_agent" => {
                        (ToolKind::InterruptAgent, None, None)
                    }
                    _ => {
                        // v0.141.0+ (PRs #27365, #27371): tool name may be namespace-qualified
                        // as "mcp:server/tool_name" or looked up via dynamic_tools registry.
                        if let Some((tool_type, server, tool)) =
                            parse_namespaced_tool_name(&pending.name)
                        {
                            match tool_type.as_str() {
                                "mcp" => (ToolKind::McpTool, Some(server), Some(tool)),
                                _ => (ToolKind::Unknown, None, None),
                            }
                        } else if let Some((tool_type, server)) =
                            self.dynamic_tool_registry.get(&pending.name).cloned()
                        {
                            match tool_type.as_str() {
                                "mcp" => {
                                    (ToolKind::McpTool, Some(server), Some(pending.name.clone()))
                                }
                                _ => (ToolKind::Unknown, None, None),
                            }
                        } else {
                            (ToolKind::Unknown, None, None)
                        }
                    }
                }
            };
            let plugin_id = if kind == ToolKind::McpTool {
                pending.plugin_id
            } else {
                None
            };
            self.finalized.push(ToolCall {
                call_id: call_id.to_string(),
                kind,
                name: pending.name,
                arguments: pending.arguments,
                input_text: pending.input_text,
                output: Some(output.to_string()),
                exit_code: None,
                command: None,
                cwd: None,
                duration_secs: None,
                mcp_server,
                mcp_tool,
                plugin_id,
                script_path: None,
                patch_success: None,
                patch_changes: None,
                web_query: None,
                web_url: None,
                image_prompt: None,
                image_file_path: None,
                worker_session: None,
                status: "completed".to_string(),
                subagent_id: None,
                subagent_name: None,
                output_truncated: None,
            });
        }
    }

    fn merge_pty_output(&mut self, original_call_id: &str, output: ExecFunctionOutput) {
        let Some(tool_call) = self
            .finalized
            .iter_mut()
            .find(|tc| tc.call_id == original_call_id)
        else {
            return;
        };

        append_output(&mut tool_call.output, output.output);
        if output.exit_code.is_some() {
            tool_call.exit_code = output.exit_code;
        }
        if output.duration_secs.is_some() {
            tool_call.duration_secs =
                Some(tool_call.duration_secs.unwrap_or(0.0) + output.duration_secs.unwrap_or(0.0));
        }
        tool_call.status = output.status;
    }

    fn single_running_exec_call_id(&self) -> Option<String> {
        let mut running = self.running_exec_call_ids.iter();
        let first = running.next()?;
        if running.next().is_none() {
            Some(first.clone())
        } else {
            None
        }
    }

    /// Finalize with exec_command_end event.
    pub fn finalize_exec(&mut self, event_type: &str, payload: &Value) {
        let call_id = str_field(payload, "call_id");
        let pending = self
            .pending
            .remove(&call_id)
            .unwrap_or_else(|| PendingCall {
                name: kind_name(event_type),
                arguments: Value::Null,
                input_text: None,
                namespace: None,
                mcp_server: None,
                plugin_id: None,
            });

        let command: Option<Vec<String>> = payload
            .get("command")
            .and_then(|c| serde_json::from_value(c.clone()).ok())
            .map(redact_command);
        let exit_code = payload
            .get("exit_code")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32);
        let cwd = payload
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let duration_secs = parse_duration(payload);
        // aggregated_output carries the actual command output; stdout is often empty.
        // formatted_output was a legacy field removed in Codex v0.132.0 (PR #22706).
        let output = ["aggregated_output", "stdout"].iter().find_map(|key| {
            payload
                .get(*key)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        });
        let status = str_field(payload, "status");
        // Codex v0.145.0: may include a truncation indicator when the runtime bounded
        // command output to fix an unbounded-exec bug (bytes_truncated, truncated, or
        // output_truncated). Parse it so the UI can warn users that output is partial.
        let output_truncated = parse_output_truncated(payload);

        let (subagent_id, subagent_name) = extract_subagent_identity(payload);

        // Codex v0.146.0: exec_command_end may carry plugin_id/script_path when the
        // command was attributed to a trusted plugin script (issue #223's "hardened
        // plugin isolation" hardening). Absent for non-plugin-attributed commands.
        let plugin_id = payload
            .get("plugin_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let script_path = payload
            .get("script_path")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        // When function_call_output already finalized an ExecCommand for this call_id
        // (v0.132.0+ sessions where both entries appear), backfill the structured
        // metadata from exec_command_end rather than creating a duplicate entry.
        // exec_command_end carries authoritative exit_code, duration, command, cwd, and
        // status; function_call_output carries the full raw output text.
        if let Some(existing) = self
            .finalized
            .iter_mut()
            .find(|tc| tc.call_id == call_id && tc.kind == ToolKind::ExecCommand)
        {
            existing.exit_code = exit_code;
            existing.duration_secs = duration_secs;
            if command.is_some() {
                existing.command = command;
            }
            if cwd.is_some() {
                existing.cwd = cwd;
            }
            existing.status = status;
            existing.subagent_id = subagent_id;
            existing.subagent_name = subagent_name;
            existing.output_truncated = output_truncated;
            if plugin_id.is_some() {
                existing.plugin_id = plugin_id;
            }
            if script_path.is_some() {
                existing.script_path = script_path;
            }
            // Prefer existing output from function_call_output (full raw text); fall back
            // to aggregated_output only if no output was captured yet.
            if existing.output.is_none() {
                existing.output = output;
            }
            return;
        }

        self.finalized.push(ToolCall {
            call_id,
            kind: ToolKind::ExecCommand,
            name: pending.name,
            arguments: pending.arguments,
            input_text: pending.input_text,
            output,
            exit_code,
            command,
            cwd,
            duration_secs,
            mcp_server: None,
            mcp_tool: None,
            plugin_id,
            script_path,
            patch_success: None,
            patch_changes: None,
            web_query: None,
            web_url: None,
            image_prompt: None,
            image_file_path: None,
            worker_session: None,
            status,
            subagent_id,
            subagent_name,
            output_truncated,
        });
    }

    /// Finalize with mcp_tool_call_end event.
    pub fn finalize_mcp(&mut self, event_type: &str, payload: &Value) {
        let call_id = str_field(payload, "call_id");
        let pending = self
            .pending
            .remove(&call_id)
            .unwrap_or_else(|| PendingCall {
                name: kind_name(event_type),
                arguments: Value::Null,
                input_text: None,
                namespace: None,
                mcp_server: None,
                plugin_id: None,
            });

        // Extract server + tool from invocation field, then namespace, then name.
        // namespace format: "mcp__<server>" (no trailing __, no tool name).
        // v0.141.0+: also handles "mcp:server/tool" qualified name format.
        let (mcp_server, mcp_tool) = if let Some(inv) = payload.get("invocation") {
            let server = inv
                .get("server")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let tool = inv
                .get("tool")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            (server, tool)
        } else if let Some(ns) = &pending.namespace {
            parse_mcp_namespace(ns, &pending.name)
        } else if let Some((tool_type, server, tool)) = parse_namespaced_tool_name(&pending.name) {
            if tool_type == "mcp" {
                (Some(server), Some(tool))
            } else {
                parse_mcp_name(&pending.name)
            }
        } else {
            parse_mcp_name(&pending.name)
        };

        // Extract output text from result.Ok.content[].text, or result.Err/is_error
        // when the call failed (Codex v0.148.0 PR #38416 started returning fail-closed
        // permission errors here for app file uploads that previously bypassed the
        // sandbox silently).
        let (output, mcp_failed) = extract_mcp_result(payload);
        let duration_secs = parse_duration(payload);
        let plugin_id = pending.plugin_id;
        let (subagent_id, subagent_name) = extract_subagent_identity(payload);

        self.finalized.push(ToolCall {
            call_id,
            kind: ToolKind::McpTool,
            name: pending.name,
            arguments: pending.arguments,
            input_text: pending.input_text,
            output,
            exit_code: None,
            command: None,
            cwd: None,
            duration_secs,
            mcp_server,
            mcp_tool,
            plugin_id,
            script_path: None,
            patch_success: None,
            patch_changes: None,
            web_query: None,
            web_url: None,
            image_prompt: None,
            image_file_path: None,
            worker_session: None,
            status: if mcp_failed {
                "failed".to_string()
            } else {
                "completed".to_string()
            },
            subagent_id,
            subagent_name,
            output_truncated: None,
        });
    }

    /// Backfill patch_success and patch_changes onto an already-finalized PatchApply call.
    /// Used for Codex v0.129.0+ where file changes arrive via an apply_patch_end turn item
    /// (PR #20540) after custom_tool_call_output has already finalized the call. Also used
    /// when patch_apply_end event arrives late in sessions where both event and turn-item
    /// paths coexist (PR #20463 made ApplyPatchEnd explicitly stored in limited history mode).
    pub fn backfill_patch_result(
        &mut self,
        call_id: &str,
        success: Option<bool>,
        changes: Option<Value>,
    ) {
        if let Some(tc) = self
            .finalized
            .iter_mut()
            .find(|tc| tc.call_id == call_id && tc.kind == ToolKind::PatchApply)
        {
            if success.is_some() {
                tc.patch_success = success;
            }
            if changes.is_some() {
                tc.patch_changes = changes;
            }
        }
    }

    /// Finalize with patch_apply_end event.
    pub fn finalize_patch(&mut self, event_type: &str, payload: &Value) {
        let call_id = str_field(payload, "call_id");

        let patch_success = payload.get("success").and_then(|v| v.as_bool());
        let patch_changes = payload.get("changes").cloned();
        let stdout = payload
            .get("stdout")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Codex v0.129.0 (PR #20463) now explicitly stores ApplyPatchEnd in limited history
        // mode, so a patch_apply_end event_msg may arrive for a call that was already
        // finalized by custom_tool_call_output. Backfill rather than create a duplicate.
        if self
            .finalized
            .iter()
            .any(|tc| tc.call_id == call_id && tc.kind == ToolKind::PatchApply)
        {
            self.backfill_patch_result(&call_id, patch_success, patch_changes);
            return;
        }

        let pending = self
            .pending
            .remove(&call_id)
            .unwrap_or_else(|| PendingCall {
                name: kind_name(event_type),
                arguments: Value::Null,
                input_text: None,
                namespace: None,
                mcp_server: None,
                plugin_id: None,
            });

        let (subagent_id, subagent_name) = extract_subagent_identity(payload);
        self.finalized.push(ToolCall {
            call_id,
            kind: ToolKind::PatchApply,
            name: pending.name,
            arguments: pending.arguments,
            input_text: pending.input_text,
            output: stdout,
            exit_code: None,
            command: None,
            cwd: None,
            duration_secs: None,
            mcp_server: None,
            mcp_tool: None,
            plugin_id: None,
            script_path: None,
            patch_success,
            patch_changes,
            web_query: None,
            web_url: None,
            image_prompt: None,
            image_file_path: None,
            worker_session: None,
            status: str_field(payload, "status"),
            subagent_id,
            subagent_name,
            output_truncated: None,
        });
    }

    /// Finalize with collab_agent_spawn_end event.
    pub fn finalize_spawn(&mut self, event_type: &str, payload: &Value) {
        let call_id = str_field(payload, "call_id");
        let pending = self.pending.remove(&call_id);
        let pending_name = pending.as_ref().map(|p| p.name.clone());
        let (subagent_id, subagent_name) = extract_subagent_identity(payload);

        self.finalized.push(ToolCall {
            call_id,
            kind: ToolKind::SpawnAgent,
            name: pending_name.unwrap_or_else(|| kind_name(event_type)),
            arguments: payload.clone(),
            input_text: None,
            output: None,
            exit_code: None,
            command: None,
            cwd: None,
            duration_secs: None,
            mcp_server: None,
            mcp_tool: None,
            plugin_id: None,
            script_path: None,
            patch_success: None,
            patch_changes: None,
            web_query: None,
            web_url: None,
            image_prompt: None,
            image_file_path: None,
            worker_session: None,
            status: str_field(payload, "status"),
            subagent_id,
            subagent_name,
            output_truncated: None,
        });
    }

    /// Finalize with collab_waiting_end event.
    pub fn finalize_wait(&mut self, event_type: &str, payload: &Value) {
        let call_id = str_field(payload, "call_id");
        let pending = self.pending.remove(&call_id);
        let pending_name = pending.as_ref().map(|p| p.name.clone());
        let (subagent_id, subagent_name) = extract_subagent_identity(payload);

        self.finalized.push(ToolCall {
            call_id,
            kind: ToolKind::WaitAgent,
            name: pending_name.unwrap_or_else(|| kind_name(event_type)),
            arguments: payload.clone(),
            input_text: None,
            output: None,
            exit_code: None,
            command: None,
            cwd: None,
            duration_secs: None,
            mcp_server: None,
            mcp_tool: None,
            plugin_id: None,
            script_path: None,
            patch_success: None,
            patch_changes: None,
            web_query: None,
            web_url: None,
            image_prompt: None,
            image_file_path: None,
            worker_session: None,
            status: "completed".to_string(),
            subagent_id,
            subagent_name,
            output_truncated: None,
        });
    }

    /// Finalize with collab_close_end event.
    pub fn finalize_close(&mut self, event_type: &str, payload: &Value) {
        let call_id = str_field(payload, "call_id");
        let pending = self.pending.remove(&call_id);
        let pending_name = pending.as_ref().map(|p| p.name.clone());
        let (subagent_id, subagent_name) = extract_subagent_identity(payload);

        self.finalized.push(ToolCall {
            call_id,
            kind: ToolKind::InterruptAgent,
            name: pending_name.unwrap_or_else(|| kind_name(event_type)),
            arguments: payload.clone(),
            input_text: None,
            output: None,
            exit_code: None,
            command: None,
            cwd: None,
            duration_secs: None,
            mcp_server: None,
            mcp_tool: None,
            plugin_id: None,
            script_path: None,
            patch_success: None,
            patch_changes: None,
            web_query: None,
            web_url: None,
            image_prompt: None,
            image_file_path: None,
            worker_session: None,
            status: "completed".to_string(),
            subagent_id,
            subagent_name,
            output_truncated: None,
        });
    }

    /// Finalize web_search (no call_id pairing — best-effort).
    pub fn add_web_search(&mut self, event_type: &str, payload: &Value) {
        let call_id = str_field(payload, "call_id");
        let pending = self.pending.remove(&call_id);
        let pending_name = pending.as_ref().map(|p| p.name.clone());
        let query = payload
            .get("query")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let web_url = payload
            .get("action")
            .and_then(|a| a.get("url"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let (subagent_id, subagent_name) = extract_subagent_identity(payload);

        self.finalized.push(ToolCall {
            call_id,
            kind: ToolKind::WebSearch,
            name: pending_name.unwrap_or_else(|| kind_name(event_type)),
            arguments: payload.clone(),
            input_text: None,
            output: None,
            exit_code: None,
            command: None,
            cwd: None,
            duration_secs: None,
            mcp_server: None,
            mcp_tool: None,
            plugin_id: None,
            script_path: None,
            patch_success: None,
            patch_changes: None,
            web_query: query,
            web_url,
            image_prompt: None,
            image_file_path: None,
            worker_session: None,
            status: "completed".to_string(),
            subagent_id,
            subagent_name,
            output_truncated: None,
        });
    }

    /// Finalize a shell_hook_output event (Codex v0.136.0, PR #24962).
    ///
    /// The v0.136.0 tightened schema requires: call_id, hook_type, stdout, exit_code.
    /// Fields that were previously null (metadata, stderr) are now absent — read only
    /// the stable fields so older null-padded payloads also parse correctly.
    pub fn finalize_shell_hook(&mut self, payload: &Value) {
        let call_id = str_field(payload, "call_id");
        let hook_type = str_field(payload, "hook_type");
        let stdout = payload
            .get("stdout")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let exit_code = payload
            .get("exit_code")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32);
        let duration_secs = parse_duration(payload);
        let status = if exit_code.map(|c| c != 0).unwrap_or(false) {
            "failed"
        } else {
            "completed"
        }
        .to_string();
        let (subagent_id, subagent_name) = extract_subagent_identity(payload);

        self.finalized.push(ToolCall {
            call_id,
            kind: ToolKind::ShellHook,
            name: hook_type,
            arguments: Value::Object(serde_json::Map::new()),
            input_text: None,
            output: stdout,
            exit_code,
            command: None,
            cwd: None,
            duration_secs,
            mcp_server: None,
            mcp_tool: None,
            plugin_id: None,
            script_path: None,
            patch_success: None,
            patch_changes: None,
            web_query: None,
            web_url: None,
            image_prompt: None,
            image_file_path: None,
            worker_session: None,
            status,
            subagent_id,
            subagent_name,
            output_truncated: None,
        });
    }

    /// Catch-all for any unrecognised *_end event — preserves name from pending.
    pub fn finalize_unknown_end(&mut self, event_type: &str, payload: &Value) {
        let call_id = str_field(payload, "call_id");
        // Codex v0.143.0 emits non-tool-call terminal rollout events (e.g.
        // `transcript_flush_end`) that end in `_end` but carry no `call_id`
        // and have no matching pending call. Emitting a phantom ToolCall for
        // them corrupts the turn's tool list, so skip them.
        let pending = match self.pending.remove(&call_id) {
            Some(p) => p,
            None if call_id.is_empty() => return,
            None => PendingCall {
                name: kind_name(event_type),
                arguments: Value::Null,
                input_text: None,
                namespace: None,
                mcp_server: None,
                plugin_id: None,
            },
        };
        let output = ["output", "aggregated_output", "stdout"]
            .iter()
            .find_map(|key| {
                payload
                    .get(*key)
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            });
        let (subagent_id, subagent_name) = extract_subagent_identity(payload);
        self.finalized.push(ToolCall {
            call_id,
            kind: ToolKind::Unknown,
            name: pending.name,
            arguments: pending.arguments,
            input_text: pending.input_text,
            output,
            exit_code: None,
            command: None,
            cwd: None,
            duration_secs: parse_duration(payload),
            mcp_server: None,
            mcp_tool: None,
            plugin_id: None,
            script_path: None,
            patch_success: None,
            patch_changes: None,
            web_query: None,
            web_url: None,
            image_prompt: None,
            image_file_path: None,
            worker_session: None,
            status: str_field(payload, "status"),
            subagent_id,
            subagent_name,
            output_truncated: None,
        });
    }

    /// Drain any remaining pending calls as Unknown (no end event arrived).
    pub fn drain_pending(&mut self) {
        let pending: Vec<(String, PendingCall)> = self.pending.drain().collect();
        for (call_id, p) in pending {
            self.finalized.push(ToolCall {
                call_id,
                kind: ToolKind::Unknown,
                name: p.name,
                arguments: p.arguments,
                input_text: p.input_text,
                output: None,
                exit_code: None,
                command: None,
                cwd: None,
                duration_secs: None,
                mcp_server: None,
                mcp_tool: None,
                plugin_id: None,
                script_path: None,
                patch_success: None,
                patch_changes: None,
                web_query: None,
                web_url: None,
                image_prompt: None,
                image_file_path: None,
                worker_session: None,
                status: "unknown".to_string(),
                subagent_id: None,
                subagent_name: None,
                output_truncated: None,
            });
        }
        // Remove Unknown entries that share a call_id with a properly classified end-event entry.
        // This happens when function_call_output arrives before exec_command_end for the same
        // call_id — the output is finalized as Unknown first, then the end event adds the real entry.
        let paired: HashSet<String> = self
            .finalized
            .iter()
            .filter(|tc| tc.kind != ToolKind::Unknown)
            .map(|tc| tc.call_id.clone())
            .collect();
        self.finalized
            .retain(|tc| tc.kind != ToolKind::Unknown || !paired.contains(&tc.call_id));
    }
}

#[derive(Debug, Clone, Default)]
struct ExecFunctionOutput {
    output: Option<String>,
    exit_code: Option<i32>,
    duration_secs: Option<f64>,
    running_session_id: Option<String>,
    status: String,
}

fn exec_tool_call_from_pending(
    call_id: String,
    pending: PendingCall,
    parsed_output: ExecFunctionOutput,
) -> ToolCall {
    let command = command_from_arguments(&pending.arguments);
    let cwd = cwd_from_arguments(&pending.arguments);

    ToolCall {
        call_id,
        kind: ToolKind::ExecCommand,
        name: pending.name,
        arguments: pending.arguments,
        input_text: pending.input_text,
        output: parsed_output.output,
        exit_code: parsed_output.exit_code,
        command,
        cwd,
        duration_secs: parsed_output.duration_secs,
        mcp_server: None,
        mcp_tool: None,
        // v0.133.0+ (PRs #23353, #23737): tool_id.plugin_id is read generically for any
        // function_call, including exec_command (issue #223). No exec_command_end event
        // exists on this path, so script_path (only carried on that event) stays None.
        plugin_id: pending.plugin_id,
        script_path: None,
        patch_success: None,
        patch_changes: None,
        web_query: None,
        web_url: None,
        image_prompt: None,
        image_file_path: None,
        worker_session: None,
        status: parsed_output.status,
        subagent_id: None,
        subagent_name: None,
        output_truncated: None,
    }
}

fn spawn_agent_status(output: &str) -> String {
    if parse_spawn_agent_output(output).is_some() {
        "completed"
    } else if output.trim().is_empty() {
        "unknown"
    } else {
        "failed"
    }
    .to_string()
}

fn command_from_arguments(arguments: &Value) -> Option<Vec<String>> {
    if let Some(cmd) = arguments.get("cmd").and_then(|v| v.as_str()) {
        return Some(redact_command(vec![cmd.to_string()]));
    }
    arguments
        .get("command")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .map(redact_command)
}

/// Codex v0.147.0 (#36893, #36908) redacts secrets from displayed commands, but only at its
/// app-server-protocol display layer — the raw command persisted to the rollout JSONL is
/// unaffected. Apply the same redaction here so codex-trace, which reads that raw JSONL
/// directly, doesn't surface secrets that Codex's own clients now hide.
fn redact_command(command: Vec<String>) -> Vec<String> {
    command.into_iter().map(|s| redact_secrets(&s)).collect()
}

fn cwd_from_arguments(arguments: &Value) -> Option<String> {
    ["workdir", "cwd"].iter().find_map(|key| {
        arguments
            .get(*key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    })
}

fn session_id_from_arguments(arguments: &Value) -> Option<String> {
    arguments
        .get("session_id")
        .and_then(|v| {
            v.as_str()
                .map(|s| s.to_string())
                .or_else(|| v.as_i64().map(|n| n.to_string()))
        })
        .filter(|s| !s.is_empty())
}

fn parse_exec_function_output(output: &str) -> ExecFunctionOutput {
    let tool_output = display_output(output);

    // Exit code, duration, and running session id extraction only apply to the old
    // SDK-format output that contains an "Output:" marker line. v0.132.0 (PR #22706)
    // switched function_call_output to plain raw text; v0.133.0 (PR #23564) preserves
    // exec output in full without truncation, so large blobs now routinely contain
    // phrases such as "exit code" or "wall time" inside actual command output (compiler
    // errors, benchmarks, test runners). Scanning raw content for these patterns without
    // the marker guard produces false-positive exit codes and durations.
    // `likely_running_output` is intentionally kept active for all formats: it enables
    // write_stdin PTY output merging when the output text signals a running process,
    // even when the old SDK "Output:" header is absent.
    let has_output_marker = payload_after_output_marker(output).is_some();
    let (exit_code, duration_secs, running_session_id) = if has_output_marker {
        (
            parse_process_exit_code(output),
            parse_wall_time(output),
            parse_running_session_id(output),
        )
    } else {
        (None, None, None)
    };

    let status = if exit_code.map(|code| code != 0).unwrap_or(false) {
        "failed"
    } else if running_session_id.is_some() || likely_running_output(output, exit_code) {
        "running"
    } else {
        "completed"
    }
    .to_string();

    ExecFunctionOutput {
        output: tool_output,
        exit_code,
        duration_secs,
        running_session_id,
        status,
    }
}

fn display_output(output: &str) -> Option<String> {
    if output.is_empty() {
        return None;
    }

    Some(
        payload_after_output_marker(output)
            .filter(|payload| !payload.is_empty())
            .unwrap_or(output)
            .to_string(),
    )
}

fn payload_after_output_marker(output: &str) -> Option<&str> {
    let mut offset = 0;
    for line in output.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        if line.trim_end_matches('\n').trim_end_matches('\r').trim() == "Output:" {
            return Some(&output[offset..]);
        }
        if line_start == output.len() {
            break;
        }
    }
    None
}

fn parse_wall_time(output: &str) -> Option<f64> {
    output.lines().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        if lower.contains("wall") && lower.contains("time") {
            parse_first_f64(line)
        } else {
            None
        }
    })
}

fn parse_process_exit_code(output: &str) -> Option<i32> {
    output.lines().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        if lower.contains("exit") && lower.contains("code") {
            parse_first_i32(line)
        } else {
            None
        }
    })
}

fn parse_running_session_id(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        let marker = "session id";
        let marker_index = lower.find(marker)?;
        let after_marker = &line[marker_index + marker.len()..];
        let id: String = after_marker
            .chars()
            .skip_while(|c| c.is_whitespace() || matches!(c, ':' | '='))
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            .collect();
        if id.is_empty() {
            None
        } else {
            Some(id)
        }
    })
}

fn likely_running_output(output: &str, exit_code: Option<i32>) -> bool {
    exit_code.is_none() && output.to_ascii_lowercase().contains("running")
}

fn parse_first_f64(text: &str) -> Option<f64> {
    let mut number = String::new();
    let mut started = false;
    for c in text.chars() {
        if c.is_ascii_digit() || (started && c == '.') {
            number.push(c);
            started = true;
        } else if started {
            break;
        }
    }
    number.parse().ok()
}

fn parse_first_i32(text: &str) -> Option<i32> {
    let mut number = String::new();
    let mut started = false;
    for c in text.chars() {
        if c.is_ascii_digit() || (!started && c == '-') {
            number.push(c);
            started = true;
        } else if started {
            break;
        }
    }
    number.parse().ok()
}

fn append_output(current: &mut Option<String>, next: Option<String>) {
    let Some(next) = next else {
        return;
    };

    match current {
        Some(current) if !current.is_empty() => {
            if !current.ends_with('\n') && !next.starts_with('\n') {
                current.push('\n');
            }
            current.push_str(&next);
        }
        _ => *current = Some(next),
    }
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

/// Extract subagent identity from a tool-call end-event payload.
///
/// Added in Codex v0.134.0 (PR #22882): `subagent_id` and `subagent_name` are now
/// injected into PostToolUse hook input payloads and logged as part of tool call end
/// events, enabling per-tool multi-agent attribution in the parent session's JSONL.
fn extract_subagent_identity(payload: &Value) -> (Option<String>, Option<String>) {
    let id = payload
        .get("subagent_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let name = payload
        .get("subagent_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    (id, name)
}

/// Reconstruct MCP server + tool from the `namespace` field and function `name`.
///
/// OpenAI encodes MCP tools as: namespace = `mcp__<server>__[ns_suffix]`, name = `[_suffix]`.
/// `server` = full namespace without `mcp__` prefix (e.g. `codex_apps__github`).
/// `tool`   = reconstructed full tool name (ns_suffix concatenated with name).
///
/// Examples:
///   namespace="mcp__codex_apps__github", name="_get_pr_info"
///     → server="codex_apps__github", tool="github_get_pr_info"
///   namespace="mcp__computer_use__", name="screenshot"
///     → server="computer_use", tool="screenshot"
fn parse_mcp_namespace(namespace: &str, name: &str) -> (Option<String>, Option<String>) {
    let after_mcp = match namespace.strip_prefix("mcp__") {
        Some(s) => s,
        None => return (None, None),
    };
    // Use the full namespace segment (minus mcp__ and any trailing __) as the server identifier.
    let server = after_mcp.trim_end_matches("__");
    if server.is_empty() {
        return (None, None);
    }
    // Reconstruct full tool name: ns_suffix (after first __) concatenated with name.
    let full_tool = if let Some((_, ns_suffix)) = after_mcp.split_once("__") {
        format!("{ns_suffix}{name}")
    } else {
        name.to_string()
    };
    (Some(server.to_string()), Some(full_tool))
}

fn kind_name(event_type: &str) -> String {
    event_type
        .strip_suffix("_end")
        .unwrap_or(event_type)
        .to_string()
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn parse_duration(v: &Value) -> Option<f64> {
    let dur = v.get("duration")?;
    let secs = dur.get("secs")?.as_f64()?;
    let nanos = dur.get("nanos").and_then(|n| n.as_f64()).unwrap_or(0.0);
    Some(secs + nanos / 1_000_000_000.0)
}

fn parse_mcp_name(name: &str) -> (Option<String>, Option<String>) {
    let parts: Vec<&str> = name.split("__").collect();
    if parts.len() >= 3 && parts[0] == "mcp" {
        (Some(parts[1].to_string()), Some(parts[2..].join("__")))
    } else {
        (Some("codex".to_string()), Some(name.to_string()))
    }
}

/// Parse a namespace-qualified tool name from Codex v0.141.0+ format: "type:server/tool_name".
/// Returns (tool_type, server, tool_name) or None if the name is not in this format.
///
/// Examples:
///   "mcp:my-server/my_tool"       → ("mcp", "my-server", "my_tool")
///   "connector:plugin-1/do_thing" → ("connector", "plugin-1", "do_thing")
fn parse_namespaced_tool_name(name: &str) -> Option<(String, String, String)> {
    let (tool_type, rest) = name.split_once(':')?;
    let (server, tool) = rest.split_once('/')?;
    if tool_type.is_empty() || server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((tool_type.to_string(), server.to_string(), tool.to_string()))
}

/// Extract an MCP tool call's output text and whether it failed.
///
/// `mcp_tool_call_end.result` is `Result<CallToolResult, String>` on the wire:
/// `{"Ok": {"content": [...], "is_error": bool}}` on a normal response, or
/// `{"Err": "<message>"}` when the call could not be completed at all (e.g. Codex
/// v0.148.0 PR #38416 rejecting an app file upload denied by the permission
/// profile). `is_error: true` inside `Ok` signals a tool-reported failure rather
/// than a transport error. Both must be treated as failed — previously only the
/// `Ok` content text was read and the call was always marked "completed",
/// silently hiding sandbox-denial and other tool errors.
fn extract_mcp_result(payload: &Value) -> (Option<String>, bool) {
    let Some(result) = payload.get("result") else {
        return (None, false);
    };

    if let Some(err) = result.get("Err").and_then(|v| v.as_str()) {
        return (Some(err.to_string()), true);
    }

    let Some(ok) = result.get("Ok") else {
        return (None, false);
    };
    let is_error = ok
        .get("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let output = ok
        .get("content")
        .and_then(|c| c.as_array())
        .map(|content| {
            content
                .iter()
                .filter(|item| item.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|s| !s.is_empty());
    (output, is_error)
}

/// Detect whether an exec_command_end payload signals truncated output.
///
/// Codex v0.145.0 introduced bounded exec output to fix an unbounded-exec bug.
/// When the runtime clips command output it may set one of three fields:
/// - `truncated` (bool true) — primary indicator added in v0.145.0
/// - `output_truncated` (bool true) — alternative spelling used in some builds
/// - `bytes_truncated` (number > 0) — byte count of dropped output
///
/// Returns `Some(true)` when any indicator is present and signals truncation,
/// `Some(false)` when an indicator is explicitly present but false/zero (not
/// truncated), and `None` when no indicator field exists (pre-v0.145.0 sessions
/// or exec calls that completed without hitting the output limit).
fn parse_output_truncated(payload: &Value) -> Option<bool> {
    if let Some(v) = payload.get("truncated").and_then(|v| v.as_bool()) {
        return Some(v);
    }
    if let Some(v) = payload.get("output_truncated").and_then(|v| v.as_bool()) {
        return Some(v);
    }
    if let Some(n) = payload.get("bytes_truncated").and_then(|v| v.as_u64()) {
        return Some(n > 0);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{parse_exec_function_output, parse_mcp_namespace, ToolCallBuilder, ToolKind};
    use serde_json::json;

    #[test]
    fn namespace_with_tool_prefix_keeps_full_namespace_as_server() {
        // namespace="mcp__codex_apps__github", name="_get_pr_info"
        // server = "codex_apps__github" (full namespace without mcp__)
        // tool   = "github_get_pr_info" (ns_suffix + name)
        let (server, tool) = parse_mcp_namespace("mcp__codex_apps__github", "_get_pr_info");
        assert_eq!(server.as_deref(), Some("codex_apps__github"));
        assert_eq!(tool.as_deref(), Some("github_get_pr_info"));
    }

    #[test]
    fn namespace_with_trailing_double_underscore() {
        // namespace="mcp__computer_use__", name="screenshot"
        // trailing __ is trimmed → server="computer_use", tool="screenshot"
        let (server, tool) = parse_mcp_namespace("mcp__computer_use__", "screenshot");
        assert_eq!(server.as_deref(), Some("computer_use"));
        assert_eq!(tool.as_deref(), Some("screenshot"));
    }

    #[test]
    fn namespace_without_trailing_separator() {
        // namespace="mcp__my_server", name="do_thing"
        let (server, tool) = parse_mcp_namespace("mcp__my_server", "do_thing");
        assert_eq!(server.as_deref(), Some("my_server"));
        assert_eq!(tool.as_deref(), Some("do_thing"));
    }

    #[test]
    fn non_mcp_namespace_returns_none() {
        let (server, tool) = parse_mcp_namespace("other__ns__tool", "fn_name");
        assert_eq!(server, None);
        assert_eq!(tool, None);
    }

    // Codex v0.132.0 (PR #22706): the legacy shell output formatting paths were removed.
    // exec_command_end events no longer carry a `formatted_output` field; the output is
    // exclusively in `aggregated_output`. The parser must read `aggregated_output` and
    // must not require `formatted_output` to be present.
    #[test]
    fn exec_command_end_v0132_reads_aggregated_output_without_formatted_output() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_1".to_string(),
            "exec_command".to_string(),
            r#"{"cmd":"echo hello","workdir":"/tmp"}"#,
            None,
            None,
            None, // plugin_id
        );

        // v0.132.0 exec_command_end: only aggregated_output, no formatted_output
        let payload = json!({
            "call_id": "call_1",
            "aggregated_output": "hello\n",
            "exit_code": 0,
            "status": "completed",
            "duration": {"secs": 0, "nanos": 120_000_000u64}
        });
        builder.finalize_exec("exec_command_end", &payload);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.output.as_deref(), Some("hello\n"));
        assert_eq!(tool.exit_code, Some(0));
        assert_eq!(tool.status, "completed");
        assert!(
            tool.duration_secs.is_some(),
            "duration should be extracted from structured field"
        );
    }

    // Codex v0.139.0 (PR #26994): multi-agent v2 close_agent renamed to interrupt_agent.
    // Both old transcripts (close_agent) and new (interrupt_agent) must classify as InterruptAgent.

    #[test]
    fn close_agent_legacy_classified_as_interrupt_agent() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_close".to_string(),
            "close_agent".to_string(),
            r#"{"reason":"task complete"}"#,
            None,
            None,
            None,
        );
        builder.add_function_call_output("call_close", r#"{"status":"ok"}"#, None);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::InterruptAgent);
        assert_eq!(tool.name, "close_agent");
        assert_eq!(tool.status, "completed");
    }

    #[test]
    fn interrupt_agent_new_name_classified_as_interrupt_agent() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_interrupt".to_string(),
            "interrupt_agent".to_string(),
            r#"{"reason":"task complete"}"#,
            None,
            None,
            None,
        );
        builder.add_function_call_output("call_interrupt", r#"{"status":"ok"}"#, None);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::InterruptAgent);
        assert_eq!(tool.name, "interrupt_agent");
        assert_eq!(tool.status, "completed");
    }

    // Codex v0.136.0 (PR #25267) renamed the multi-agent v2 assignment tool from
    // `assign_task` to `followup_task` (v0.137.0, PR #25636). Both names must be
    // classified as FollowupTask so sessions from all versions display correctly.
    #[test]
    fn assign_task_legacy_classified_as_followup_task() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_assign".to_string(),
            "assign_task".to_string(),
            r#"{"message":"Please investigate the regression","agent":"worker-1"}"#,
            None,
            None,
            None,
        );
        builder.add_function_call_output("call_assign", r#"{"status":"accepted"}"#, None);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::FollowupTask);
        assert_eq!(tool.name, "assign_task");
        assert_eq!(tool.status, "completed");
    }

    #[test]
    fn followup_task_new_name_classified_as_followup_task() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_followup".to_string(),
            "followup_task".to_string(),
            r#"{"message":"Continue the analysis","agent":"worker-2"}"#,
            None,
            None,
            None,
        );
        builder.add_function_call_output("call_followup", r#"{"status":"accepted"}"#, None);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::FollowupTask);
        assert_eq!(tool.name, "followup_task");
        assert_eq!(tool.status, "completed");
    }

    // Codex v0.133.0 (PR #23564): exec output is now preserved in raw form (no truncation).
    // function_call_output for exec_command may therefore be a large, unstructured blob
    // containing phrases such as "exit code" or "wall time" as part of actual command output
    // (compiler errors, benchmarks, test results). The parser must not extract false-positive
    // metadata from such blobs — metadata only comes from the structured exec_command_end event.

    #[test]
    fn v0133_raw_exec_output_with_exit_code_in_content_does_not_false_positive() {
        // Compiler-style output that mentions "exit code" inline — should NOT be parsed as
        // the process exit code since there is no "Output:" structured-format marker.
        let output = "error[E0505]: process exited with exit code 1\ncompilation failed\n";
        let result = parse_exec_function_output(output);
        assert_eq!(
            result.exit_code, None,
            "raw output with 'exit code' in content must not yield a false-positive exit_code"
        );
        assert_eq!(result.status, "completed");
        assert_eq!(result.output.as_deref(), Some(output));
    }

    #[test]
    fn v0133_raw_exec_output_with_wall_time_in_content_does_not_false_positive() {
        // Benchmark output with "wall time" in the text — must not be parsed as duration.
        let output = "benchmark result: wall time 2.5 seconds per iteration\ntest passed\n";
        let result = parse_exec_function_output(output);
        assert_eq!(
            result.duration_secs, None,
            "raw output with 'wall time' in content must not yield a false-positive duration"
        );
        assert_eq!(result.status, "completed");
    }

    #[test]
    fn v0133_raw_exec_output_with_session_id_in_content_is_not_marked_running() {
        // Web app log output with "session id" — must not mark exec call as running.
        let output = "2026-05-21 10:00:00 INFO  Starting server with session id abc-123\nListening on :8080\n";
        let result = parse_exec_function_output(output);
        assert_eq!(
            result.running_session_id, None,
            "raw output with 'session id' must not yield a false-positive running_session_id"
        );
        assert_eq!(result.status, "completed");
    }

    #[test]
    fn v0133_raw_exec_output_large_blob_metadata_none() {
        // Simulate a large raw output (multiple lines, realistic content) that contains
        // "exit code" and "wall time" false-positive patterns without the "Output:" marker.
        // Neither field should be extracted; status defaults to "completed".
        // Note: output intentionally contains no "running" keyword to avoid triggering
        // `likely_running_output`, which is intentionally kept active for PTY streaming
        // detection even in unmarked output.
        let output = concat!(
            "PASS src/auth.test.js\n",
            "FAIL src/billing.test.js\n",
            "  ● billing › returns exit code 1 on invalid input\n",
            "  wall time exceeded threshold: 3.5s > 2.0s\n",
            "  session id: test-run-7f8a2b\n",
            "Test Suites: 1 failed, 1 passed\n",
        );
        let result = parse_exec_function_output(output);
        assert_eq!(result.exit_code, None);
        assert_eq!(result.duration_secs, None);
        assert_eq!(result.running_session_id, None);
        assert_eq!(result.status, "completed");
        assert_eq!(result.output.as_deref(), Some(output));
    }

    #[test]
    fn old_format_with_output_marker_still_parses_metadata_correctly() {
        // The "Output:" marker guard must not break old-format sessions that legitimately
        // carry metadata in the SDK header or banner.
        let sdk_format = "Chunk ID: abc123\nWall time: 0.2500 seconds\nProcess exited with code 0\nOriginal token count: 1\nOutput:\nhello\n";
        let result = parse_exec_function_output(sdk_format);
        assert_eq!(result.exit_code, Some(0));
        assert!(
            result.duration_secs.is_some(),
            "duration should be extracted from SDK header"
        );
        assert_eq!(result.status, "completed");
        assert_eq!(result.output.as_deref(), Some("hello\n"));
    }

    #[test]
    fn exec_output_parsing_unaffected_by_codex_v0_130_0_banner_change() {
        // Codex v0.130.0 (PR #21683) removed "research preview" from the `codex exec`
        // startup banner. codex-trace must not pattern-match on banner wording — only
        // stable structural markers (Output:, exit code, wall time, session id) are used.
        let old_banner = "Codex - a coding agent (research preview)\nOutput:\nhello\nExit code: 0\nWall time: 0.5s\n";
        let new_banner = "Codex - a coding agent\nOutput:\nhello\nExit code: 0\nWall time: 0.5s\n";

        let old = parse_exec_function_output(old_banner);
        let new = parse_exec_function_output(new_banner);

        assert_eq!(
            old.status, new.status,
            "status differs between banner formats"
        );
        assert_eq!(old.exit_code, new.exit_code);
        assert_eq!(old.duration_secs, new.duration_secs);
        let old_out = old.output.unwrap_or_default();
        let new_out = new.output.unwrap_or_default();
        assert!(old_out.contains("hello"), "old banner output missing hello");
        assert!(new_out.contains("hello"), "new banner output missing hello");
        assert!(
            !old_out.contains("research preview"),
            "banner text leaked into output"
        );
    }

    // Codex v0.134.0 (PR #22882): subagent identity fields added to hook input payloads.
    // exec_command_end, mcp_tool_call_end, and other end events now optionally carry
    // `subagent_id` and `subagent_name` so that tool calls can be attributed to the
    // subagent that executed them in multi-agent sessions.
    //
    // The parser must:
    //   1. Expose the fields on ToolCall when present in the end event payload.
    //   2. Propagate them from function_call (PendingCall) when present there instead.
    //   3. Default both to None for pre-v0.134.0 sessions and parent-agent tool calls.

    #[test]
    fn v0134_exec_command_end_with_subagent_identity_populates_tool_call() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_exec_sub".to_string(),
            "exec_command".to_string(),
            r#"{"cmd":"echo subagent","workdir":"/tmp"}"#,
            None,
            None,
            None, // plugin_id
        );

        // v0.134.0 exec_command_end: carries subagent identity
        let payload = json!({
            "call_id": "call_exec_sub",
            "aggregated_output": "subagent\n",
            "exit_code": 0,
            "status": "completed",
            "duration": {"secs": 0, "nanos": 10_000_000u64},
            "subagent_id": "worker-session-abc",
            "subagent_name": "Parfit"
        });
        builder.finalize_exec("exec_command_end", &payload);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.output.as_deref(), Some("subagent\n"));
        assert_eq!(tool.exit_code, Some(0));
        assert_eq!(tool.subagent_id.as_deref(), Some("worker-session-abc"));
        assert_eq!(tool.subagent_name.as_deref(), Some("Parfit"));
    }

    #[test]
    fn v0134_mcp_tool_call_end_with_subagent_identity_populates_tool_call() {
        // Codex v0.134.0 (PR #22882): subagent_id/subagent_name are present on PostToolUse
        // hook input data, which is included in mcp_tool_call_end event payloads. The parser
        // must extract these fields from the end event and populate ToolCall accordingly.
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_mcp_sub".to_string(),
            "get_pr".to_string(),
            r#"{}"#,
            Some("mcp__github".to_string()),
            None,
            None, // plugin_id
        );

        // mcp_tool_call_end carries subagent identity in PostToolUse hook data
        let payload = json!({
            "call_id": "call_mcp_sub",
            "result": {"Ok": {"content": [{"type": "text", "text": "PR info"}]}},
            "subagent_id": "worker-session-xyz",
            "subagent_name": "Noether"
        });
        builder.finalize_mcp("mcp_tool_call_end", &payload);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.subagent_id.as_deref(), Some("worker-session-xyz"));
        assert_eq!(tool.subagent_name.as_deref(), Some("Noether"));
        assert_eq!(tool.output.as_deref(), Some("PR info"));
    }

    #[test]
    fn v0148_mcp_tool_call_end_with_err_result_is_marked_failed() {
        // Codex v0.148.0 (PR #38416, issue #237): app file uploads through the MCP
        // layer now honor the configured permission profile instead of bypassing it,
        // so a denied upload surfaces as `result: {"Err": "<message>"}` on
        // mcp_tool_call_end. Previously extract_mcp_output only read result.Ok and
        // finalize_mcp unconditionally set status to "completed", so a denial like
        // this showed up as a successful call with no output at all.
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_mcp_denied".to_string(),
            "extract_text".to_string(),
            r#"{}"#,
            Some("mcp__calendar".to_string()),
            None,
            None,
        );

        let payload = json!({
            "call_id": "call_mcp_denied",
            "result": {"Err": "failed to upload `report.txt`: Permission denied (os error 13)"},
        });
        builder.finalize_mcp("mcp_tool_call_end", &payload);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.status, "failed");
        assert_eq!(
            tool.output.as_deref(),
            Some("failed to upload `report.txt`: Permission denied (os error 13)")
        );
    }

    #[test]
    fn v0148_mcp_tool_call_end_with_ok_is_error_is_marked_failed() {
        // An MCP server can also report a tool-level failure inside a successful
        // transport response (`result.Ok.is_error: true`). This must be treated the
        // same as an Err result rather than silently reported as "completed".
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_mcp_tool_err".to_string(),
            "extract_text".to_string(),
            r#"{}"#,
            Some("mcp__calendar".to_string()),
            None,
            None,
        );

        let payload = json!({
            "call_id": "call_mcp_tool_err",
            "result": {"Ok": {"is_error": true, "content": [{"type": "text", "text": "denied: sandbox blocked read of private.txt"}]}},
        });
        builder.finalize_mcp("mcp_tool_call_end", &payload);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.status, "failed");
        assert_eq!(
            tool.output.as_deref(),
            Some("denied: sandbox blocked read of private.txt")
        );
    }

    #[test]
    fn v0134_absent_subagent_fields_default_to_none() {
        // Pre-v0.134.0 sessions and parent-agent tool calls must have None for both
        // subagent fields — existing logic must be backward-compatible.
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_no_sub".to_string(),
            "exec_command".to_string(),
            r#"{"cmd":"ls","workdir":"/tmp"}"#,
            None,
            None,
            None, // plugin_id
        );

        let payload = json!({
            "call_id": "call_no_sub",
            "aggregated_output": "file.txt\n",
            "exit_code": 0,
            "status": "completed",
            "duration": {"secs": 0, "nanos": 5_000_000u64}
        });
        builder.finalize_exec("exec_command_end", &payload);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        // Both fields must be None for pre-v0.134.0 sessions.
        assert!(
            tool.subagent_id.is_none(),
            "subagent_id must be None when absent"
        );
        assert!(
            tool.subagent_name.is_none(),
            "subagent_name must be None when absent"
        );
    }

    // Codex v0.136.0 (PR #24962): shell hook output events with the tightened schema.
    // The v0.136.0 schema enforces: call_id, hook_type, stdout, exit_code.
    // Fields previously present as null (metadata, stderr) are now absent entirely.

    #[test]
    fn shell_hook_output_v0136_pre_exec_classified_as_shell_hook() {
        let mut builder = ToolCallBuilder::new();
        // v0.136.0 strict schema: no metadata or stderr fields
        let payload = json!({
            "call_id": "hook-call-1",
            "hook_type": "pre_exec",
            "stdout": "hook ran ok\n",
            "exit_code": 0,
            "duration": {"secs": 0, "nanos": 5_000_000u64}
        });
        builder.finalize_shell_hook(&payload);

        assert_eq!(builder.finalized.len(), 1);
        let tc = &builder.finalized[0];
        assert_eq!(tc.kind, ToolKind::ShellHook);
        assert_eq!(tc.call_id, "hook-call-1");
        assert_eq!(tc.name, "pre_exec");
        assert_eq!(tc.output.as_deref(), Some("hook ran ok\n"));
        assert_eq!(tc.exit_code, Some(0));
        assert_eq!(tc.status, "completed");
        assert!(tc.duration_secs.is_some());
    }

    #[test]
    fn shell_hook_output_v0136_post_exec_failed_hook() {
        let mut builder = ToolCallBuilder::new();
        let payload = json!({
            "call_id": "hook-call-2",
            "hook_type": "post_exec",
            "stdout": "hook failed with error\n",
            "exit_code": 1,
            "duration": {"secs": 0, "nanos": 2_000_000u64}
        });
        builder.finalize_shell_hook(&payload);

        assert_eq!(builder.finalized.len(), 1);
        let tc = &builder.finalized[0];
        assert_eq!(tc.kind, ToolKind::ShellHook);
        assert_eq!(tc.name, "post_exec");
        assert_eq!(tc.exit_code, Some(1));
        assert_eq!(tc.status, "failed");
    }

    #[test]
    fn shell_hook_output_v0136_absent_fields_not_null() {
        // v0.136.0 tightening: previously-null fields (metadata, stderr) are now absent.
        // Verify finalize_shell_hook does not panic when those fields are absent.
        let mut builder = ToolCallBuilder::new();
        let payload = json!({
            "call_id": "hook-call-3",
            "hook_type": "pre_mcp",
            "stdout": "",
            "exit_code": 0
            // no duration, no metadata, no stderr — strict v0.136.0 schema
        });
        builder.finalize_shell_hook(&payload);

        assert_eq!(builder.finalized.len(), 1);
        let tc = &builder.finalized[0];
        assert_eq!(tc.kind, ToolKind::ShellHook);
        assert_eq!(tc.name, "pre_mcp");
        assert!(tc.output.is_none()); // empty stdout → None
        assert!(tc.duration_secs.is_none());
        assert_eq!(tc.status, "completed");
    }

    // Codex v0.150.0 (PR #40511): new `Interrupt` variant added to the HookEventName enum.
    // hook_type is read as a free-form string (no fixed/exhaustive match against known hook
    // names), so new and future hook event names classify as ShellHook the same way existing
    // ones do — nothing to special-case.

    #[test]
    fn shell_hook_output_v0150_interrupt_hook_classified_as_shell_hook() {
        let mut builder = ToolCallBuilder::new();
        let payload = json!({
            "call_id": "hook-interrupt-1",
            "hook_type": "interrupt",
            "stdout": "turn interrupted\n",
            "exit_code": 0
        });
        builder.finalize_shell_hook(&payload);

        assert_eq!(builder.finalized.len(), 1);
        let tc = &builder.finalized[0];
        assert_eq!(tc.kind, ToolKind::ShellHook);
        assert_eq!(tc.name, "interrupt");
        assert_eq!(tc.output.as_deref(), Some("turn interrupted\n"));
        assert_eq!(tc.status, "completed");
    }

    #[test]
    fn shell_hook_output_unrecognized_future_hook_name_still_classified_as_shell_hook() {
        // No hook_type value is matched exhaustively, so a name codex-trace has never seen
        // before must still classify as ShellHook rather than being dropped or erroring.
        let mut builder = ToolCallBuilder::new();
        let payload = json!({
            "call_id": "hook-future-1",
            "hook_type": "some_future_hook_name",
            "stdout": "ran\n",
            "exit_code": 0
        });
        builder.finalize_shell_hook(&payload);

        assert_eq!(builder.finalized.len(), 1);
        let tc = &builder.finalized[0];
        assert_eq!(tc.kind, ToolKind::ShellHook);
        assert_eq!(tc.name, "some_future_hook_name");
    }

    // Codex v0.135.0 (PR #24652): plain image wrapper spans removed from session output.
    // Image content is now emitted bare (e.g. {"type":"image_url",...}) rather than wrapped
    // in {"type":"image_span","content":[...]}. The image_generation function call must be
    // classified as ImageGeneration with image_prompt extracted from arguments — never from
    // the output content, since image data is not stored in the text output field.

    #[test]
    fn image_generation_classified_as_image_generation_kind() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_img".to_string(),
            "image_generation".to_string(),
            r#"{"prompt":"a sunset over mountains","size":"1024x1024"}"#,
            None,
            None,
            None,
        );

        // v0.135.0+: output is a bare image_url item (no image_span wrapper).
        // The text extraction from the content array yields an empty string —
        // the prompt comes from arguments, not the output.
        builder.add_function_call_output("call_img", "", None);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::ImageGeneration);
        assert_eq!(
            tool.image_prompt.as_deref(),
            Some("a sunset over mountains")
        );
        assert_eq!(tool.status, "completed");
        assert!(
            tool.output.is_none(),
            "empty output string should yield None"
        );
    }

    #[test]
    fn image_generation_with_no_prompt_argument_yields_none_image_prompt() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_img2".to_string(),
            "image_generation".to_string(),
            r#"{"size":"512x512"}"#,
            None,
            None,
            None,
        );
        builder.add_function_call_output("call_img2", "", None);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::ImageGeneration);
        assert!(tool.image_prompt.is_none());
    }

    #[test]
    fn image_generation_with_non_empty_output_preserves_output() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_img3".to_string(),
            "image_generation".to_string(),
            r#"{"prompt":"a mountain lake"}"#,
            None,
            None,
            None,
        );
        // If upstream text extraction yields something (future format), preserve it.
        builder.add_function_call_output("call_img3", "Generated image successfully", None);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::ImageGeneration);
        assert_eq!(tool.image_prompt.as_deref(), Some("a mountain lake"));
        assert_eq!(tool.output.as_deref(), Some("Generated image successfully"));
    }

    // Codex v0.138.0 (PRs #25944, #25947): local image attachments and standalone image
    // generations now expose their saved file paths. The file_path is a top-level field in
    // the function_call_output payload alongside call_id and output.

    #[test]
    fn v0138_image_generation_with_file_path_stores_image_file_path() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_img_v138".to_string(),
            "image_generation".to_string(),
            r#"{"prompt":"a sunset over mountains","size":"1024x1024"}"#,
            None,
            None,
            None,
        );
        // v0.138.0: file_path present alongside the image output
        builder.add_function_call_output(
            "call_img_v138",
            "",
            Some("/home/user/.codex/images/sunset_abc123.png"),
        );

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::ImageGeneration);
        assert_eq!(
            tool.image_prompt.as_deref(),
            Some("a sunset over mountains")
        );
        assert_eq!(
            tool.image_file_path.as_deref(),
            Some("/home/user/.codex/images/sunset_abc123.png")
        );
        assert_eq!(tool.status, "completed");
    }

    #[test]
    fn v0138_image_generation_without_file_path_yields_none() {
        // Pre-v0.138.0 sessions must parse normally with image_file_path as None.
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_img_old".to_string(),
            "image_generation".to_string(),
            r#"{"prompt":"a mountain lake"}"#,
            None,
            None,
            None,
        );
        builder.add_function_call_output("call_img_old", "", None);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::ImageGeneration);
        assert_eq!(tool.image_prompt.as_deref(), Some("a mountain lake"));
        assert!(
            tool.image_file_path.is_none(),
            "image_file_path must be None when file_path is absent (pre-v0.138.0)"
        );
    }

    #[test]
    fn v0138_non_image_tool_ignores_file_path_field() {
        // file_path on a non-image function_call_output must not crash and must not
        // affect non-image tool calls — the field is only meaningful for image_generation.
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_exec_v138".to_string(),
            "exec_command".to_string(),
            r#"{"cmd":"echo hi","workdir":"/tmp"}"#,
            None,
            None,
            None,
        );
        // The function_call_output for exec_command is processed via finalize_exec, not
        // add_function_call_output, so file_path passed here is silently ignored.
        builder.add_function_call_output("call_exec_v138", "hi\n", Some("/tmp/some_file.txt"));

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::ExecCommand);
        assert!(tool.image_file_path.is_none());
    }

    // Codex v0.133.0 (PR #23564): code-mode exec output is now preserved raw unless an
    // explicit output token limit is requested. function_call_output entries for exec_command
    // now carry the raw command output with no "Output:" preamble or metadata footer.
    // The parser must:
    //   1. Preserve the full raw output without truncation.
    //   2. Not extract exit_code from text like "exit code" that appears in raw content.
    //   3. Not extract duration_secs from "wall time" text that appears in raw content.
    //   4. Not set status to "running" from "running" text in raw output content.
    //   5. Continue to work correctly for structured (marker-bearing) output.

    #[test]
    fn v0133_raw_exec_output_preserved_in_full_without_truncation() {
        let raw_output = "line 1\nline 2\nline 3\n".repeat(100);
        let result = parse_exec_function_output(&raw_output);
        assert_eq!(
            result.output.as_deref(),
            Some(raw_output.as_str()),
            "full raw output must be preserved"
        );
        assert!(result.exit_code.is_none());
        assert!(result.duration_secs.is_none());
        assert_eq!(result.status, "completed");
    }

    #[test]
    fn v0133_raw_exec_output_exit_code_text_not_false_positive() {
        // "exit code" appears in raw command output — must not be parsed as a metadata exit code.
        let output = "Test suite finished.\nFinal exit code: 1 was expected but got 0\n";
        let result = parse_exec_function_output(output);
        assert!(
            result.exit_code.is_none(),
            "exit code phrase in raw content must not be extracted"
        );
        assert_eq!(result.status, "completed");
    }

    #[test]
    fn v0133_raw_exec_output_wall_time_text_not_false_positive() {
        // "wall time" appears in raw command output — must not be parsed as a duration.
        let output = "Benchmark result: wall time 10.5 seconds\nBenchmark complete\n";
        let result = parse_exec_function_output(output);
        assert!(
            result.duration_secs.is_none(),
            "wall time phrase in raw content must not be extracted"
        );
    }

    #[test]
    fn v0133_exec_command_via_function_call_output_raw_output_preserved() {
        // Codex v0.133.0 (PR #23564): function_call_output for exec_command now carries raw
        // output without the "Output:" preamble or metadata footer. The full output must be
        // stored on ToolCall.output and metadata must not be falsely extracted from content.
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_raw".to_string(),
            "exec_command".to_string(),
            r#"{"cmd":"cargo build","workdir":"/project"}"#,
            None,
            None,
            None,
        );

        // v0.133.0 raw output — no "Output:" marker. Content includes phrases that would
        // falsely match metadata patterns if the parser scanned the full output text.
        let raw_output = "   Compiling my-crate v1.0.0\n   Compiling dep-crate v2.3.1\nwarning: unused import\nFinished with exit code: 0\n";
        builder.add_function_call_output("call_raw", raw_output, None);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::ExecCommand);
        assert_eq!(
            tool.output.as_deref(),
            Some(raw_output),
            "full raw output must be preserved"
        );
        assert!(
            tool.exit_code.is_none(),
            "\"exit code\" in raw content must not set exit_code"
        );
        assert_eq!(
            tool.status, "completed",
            "status must be completed for raw output without failure signal"
        );
    }

    // Codex v0.140.0 (PRs #27438, #27488, #27518): three new built-in context-budget tools.
    // token_budget_context, context_remaining, and context_window appear in session transcripts
    // as function_call / function_call_output pairs. They must be classified as ContextQuery,
    // not Unknown, and must not panic or produce parse errors.

    #[test]
    fn v0140_token_budget_context_classified_as_context_query() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_budget".to_string(),
            "token_budget_context".to_string(),
            r#"{}"#,
            None,
            None,
            None,
        );
        builder.add_function_call_output(
            "call_budget",
            r#"{"tokens_used":12345,"tokens_remaining":87655}"#,
            None,
        );

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::ContextQuery);
        assert_eq!(tool.name, "token_budget_context");
        assert_eq!(tool.status, "completed");
        assert!(tool.output.is_some());
    }

    #[test]
    fn v0140_context_remaining_classified_as_context_query() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_ctx".to_string(),
            "context_remaining".to_string(),
            r#"{}"#,
            None,
            None,
            None,
        );
        builder.add_function_call_output("call_ctx", r#"{"remaining":50000}"#, None);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::ContextQuery);
        assert_eq!(tool.name, "context_remaining");
        assert_eq!(tool.status, "completed");
    }

    #[test]
    fn v0140_context_window_classified_as_context_query() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_win".to_string(),
            "context_window".to_string(),
            r#"{}"#,
            None,
            None,
            None,
        );
        builder.add_function_call_output("call_win", r#"{"window_size":128000}"#, None);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::ContextQuery);
        assert_eq!(tool.name, "context_window");
        assert_eq!(tool.status, "completed");
    }

    #[test]
    fn v0140_context_query_with_empty_output_yields_none() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_empty".to_string(),
            "token_budget_context".to_string(),
            r#"{}"#,
            None,
            None,
            None,
        );
        builder.add_function_call_output("call_empty", "", None);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::ContextQuery);
        assert!(tool.output.is_none(), "empty output should yield None");
    }

    #[test]
    fn v0133_structured_exec_output_still_extracts_metadata() {
        // Regression guard: structured output (with "Output:" marker) must still have
        // exit_code and duration_secs extracted from the metadata footer after the marker.
        let structured =
            "Codex - a coding agent\nOutput:\nhello world\nExit code: 0\nWall time: 1.5s\n";
        let result = parse_exec_function_output(structured);
        assert_eq!(
            result.exit_code,
            Some(0),
            "exit_code must be extracted from structured output"
        );
        assert!(
            result.duration_secs.is_some(),
            "duration must be extracted from structured output"
        );
        assert_eq!(
            result.output.as_deref(),
            Some("hello world\nExit code: 0\nWall time: 1.5s\n")
        );
        assert_eq!(result.status, "completed");
    }

    // --- Codex v0.141.0+ dynamic tool namespace tests ---

    #[test]
    fn parse_namespaced_tool_name_mcp_format() {
        use super::parse_namespaced_tool_name;
        let result = parse_namespaced_tool_name("mcp:my-server/my_tool");
        assert_eq!(
            result,
            Some((
                "mcp".to_string(),
                "my-server".to_string(),
                "my_tool".to_string()
            ))
        );
    }

    #[test]
    fn parse_namespaced_tool_name_connector_format() {
        use super::parse_namespaced_tool_name;
        let result = parse_namespaced_tool_name("connector:plugin-1/do_thing");
        assert_eq!(
            result,
            Some((
                "connector".to_string(),
                "plugin-1".to_string(),
                "do_thing".to_string()
            ))
        );
    }

    #[test]
    fn parse_namespaced_tool_name_unqualified_returns_none() {
        use super::parse_namespaced_tool_name;
        assert_eq!(parse_namespaced_tool_name("my_tool"), None);
        assert_eq!(parse_namespaced_tool_name("exec_command"), None);
        assert_eq!(parse_namespaced_tool_name("mcp__my_server"), None);
    }

    #[test]
    fn qualified_mcp_name_in_function_call_classified_as_mcp_tool() {
        // v0.141.0+: function_call name is "mcp:server/tool_name" (no namespace field)
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_mcp_1".to_string(),
            "mcp:my-server/my_tool".to_string(),
            "{}",
            None,
            None,
            None,
        );
        builder.add_function_call_output("call_mcp_1", "result output", None);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::McpTool);
        assert_eq!(tool.mcp_server.as_deref(), Some("my-server"));
        assert_eq!(tool.mcp_tool.as_deref(), Some("my_tool"));
    }

    #[test]
    fn dynamic_tool_registry_classifies_unqualified_mcp_tool() {
        // v0.141.0+: registry from dynamic_tools in task_started lets codex-trace classify
        // MCP tools that arrive as unqualified names with no namespace field.
        let mut builder = ToolCallBuilder::new();
        let mut registry = std::collections::HashMap::new();
        registry.insert(
            "my_tool".to_string(),
            ("mcp".to_string(), "my-server".to_string()),
        );
        builder.set_dynamic_tool_registry(registry);

        builder.add_function_call(
            "call_mcp_2".to_string(),
            "my_tool".to_string(),
            "{}",
            None,
            None,
            None,
        );
        builder.add_function_call_output("call_mcp_2", "result output", None);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::McpTool);
        assert_eq!(tool.mcp_server.as_deref(), Some("my-server"));
        assert_eq!(tool.mcp_tool.as_deref(), Some("my_tool"));
    }

    #[test]
    fn dynamic_tool_registry_does_not_affect_builtin_tool_classification() {
        // Registry entries must not override built-in tool classifications.
        let mut builder = ToolCallBuilder::new();
        let mut registry = std::collections::HashMap::new();
        // Hypothetical conflict: someone registers exec_command as an MCP tool
        registry.insert(
            "exec_command".to_string(),
            ("mcp".to_string(), "some-server".to_string()),
        );
        builder.set_dynamic_tool_registry(registry);

        builder.add_function_call(
            "call_exec".to_string(),
            "exec_command".to_string(),
            r#"{"cmd":"echo hi","workdir":"/tmp"}"#,
            None,
            None,
            None,
        );
        let payload = json!({
            "call_id": "call_exec",
            "aggregated_output": "hi\n",
            "exit_code": 0,
            "status": "completed",
            "duration": {"secs": 0, "nanos": 10_000_000u64}
        });
        builder.finalize_exec("exec_command_end", &payload);

        assert_eq!(builder.finalized.len(), 1);
        // Built-in classification must win — exec_command via exec_command_end → ExecCommand
        assert_eq!(builder.finalized[0].kind, ToolKind::ExecCommand);
    }

    // Codex v0.145.0: bounded exec output — truncation indicator fields.
    // When the runtime clips command output it sets one of: `truncated` (bool),
    // `output_truncated` (bool), or `bytes_truncated` (number > 0) in the
    // exec_command_end payload. The parser must surface this as output_truncated
    // on the ToolCall so the UI can warn users that output is partial.

    #[test]
    fn v0145_exec_command_end_with_truncated_bool_sets_output_truncated() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_trunc_1".to_string(),
            "exec_command".to_string(),
            r#"{"cmd":"cat large_file.log","workdir":"/tmp"}"#,
            None,
            None,
            None,
        );

        let payload = json!({
            "call_id": "call_trunc_1",
            "aggregated_output": "line1\nline2\n...(clipped)",
            "exit_code": 0,
            "status": "completed",
            "truncated": true,
            "duration": {"secs": 1, "nanos": 0u64}
        });
        builder.finalize_exec("exec_command_end", &payload);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.output_truncated, Some(true));
        assert_eq!(tool.exit_code, Some(0));
    }

    #[test]
    fn v0145_exec_command_end_with_output_truncated_bool_sets_output_truncated() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_trunc_2".to_string(),
            "exec_command".to_string(),
            r#"{"cmd":"ls -la","workdir":"/tmp"}"#,
            None,
            None,
            None,
        );

        let payload = json!({
            "call_id": "call_trunc_2",
            "aggregated_output": "total 999\n...(clipped)",
            "exit_code": 0,
            "status": "completed",
            "output_truncated": true
        });
        builder.finalize_exec("exec_command_end", &payload);

        assert_eq!(builder.finalized.len(), 1);
        assert_eq!(builder.finalized[0].output_truncated, Some(true));
    }

    #[test]
    fn v0145_exec_command_end_with_bytes_truncated_nonzero_sets_output_truncated() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_trunc_3".to_string(),
            "exec_command".to_string(),
            r#"{"cmd":"find / -name '*.rs'","workdir":"/"}"#,
            None,
            None,
            None,
        );

        let payload = json!({
            "call_id": "call_trunc_3",
            "aggregated_output": "/src/main.rs\n...(clipped)",
            "exit_code": 0,
            "status": "completed",
            "bytes_truncated": 65536u64
        });
        builder.finalize_exec("exec_command_end", &payload);

        assert_eq!(builder.finalized.len(), 1);
        assert_eq!(builder.finalized[0].output_truncated, Some(true));
    }

    #[test]
    fn v0145_exec_command_end_bytes_truncated_zero_yields_some_false() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_trunc_4".to_string(),
            "exec_command".to_string(),
            r#"{"cmd":"echo hi","workdir":"/tmp"}"#,
            None,
            None,
            None,
        );

        // bytes_truncated = 0 means no bytes were dropped — not truncated
        let payload = json!({
            "call_id": "call_trunc_4",
            "aggregated_output": "hi\n",
            "exit_code": 0,
            "status": "completed",
            "bytes_truncated": 0u64
        });
        builder.finalize_exec("exec_command_end", &payload);

        assert_eq!(builder.finalized.len(), 1);
        assert_eq!(builder.finalized[0].output_truncated, Some(false));
    }

    #[test]
    fn pre_v0145_exec_command_end_without_truncation_field_yields_none() {
        // Pre-v0.145.0 sessions have no truncation indicator — output_truncated must be None.
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_no_trunc".to_string(),
            "exec_command".to_string(),
            r#"{"cmd":"echo hello","workdir":"/tmp"}"#,
            None,
            None,
            None,
        );

        let payload = json!({
            "call_id": "call_no_trunc",
            "aggregated_output": "hello\n",
            "exit_code": 0,
            "status": "completed",
            "duration": {"secs": 0, "nanos": 10_000_000u64}
        });
        builder.finalize_exec("exec_command_end", &payload);

        assert_eq!(builder.finalized.len(), 1);
        assert!(
            builder.finalized[0].output_truncated.is_none(),
            "output_truncated must be None when no truncation field is present"
        );
    }

    #[test]
    fn v0145_truncation_propagated_to_backfill_exec_entry() {
        // When function_call_output arrives before exec_command_end (v0.132.0+ sessions),
        // exec_command_end must backfill output_truncated into the existing entry.
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_backfill".to_string(),
            "exec_command".to_string(),
            r#"{"cmd":"cat huge.log","workdir":"/tmp"}"#,
            None,
            None,
            None,
        );

        // function_call_output arrives first (v0.132.0+ ordering)
        builder.add_function_call_output("call_backfill", "partial output...", None);

        // exec_command_end arrives second with truncation indicator
        let payload = json!({
            "call_id": "call_backfill",
            "aggregated_output": "partial output...",
            "exit_code": 0,
            "status": "completed",
            "truncated": true,
            "duration": {"secs": 2, "nanos": 0u64}
        });
        builder.finalize_exec("exec_command_end", &payload);

        // Should still have exactly one entry (backfilled, not duplicated)
        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::ExecCommand);
        assert_eq!(tool.output_truncated, Some(true));
        assert_eq!(tool.exit_code, Some(0));
    }

    // Codex v0.146.0 (issue #223): exec_command_end may carry plugin_id/script_path when
    // the command was attributed to a trusted plugin script ("hardened plugin isolation").

    #[test]
    fn exec_command_end_with_plugin_attribution_is_captured() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_plugin_exec".to_string(),
            "exec_command".to_string(),
            r#"{"cmd":"echo hi","workdir":"/tmp"}"#,
            None,
            None,
            None,
        );

        let payload = json!({
            "call_id": "call_plugin_exec",
            "aggregated_output": "hi\n",
            "exit_code": 0,
            "status": "completed",
            "duration": {"secs": 0, "nanos": 1_000_000u64},
            "plugin_id": "plugin-abc",
            "script_path": "scripts/run.py"
        });
        builder.finalize_exec("exec_command_end", &payload);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::ExecCommand);
        assert_eq!(tool.plugin_id.as_deref(), Some("plugin-abc"));
        assert_eq!(tool.script_path.as_deref(), Some("scripts/run.py"));
    }

    #[test]
    fn exec_command_end_without_plugin_attribution_has_none() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_no_plugin_exec".to_string(),
            "exec_command".to_string(),
            r#"{"cmd":"echo hi","workdir":"/tmp"}"#,
            None,
            None,
            None,
        );

        let payload = json!({
            "call_id": "call_no_plugin_exec",
            "aggregated_output": "hi\n",
            "exit_code": 0,
            "status": "completed",
            "duration": {"secs": 0, "nanos": 1_000_000u64}
        });
        builder.finalize_exec("exec_command_end", &payload);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert!(tool.plugin_id.is_none());
        assert!(tool.script_path.is_none());
    }

    #[test]
    fn v0146_plugin_attribution_propagated_to_backfill_exec_entry() {
        // When function_call_output arrives before exec_command_end (v0.132.0+ ordering),
        // exec_command_end must backfill plugin_id/script_path into the existing entry.
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_backfill_plugin".to_string(),
            "exec_command".to_string(),
            r#"{"cmd":"run.py","workdir":"/tmp"}"#,
            None,
            None,
            None,
        );

        builder.add_function_call_output("call_backfill_plugin", "done", None);

        let payload = json!({
            "call_id": "call_backfill_plugin",
            "aggregated_output": "done",
            "exit_code": 0,
            "status": "completed",
            "duration": {"secs": 0, "nanos": 0u64},
            "plugin_id": "plugin-xyz",
            "script_path": "scripts/run.py"
        });
        builder.finalize_exec("exec_command_end", &payload);

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::ExecCommand);
        assert_eq!(tool.plugin_id.as_deref(), Some("plugin-xyz"));
        assert_eq!(tool.script_path.as_deref(), Some("scripts/run.py"));
    }

    // Codex's Agent Plugins subsystem (issue #223): list_available_plugins_to_install and
    // request_plugin_install are built-in function tools for searching/installing plugin
    // and connector catalogs. They previously fell through to ToolKind::Unknown.

    #[test]
    fn list_available_plugins_to_install_is_classified_as_agent_plugin() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_list_plugins".to_string(),
            "list_available_plugins_to_install".to_string(),
            "{}",
            None,
            None,
            None,
        );

        builder.add_function_call_output(
            "call_list_plugins",
            r#"{"tools":[{"id":"sample@openai-curated","name":"Sample Plugin","description":null,"tool_type":"plugin","has_skills":false,"mcp_server_names":[],"app_connector_ids":[]}]}"#,
            None,
        );

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::AgentPlugin);
        assert_eq!(tool.name, "list_available_plugins_to_install");
    }

    #[test]
    fn request_plugin_install_is_classified_as_agent_plugin() {
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_request_install".to_string(),
            "request_plugin_install".to_string(),
            r#"{"tool_type":"plugin","action_type":"install","tool_id":"sample@openai-curated","suggest_reason":"Needed for calendar access"}"#,
            None,
            None,
            None,
        );

        builder.add_function_call_output(
            "call_request_install",
            r#"{"completed":true,"user_confirmed":true,"tool_type":"plugin","action_type":"install","tool_id":"sample@openai-curated","tool_name":"Sample Plugin","suggest_reason":"Needed for calendar access"}"#,
            None,
        );

        assert_eq!(builder.finalized.len(), 1);
        let tool = &builder.finalized[0];
        assert_eq!(tool.kind, ToolKind::AgentPlugin);
        assert_eq!(tool.name, "request_plugin_install");
        assert_eq!(
            tool.arguments.get("tool_id").and_then(|v| v.as_str()),
            Some("sample@openai-curated")
        );
    }

    // Codex v0.147.0 (#36893, #36908): Codex's own redaction only applies at its
    // app-server-protocol display layer, not to the raw command persisted in the rollout
    // JSONL. codex-trace must redact recognizable secrets itself when extracting `command`.
    #[test]
    fn exec_command_end_command_field_has_secrets_redacted() {
        let fixture_token = format!("{}{}", "abcdefghijklmnop", "01234567");
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_secret".to_string(),
            "exec_command".to_string(),
            r#"{"cmd":"echo hi","workdir":"/tmp"}"#,
            None,
            None,
            None,
        );

        let payload = json!({
            "call_id": "call_secret",
            "command": ["curl", "-H", format!("Authorization: Bearer {fixture_token}")],
            "aggregated_output": "",
            "exit_code": 0,
            "status": "completed"
        });
        builder.finalize_exec("exec_command_end", &payload);

        let tool = &builder.finalized[0];
        let command = tool.command.as_ref().expect("command should be present");
        assert!(
            !command.iter().any(|arg| arg.contains(&fixture_token)),
            "redacted command must not contain the raw secret"
        );
        assert!(command[2].contains("Bearer [REDACTED_SECRET]"));
    }

    #[test]
    fn function_call_output_cmd_argument_has_secrets_redacted() {
        let fixture_token = format!("{}{}", "sk-", "abcdefghijklmnopqrstuvwx");
        let mut builder = ToolCallBuilder::new();
        builder.add_function_call(
            "call_secret_arg".to_string(),
            "exec_command".to_string(),
            &format!(r#"{{"cmd":"export OPENAI_API_KEY={fixture_token}","workdir":"/tmp"}}"#),
            None,
            None,
            None,
        );

        builder.add_function_call_output("call_secret_arg", "done", None);

        let tool = &builder.finalized[0];
        let command = tool.command.as_ref().expect("command should be present");
        assert!(
            !command.iter().any(|arg| arg.contains(&fixture_token)),
            "redacted command must not contain the raw secret"
        );
        assert!(command[0].contains("[REDACTED_SECRET]"));
    }
}
