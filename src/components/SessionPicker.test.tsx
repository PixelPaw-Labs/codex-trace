import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { CodexSessionInfo } from "../../shared/types";
import { SessionPicker } from "./SessionPicker";

function makeSession(overrides: Partial<CodexSessionInfo> = {}): CodexSessionInfo {
  return {
    id: "session-a",
    path: "/sessions/2026/09/07/rollout-a.jsonl",
    cwd: "/work/codex-trace",
    git_branch: "main",
    originator: null,
    model: null,
    cli_version: null,
    thread_name: "Add session titles",
    turn_count: 1,
    start_time: "2026-09-07T10:00:00Z",
    end_time: null,
    total_tokens: null,
    is_ongoing: false,
    is_external_worker: false,
    is_inline_worker: false,
    worker_nickname: null,
    worker_role: null,
    spawned_worker_ids: [],
    date_group: "2026/09/07",
    ai_title: null,
    is_headless: false,
    is_archived: false,
    approval_mode: null,
    history_base_thread_id: null,
    forked_from_thread_id: null,
    mentioned_thread_ids: [],
    ...overrides,
  };
}

describe("SessionPicker", () => {
  it("groups sessions by cwd and displays each indexed title", () => {
    const sessions = [
      makeSession(),
      makeSession({
        id: "session-b",
        path: "/sessions/2026/09/06/rollout-b.jsonl",
        date_group: "2026/09/06",
        thread_name: "Fix remote session loading",
      }),
      makeSession({
        id: "session-c",
        path: "/sessions/2026/09/07/rollout-c.jsonl",
        cwd: "/work/another-project",
        thread_name: "Inspect another project",
      }),
    ];

    const { container } = render(
      <SessionPicker
        sessions={sessions}
        loading={false}
        searchQuery=""
        selectedIndex={0}
        sessionsDir="/Users/test/.codex/sessions"
        onSelectSession={vi.fn()}
        onSearchChange={vi.fn()}
      />,
    );

    expect(
      Array.from(container.querySelectorAll(".picker__group-header"), (node) => node.textContent),
    ).toEqual(["codex-trace", "another-project"]);
    expect(screen.getByText("Add session titles")).toBeInTheDocument();
    expect(screen.getByText("Fix remote session loading")).toBeInTheDocument();
    expect(screen.getByText("Inspect another project")).toBeInTheDocument();
  });
});
