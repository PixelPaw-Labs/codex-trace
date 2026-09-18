import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { CodexSessionInfo } from "../../shared/types";
import { SessionPicker, isNearEnd, LOAD_MORE_THRESHOLD_PX } from "./SessionPicker";

function makeSession(overrides: Partial<CodexSessionInfo> = {}): CodexSessionInfo {
  return {
    id: "abc123",
    path: "/sessions/2026/04/26/rollout-abc.jsonl",
    cwd: "/Users/user/myproject",
    git_branch: "main",
    originator: null,
    model: "gpt-4",
    cli_version: null,
    thread_name: null,
    turn_count: 3,
    start_time: "2026-04-26T10:00:00Z",
    end_time: null,
    total_tokens: null,
    is_ongoing: false,
    is_external_worker: false,
    is_inline_worker: false,
    parent_session_id: null,
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
    mentioned_thread_ids: [],
    ...overrides,
  };
}

function renderPicker(props: Partial<Parameters<typeof SessionPicker>[0]> = {}) {
  render(
    <SessionPicker
      sessions={[makeSession()]}
      index={{ files_read: 1, total_files: 1, bytes_read: 1, total_bytes: 1, done: true }}
      loading={false}
      searchQuery=""
      selectedIndex={-1}
      onSelectSession={vi.fn()}
      onSearchChange={vi.fn()}
      {...props}
    />,
  );
}

/** Give the list a real geometry, which jsdom does not provide on its own. */
function scrollList(scrollTop: number) {
  const list = document.querySelector(".picker__list") as HTMLElement;
  Object.defineProperty(list, "scrollHeight", { value: 5000, configurable: true });
  Object.defineProperty(list, "clientHeight", { value: 800, configurable: true });
  list.scrollTop = scrollTop;
  fireEvent.scroll(list);
}

describe("isNearEnd", () => {
  it("is true once the remaining scroll is within the threshold", () => {
    expect(isNearEnd({ scrollTop: 4000, scrollHeight: 5000, clientHeight: 800 })).toBe(true);
  });

  it("is false while there is still more than the threshold to go", () => {
    expect(isNearEnd({ scrollTop: 0, scrollHeight: 5000, clientHeight: 800 })).toBe(false);
  });

  it("is true for a list too short to scroll, so the next batch is still fetched", () => {
    // A batch that does not fill the viewport would otherwise strand the list: no scroll
    // is possible, so no scroll event ever arrives to ask for the rest.
    expect(isNearEnd({ scrollTop: 0, scrollHeight: 400, clientHeight: 800 })).toBe(true);
  });

  it("is exactly true at the threshold", () => {
    const scrollTop = 5000 - 800 - LOAD_MORE_THRESHOLD_PX;
    expect(isNearEnd({ scrollTop, scrollHeight: 5000, clientHeight: 800 })).toBe(true);
    expect(isNearEnd({ scrollTop: scrollTop - 1, scrollHeight: 5000, clientHeight: 800 })).toBe(
      false,
    );
  });
});

describe("SessionPicker batching", () => {
  it("asks for more when the list is scrolled near its end", () => {
    const onReachEnd = vi.fn();
    renderPicker({ hasMore: true, onReachEnd });

    scrollList(4000);

    expect(onReachEnd).toHaveBeenCalled();
  });

  it("does not ask for more while the user is still near the top", () => {
    const onReachEnd = vi.fn();
    renderPicker({ hasMore: true, onReachEnd });
    onReachEnd.mockClear();

    scrollList(100);

    expect(onReachEnd).not.toHaveBeenCalled();
  });

  it("asks for more on its own when the batch does not fill the viewport", () => {
    const onReachEnd = vi.fn();
    // Nothing to scroll means no scroll event, so the list has to ask by itself or it
    // would strand at one batch forever.
    renderPicker({ hasMore: true, onReachEnd });

    expect(onReachEnd).toHaveBeenCalled();
  });

  it("does not ask on its own once the whole list is loaded", () => {
    const onReachEnd = vi.fn();
    renderPicker({ hasMore: false, onReachEnd });

    expect(onReachEnd).not.toHaveBeenCalled();
  });

  it("says a batch is on its way while one is outstanding", () => {
    renderPicker({ hasMore: true, loadingMore: true });
    expect(screen.getByText("Loading more…")).toBeInTheDocument();
  });

  it("says nothing more is coming once the whole list is loaded", () => {
    renderPicker({ hasMore: false });
    expect(screen.queryByText(/more/)).not.toBeInTheDocument();
  });
});
