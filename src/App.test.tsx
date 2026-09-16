import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { forwardRef, useImperativeHandle, type ReactNode, type Ref } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { CodexSession, CodexSessionInfo, CodexToolCall } from "../shared/types";

vi.mock("./lib/listen", () => ({
  listen: vi.fn(async () => () => {}),
}));

// react-virtuoso measures rows against real geometry, which jsdom does not
// provide, so the turn list would render nothing to click.
vi.mock("react-virtuoso", async () => {
  const actual = await vi.importActual<typeof import("react-virtuoso")>("react-virtuoso");
  return {
    ...actual,
    Virtuoso: forwardRef(function VirtuosoMock(
      {
        totalCount,
        itemContent,
        className,
        context,
      }: {
        totalCount: number;
        itemContent: (index: number, data: unknown, context: unknown) => ReactNode;
        className?: string;
        context?: unknown;
      },
      ref: Ref<unknown>,
    ) {
      useImperativeHandle(ref, () => ({ scrollToIndex: vi.fn() }));
      return (
        <div className={className}>
          {Array.from({ length: totalCount }, (_, i) => (
            <div key={i}>{itemContent(i, undefined, context)}</div>
          ))}
        </div>
      );
    }),
  };
});

const invoke = vi.fn();
vi.mock("./lib/invoke", () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
}));

const { App } = await import("./App");

