import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { AgentMessage, CodexToolCall, CodexTurn, TokenInfo } from "../../shared/types";
import { TurnDetail } from "./TurnDetail";

const TOKEN_INFO: TokenInfo = {
  input_tokens: 38_000,
  cached_input_tokens: 12_000,
  output_tokens: 2_000,
  reasoning_output_tokens: 500,
  total_tokens: 40_000,
  context_window_tokens: 26_000,
  model_context_window: 100_000,
};

const FINAL_MSG: AgentMessage = {
  text: "Done",
  phase: "final_answer",
  timestamp: "2026-04-26T10:01:00Z",
  is_reasoning: false,
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
    final_answer: "Done",
    total_tokens: TOKEN_INFO,
    model: "gpt-5.4",
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

function makeTool(overrides: Partial<CodexToolCall> = {}): CodexToolCall {
  return {
    call_id: "call-1",
    kind: "exec_command",
    name: "shell",
    arguments: {},
    input_text: null,
    output: "out",
    exit_code: 0,
    command: ["echo", "hi"],
    cwd: "/tmp",
    duration_secs: 0.1,
    mcp_server: null,
    mcp_tool: null,
    plugin_id: null,
    patch_success: null,
    patch_changes: null,
    web_query: null,
    web_url: null,
    image_prompt: null,
    worker_session: null,
    status: "completed",
    subagent_id: null,
    subagent_name: null,
    ...overrides,
  };
}

function renderTurnDetail(turn: CodexTurn) {
  render(<TurnDetail turn={turn} expanded={new Set()} onToggle={vi.fn()} onBack={vi.fn()} />);
}

describe("TurnDetail", () => {
  it("shows context-left metadata using Codex's last-token usage", () => {
    renderTurnDetail(makeTurn());

    expect(screen.getByText("ctx 84% left")).toBeInTheDocument();
    expect(document.querySelector(".info-bar__context-fill")).toHaveStyle({ width: "16%" });
    expect(screen.getByText("40.0k tok · 1m")).toBeInTheDocument();
  });

  it("omits context-left metadata when last-token usage is unavailable", () => {
    renderTurnDetail(
      makeTurn({
        total_tokens: {
          ...TOKEN_INFO,
          context_window_tokens: null,
        },
      }),
    );

    expect(screen.queryByText(/ctx .* left/)).not.toBeInTheDocument();
    expect(screen.getByText("40.0k tok · 1m")).toBeInTheDocument();
  });

  it("renders assistant commentary as an inline Complementary item without duplication", () => {
    const msg: AgentMessage = {
      text: "COMMENTARY_TEXT",
      phase: "commentary",
      timestamp: "2026-04-26T10:00:00Z",
      is_reasoning: false,
      order: 0,
    };
    const { container } = render(
      <TurnDetail
        turn={makeTurn({ agent_messages: [msg], tool_calls: [], final_answer: null })}
        expanded={new Set()}
        onToggle={vi.fn()}
        onBack={vi.fn()}
      />,
    );

    // Shown as a labelled Complementary item with its prose inline (no expansion needed)...
    expect(screen.getByText("Complementary")).toBeInTheDocument();
    expect(screen.getByText("COMMENTARY_TEXT")).toBeInTheDocument();
    // ...and exactly once — no duplicated flattened blob above the timeline.
    const occurrences = (container.textContent ?? "").split("COMMENTARY_TEXT").length - 1;
    expect(occurrences).toBe(1);
  });

  it("interleaves tool calls with commentary by stream order", () => {
    const first: AgentMessage = {
      text: "FIRST_MESSAGE",
      phase: "commentary",
      timestamp: "2026-04-26T10:00:00Z",
      is_reasoning: false,
      order: 0,
    };
    const second: AgentMessage = {
      text: "SECOND_MESSAGE",
      phase: "commentary",
      timestamp: "2026-04-26T10:00:02Z",
      is_reasoning: false,
      order: 2,
    };
    // Tool call's stream order (1) sits between the two messages (0 and 2), so it must render
    // between them — not after both.
    const tool = makeTool({ call_id: "c1", command: ["TOOL_MARKER_CMD"] });
    const { container } = render(
      <TurnDetail
        turn={makeTurn({
          agent_messages: [first, second],
          tool_calls: [tool],
          tool_call_orders: [1],
          final_answer: null,
        })}
        expanded={new Set([0])}
        onToggle={vi.fn()}
        onBack={vi.fn()}
      />,
    );

    const text = container.textContent ?? "";
    const iFirst = text.indexOf("FIRST_MESSAGE");
    const iTool = text.indexOf("TOOL_MARKER_CMD");
    const iSecond = text.indexOf("SECOND_MESSAGE");
    expect(iFirst).toBeGreaterThanOrEqual(0);
    expect(iTool).toBeGreaterThan(iFirst);
    expect(iSecond).toBeGreaterThan(iTool);
  });
});
