import { describe, expect, it } from "vitest";
import type { CodexSessionInfo } from "../../shared/types";
import { sessionDisplayName } from "./sessionDisplay";

function makeSession(overrides: Partial<CodexSessionInfo> = {}): CodexSessionInfo {
  return {
    id: "worker-session",
    path: "/sessions/2026/04/26/rollout-worker.jsonl",
    cwd: "/Users/user/myproject",
    git_branch: null,
    originator: null,
    model: null,
    cli_version: null,
    thread_name: null,
    turn_count: 1,
    start_time: "2026-04-26T10:00:00Z",
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
    date_group: "2026/04/26",
    ai_title: null,
    ...overrides,
  };
}

describe("sessionDisplayName", () => {
  it("uses normal thread names for parent sessions", () => {
    expect(sessionDisplayName(makeSession({ thread_name: "Parent Session" }))).toBe(
      "Parent Session",
    );
  });

  it("does not use inherited thread names for worker sessions", () => {
    expect(
      sessionDisplayName(
        makeSession({
          is_inline_worker: true,
          thread_name: "Parent Session",
          worker_nickname: "Parfit",
        }),
      ),
    ).toBe("Parfit (worker-s)");
  });

  it("falls back to role plus short id for workers without nickname", () => {
    expect(
      sessionDisplayName(
        makeSession({
          is_external_worker: true,
          thread_name: "Parent Session",
          worker_role: "review",
        }),
      ),
    ).toBe("review worker-s");
  });
});