function makeSpawnTool(overrides: Partial<CodexToolCall> = {}): CodexToolCall {
  return {
    call_id: "call-spawn",
    kind: "spawn_agent",
    name: "spawn_agent",
    arguments: { agent_type: "worker", message: "Do the thing" },
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

function makeSession(id: string, toolCalls: CodexToolCall[]): CodexSession {
  return {
    id,
    timestamp: "2026-08-20T10:00:00Z",
    cwd: "/project",
    originator: null,
    cli_version: null,
    model_provider: null,
    git: null,
    instructions: null,
    turns: [
      {
        turn_id: `${id}-turn`,
        started_at: null,
        completed_at: null,
        duration_ms: null,
        status: "complete",
        user_message: "Parent prompt",
        agent_messages: [],
        tool_calls: toolCalls,
        final_answer: "Parent final",
        total_tokens: null,
        model: null,
        cwd: "/project",
        reasoning_effort: null,
        error: null,
        has_compaction: false,
        thread_name: null,
        collab_spawns: [],
        trace_id: null,
        forked_from_thread_id: null,
        compaction_meta: null,
      },
    ],
    is_ongoing: false,
    total_tokens: null,
    thread_name: null,
    spawned_worker_ids: [],
    path: `/sessions/${id}.jsonl`,
    ai_title: null,
    is_headless: false,
    has_missing_spawn_metadata: false,
    is_archived: false,
    approval_mode: null,
    history_base_thread_id: null,
    forked_from_thread_id: null,
  };
}

function makeSessionInfo(): CodexSessionInfo {
  return {
    id: "parent",
    path: "/sessions/parent.jsonl",
    cwd: "/project",
    git_branch: "main",
    originator: null,
    model: null,
    cli_version: null,
    thread_name: null,
    turn_count: 1,
    start_time: "2026-08-20T10:00:00Z",
    end_time: null,
    total_tokens: null,
    is_ongoing: false,
    is_external_worker: false,
    is_inline_worker: false,
    is_headless: false,
    is_archived: false,
    approval_mode: null,
    history_base_thread_id: null,
    forked_from_thread_id: null,
    worker_nickname: null,
    worker_role: null,
    spawned_worker_ids: [],
    date_group: "2026/08/20",
    ai_title: null,
    mentioned_thread_ids: [],
  };
}

/** The list-view summary of a session's only turn. */
function summaryOf(session: CodexSession) {
  const turn = session.turns[0];
  return {
    turn_id: turn.turn_id,
    status: turn.status,
    started_at: turn.started_at,
    completed_at: turn.completed_at,
    duration_ms: turn.duration_ms,
    user_message: turn.user_message,
    agent_preview: turn.final_answer,
    last_agent_timestamp: null,
    tool_call_count: turn.tool_calls.length,
    reasoning_count: 0,
    total_tokens: null,
    model: turn.model,
    has_detail: turn.tool_calls.length > 0 || turn.agent_messages.length > 0,
  };
}

/** The worker panel's own close button — unique to the mounted panel. */
const PANEL = ".agent-panel__close";
/** The per-tool-call toggle inside the detail view's activity list. */
const TOGGLE = ".tool-call__worker-btn";
/** Opens the detail view for a turn directly (a plain click only expands it). */
const DETAIL_BTN = ".turn-list__turn .message__detail-btn";

/** Drive the app from the picker into the detail view of the only turn. */
async function openDetailView() {
  const { container } = render(<App />);
  const find = (sel: string) => container.querySelector(sel);

  // Scoped to the picker: the sidebar tree lists the same session name.
  await waitFor(() => expect(find(".picker__session")).not.toBeNull());
  fireEvent.click(find(".picker__session") as Element);

  await waitFor(() => expect(find(DETAIL_BTN)).not.toBeNull());
  fireEvent.click(find(DETAIL_BTN) as Element);

  await waitFor(() => expect(find(TOGGLE)).not.toBeNull());
  return { container, find };
}

describe("App worker panel", () => {
  beforeEach(() => {
    invoke.mockReset();
    const workerSession = makeSession("worker-session", []);
    const parentSession = makeSession("parent", [makeSpawnTool({ worker_session: workerSession })]);

    invoke.mockImplementation(async (cmd: string) => {
      switch (cmd) {
        case "get_settings":
          return {
            sessions_dir: "/sessions",
            default_dir: "/sessions",
            allowed_origins: [],
            api_auth_enabled: true,
            api_auth_source: "file",
            clients: [],
          };
        case "list_sessions":
          return {
            sessions: [makeSessionInfo()],
            total: 1,
            groups: [{ date_group: "2026/04/26", count: 1 }],
          };
        // The session load carries only the turn index; bodies arrive per turn.
        case "load_session":
          return {
            session: { ...parentSession, turns: [] },
            summaries: [summaryOf(parentSession)],
          };
        case "load_turn":
          return parentSession.turns[0];
        default:
          return undefined;
      }
    });
  });

  it("opens the worker panel from a spawn_agent tool call", async () => {
    const { find } = await openDetailView();
    expect(find(PANEL)).toBeNull();

    fireEvent.click(find(TOGGLE) as Element);

    await waitFor(() => expect(find(PANEL)).not.toBeNull());
  });

  // The panel is only meaningful inside the detail view for the selected turn, so
  // leaving detail must dismiss it — returning to the same turn shows it closed.
  it("dismisses the worker panel when leaving and re-entering the detail view", async () => {
    const { find } = await openDetailView();
    fireEvent.click(find(TOGGLE) as Element);
    await waitFor(() => expect(find(PANEL)).not.toBeNull());

    fireEvent.click(screen.getByRole("button", { name: /Back/ }));

    await waitFor(() => expect(find(DETAIL_BTN)).not.toBeNull());
    fireEvent.click(find(DETAIL_BTN) as Element);

    await waitFor(() => expect(find(TOGGLE)).not.toBeNull());
    expect(find(PANEL)).toBeNull();
  });

  it("closes the worker panel on Escape without leaving the detail view", async () => {
    const { find } = await openDetailView();
    fireEvent.click(find(TOGGLE) as Element);
    await waitFor(() => expect(find(PANEL)).not.toBeNull());

    fireEvent.keyDown(window, { key: "Escape" });

    await waitFor(() => expect(find(PANEL)).toBeNull());
    // Still in the detail view, not bounced back to the turn list.
    expect(find(TOGGLE)).not.toBeNull();
  });
});
