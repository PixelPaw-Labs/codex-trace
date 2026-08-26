import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { CodexSession, CodexToolCall } from "../../shared/types";
import { WorkerPanel, workerPanelTitle } from "./WorkerPanel";

function makeTool(overrides: Partial<CodexToolCall> = {}): CodexToolCall {
  return {
    call_id: "call-1",
    kind: "spawn_agent",
    name: "spawn_agent",
    arguments: {
      agent_type: "worker",
      message: "Inspect the worker content",
    },
    input_text: null,
    output: '{"agent_id":"worker-session","nickname":"Parfit"}',
    exit_code: null,
    command: null,
    cwd: null,
    duration_secs: 0.5,
    mcp_server: null,
    mcp_tool: null,
    plugin_id: null,
    script_path: null,
    patch_success: null,
    patch_changes: null,
    web_query: null,
    web_url: null,
    image_prompt: null,
    image_file_path: null,
    worker_session: null,
    status: "completed",
    subagent_id: null,
    subagent_name: null,
    output_truncated: null,
    ...overrides,
  };
}

function makeSession(toolCalls: CodexToolCall[]): CodexSession {
  return {
    id: "worker-session",
    timestamp: "2026-04-27T04:50:46Z",
    cwd: "/tmp/worker",
    originator: null,
    cli_version: null,
    model_provider: null,
    git: null,
    instructions: null,
    turns: [
      {
        turn_id: "worker-turn",
        started_at: null,
        completed_at: null,
        duration_ms: null,
        status: "complete",
        user_message: "Worker prompt",
        agent_messages: [
          {
            text: "Nested final",
            phase: "final_answer",
            timestamp: "2026-04-27T04:51:00Z",
            is_reasoning: false,
          },
        ],
        tool_calls: toolCalls,
        final_answer: "Nested final",
        total_tokens: null,
        model: null,
        cwd: "/tmp/worker",
        reasoning_effort: null,
        error: null,
        has_compaction: false,
        thread_name: "Same parent title",
        collab_spawns: [],
        trace_id: null,
        forked_from_thread_id: null,
        compaction_meta: null,
      },
    ],
    is_ongoing: false,
    total_tokens: null,
    thread_name: "Same parent title",
    spawned_worker_ids: [],
    path: "/tmp/worker.jsonl",
    ai_title: null,
    is_headless: false,
    has_missing_spawn_metadata: false,
    is_archived: false,
    approval_mode: null,
    history_base_thread_id: null,
    forked_from_thread_id: null,
  };
}

describe("WorkerPanel", () => {
  it("uses spawn metadata instead of inherited session thread name for the title", () => {
    const session = makeSession([]);
    expect(workerPanelTitle(makeTool(), session)).toBe("Parfit (worker-s)");
  });

  it("renders worker content in a panel", () => {
    render(
      <WorkerPanel
        session={makeSession([
          makeTool({
            call_id: "nested-tool",
            kind: "exec_command",
            name: "nested_shell",
            arguments: {},
            command: ["pwd"],
            output: "/tmp/worker",
            duration_secs: null,
          }),
        ])}
        sourceTool={makeTool()}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByText("Parfit (worker-s)")).toBeInTheDocument();
    expect(screen.getByText("Worker prompt")).toBeInTheDocument();
    expect(screen.getByText("Nested final")).toBeInTheDocument();
    expect(screen.getByText("nested_shell")).toBeInTheDocument();
  });

  it("updates panel content when a new worker tool call is appended", () => {
    const sourceTool = makeTool();
    const { rerender } = render(
      <WorkerPanel
        session={makeSession([
          makeTool({
            call_id: "child-1",
            kind: "exec_command",
            name: "first_nested_tool",
            arguments: {},
            command: ["echo", "first"],
            output: "first",
            duration_secs: null,
          }),
        ])}
        sourceTool={sourceTool}
        onClose={vi.fn()}
      />,
    );

    rerender(
      <WorkerPanel
        session={makeSession([
          makeTool({
            call_id: "child-1",
            kind: "exec_command",
            name: "first_nested_tool",
            arguments: {},
            command: ["echo", "first"],
            output: "first",
            duration_secs: null,
          }),
          makeTool({
            call_id: "child-2",
            kind: "exec_command",
            name: "appended_nested_tool",
            arguments: {},
            command: ["echo", "second"],
            output: "second",
            duration_secs: null,
          }),
        ])}
        sourceTool={sourceTool}
        onClose={vi.fn()}
      />,
    );

    expect(screen.getByText("first_nested_tool")).toBeInTheDocument();
    expect(screen.getByText("appended_nested_tool")).toBeInTheDocument();
  });
});
