import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { AgentMessage, CodexToolCall, CodexTurn, TokenInfo } from "../../shared/types";
import { TurnList } from "./TurnList";

const TOKEN_INFO: TokenInfo = {
  input_tokens: 100,
  cached_input_tokens: 0,
  output_tokens: 50,
  reasoning_output_tokens: 0,
  total_tokens: 150,
  context_window_tokens: 150,
  model_context_window: 8000,
  rate_limits: null,
};

const FINAL_MSG: AgentMessage = {
  text: "Hi there!",
  phase: "final_answer",
  timestamp: "2026-04-26T10:01:00Z",
  is_reasoning: false,
};

const EXEC_TOOL: CodexToolCall = {
  call_id: "c1",
  kind: "exec_command",
  name: "shell",
  arguments: {},
  input_text: null,
  output: "ok",
  exit_code: 0,
  command: ["ls"],
  cwd: null,
  duration_secs: 0.1,
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
};

function makeTurn(overrides: Partial<CodexTurn> = {}): CodexTurn {
  return {
    turn_id: "turn-1",
    started_at: 1745661600,
    completed_at: 1745661660,
    duration_ms: 60000,
    status: "complete",
    user_message: "Hello Codex",
    agent_messages: [FINAL_MSG],
    tool_calls: [],
    final_answer: "Hi there!",
    total_tokens: TOKEN_INFO,
    model: "gpt-4",
    cwd: null,
    reasoning_effort: null,
    error: null,
    has_compaction: false,
    thread_name: null,
    collab_spawns: [],
    trace_id: null,
    forked_from_thread_id: null,
    compaction_meta: null,
    ...overrides,
  };
}

describe("TurnList", () => {
  it("shows empty state message when there are no turns", () => {
    render(<TurnList turns={[]} selectedIndex={-1} onSelectTurn={vi.fn()} />);
    expect(screen.getByText("No turns in this session.")).toBeInTheDocument();
  });

  it("renders the user message text", () => {
    render(<TurnList turns={[makeTurn()]} selectedIndex={-1} onSelectTurn={vi.fn()} />);
    expect(screen.getByText("Hello Codex")).toBeInTheDocument();
  });

  it("renders the agent final answer as preview", () => {
    render(<TurnList turns={[makeTurn()]} selectedIndex={-1} onSelectTurn={vi.fn()} />);
    expect(screen.getByText("Hi there!")).toBeInTheDocument();
  });

  it("renders final_answer as preview when no agent message was emitted", () => {
    render(
      <TurnList
        turns={[makeTurn({ agent_messages: [], final_answer: "Desktop output" })]}
        selectedIndex={-1}
        onSelectTurn={vi.fn()}
      />,
    );
    expect(screen.getByText("Desktop output")).toBeInTheDocument();
  });

  it("shows tool count for a single tool call", () => {
    render(
      <TurnList
        turns={[makeTurn({ tool_calls: [EXEC_TOOL] })]}
        selectedIndex={-1}
        onSelectTurn={vi.fn()}
      />,
    );
    expect(screen.getByText("1 tool")).toBeInTheDocument();
  });

  it("pluralises tool count for multiple tool calls", () => {
    const tool2 = { ...EXEC_TOOL, call_id: "c2" };
    render(
      <TurnList
        turns={[makeTurn({ tool_calls: [EXEC_TOOL, tool2] })]}
        selectedIndex={-1}
        onSelectTurn={vi.fn()}
      />,
    );
    expect(screen.getByText("2 tools")).toBeInTheDocument();
  });

  it("shows ongoing dot for an ongoing turn", () => {
    render(
      <TurnList
        turns={[makeTurn({ status: "ongoing", completed_at: null })]}
        selectedIndex={-1}
        onSelectTurn={vi.fn()}
      />,
    );
    expect(document.querySelector(".ongoing-dots")).toBeInTheDocument();
  });

  it("does not show ongoing dot for a completed turn", () => {
    render(<TurnList turns={[makeTurn()]} selectedIndex={-1} onSelectTurn={vi.fn()} />);
    expect(document.querySelector(".ongoing-dots")).not.toBeInTheDocument();
  });

  it("shows token stat when total_tokens is set", () => {
    render(<TurnList turns={[makeTurn()]} selectedIndex={-1} onSelectTurn={vi.fn()} />);
    expect(screen.getByText("150 tok")).toBeInTheDocument();
  });

  it("shows duration stat when duration_ms is set", () => {
    render(<TurnList turns={[makeTurn()]} selectedIndex={-1} onSelectTurn={vi.fn()} />);
    expect(screen.getByText("1m")).toBeInTheDocument();
  });

  it("calls onSelectTurn with the turn index when Detail button is clicked", () => {
    const onSelect = vi.fn();
    render(<TurnList turns={[makeTurn()]} selectedIndex={-1} onSelectTurn={onSelect} />);
    fireEvent.click(screen.getByText(/Detail/));
    expect(onSelect).toHaveBeenCalledWith(0);
  });

  it("applies selected class to the currently selected turn", () => {
    render(<TurnList turns={[makeTurn()]} selectedIndex={0} onSelectTurn={vi.fn()} />);
    const msgs = document.querySelectorAll(".message--selected");
    expect(msgs.length).toBeGreaterThan(0);
  });

  it("shows reasoning count when reasoning messages are present", () => {
    const reasoningMsg: AgentMessage = {
      text: "thinking...",
      phase: null,
      timestamp: "2026-04-26T10:00:30Z",
      is_reasoning: true,
    };
    render(
      <TurnList
        turns={[makeTurn({ agent_messages: [reasoningMsg, FINAL_MSG] })]}
        selectedIndex={-1}
        onSelectTurn={vi.fn()}
      />,
    );
    expect(screen.getByText("1 think")).toBeInTheDocument();
  });
});
