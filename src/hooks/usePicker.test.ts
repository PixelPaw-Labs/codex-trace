import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { CodexSessionInfo } from "../../shared/types";

const invoke = vi.fn();
vi.mock("../lib/invoke", () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
}));
// Capture the live-update handler so a test can fire the signal by hand.
const handlers = new Map<string, (payload: unknown) => void>();
vi.mock("./useTauriEvent", () => ({
  useTauriEvent: (event: string, handler: (payload: unknown) => void) => {
    handlers.set(event, handler);
  },
}));

const { usePicker, SESSION_BATCH } = await import("./usePicker");

function info(i: number, overrides: Partial<CodexSessionInfo> = {}): CodexSessionInfo {
  return {
    id: `s${i}`,
    path: `/sessions/rollout-${i}.jsonl`,
    date_group: "2026/09/16",
    is_inline_worker: false,
    is_ongoing: false,
    spawned_worker_ids: [],
    ...overrides,
  } as CodexSessionInfo;
}

/** The arguments of every `list_sessions` request, in order. */
function requests() {
  return invoke.mock.calls
    .filter(([cmd]) => cmd === "list_sessions")
    .map(([, args]) => args as { offset: number; limit: number; query: string | null });
}

/** A backend holding `total` sessions, answering whatever slice is asked for. */
function mockBackend(total: number) {
  invoke.mockImplementation(async (cmd: string, args?: unknown) => {
    if (cmd !== "list_sessions") return undefined;
    const { offset, limit } = args as { offset: number; limit: number };
    const end = Math.min(offset + limit, total);
    return {
      sessions: Array.from({ length: Math.max(0, end - offset) }, (_, i) => info(offset + i)),
      total,
      groups: [{ date_group: "2026/09/16", count: total }],
    };
  });
}

async function loaded(total: number) {
  mockBackend(total);
  const { result } = renderHook(() => usePicker());
  await act(async () => {
    await result.current.discoverSessions("/sessions");
  });
  return result;
}

describe("usePicker", () => {
  beforeEach(() => {
    invoke.mockReset();
    handlers.clear();
    vi.useRealTimers();
  });

  it("asks for one batch rather than the whole directory", async () => {
    const result = await loaded(3000);

    expect(requests()[0]).toMatchObject({ offset: 0, limit: SESSION_BATCH });
    expect(result.current.sessions).toHaveLength(SESSION_BATCH);
  });

  it("reports how many sessions there are without holding them", async () => {
    const result = await loaded(3000);

    expect(result.current.total).toBe(3000);
    expect(result.current.hasMore).toBe(true);
  });

  it("appends the next batch when the list reaches its end", async () => {
    const result = await loaded(3000);

    await act(async () => {
      result.current.loadMore();
    });

    await waitFor(() => expect(result.current.sessions).toHaveLength(SESSION_BATCH * 2));
    expect(requests()[1]).toMatchObject({ offset: SESSION_BATCH, limit: SESSION_BATCH });
    // The batches must join up rather than repeat the first one.
    expect(result.current.sessions[SESSION_BATCH].id).toBe(`s${SESSION_BATCH}`);
  });

  it("ignores a request for more once everything is loaded", async () => {
    const result = await loaded(10);
    expect(result.current.hasMore).toBe(false);

    await act(async () => {
      result.current.loadMore();
    });

    expect(requests()).toHaveLength(1);
  });

  it("does not fire a second request while one is in flight", async () => {
    let release: ((value: unknown) => void) | undefined;
    const result = await loaded(3000);
    invoke.mockImplementation(
      async () =>
        new Promise((resolve) => {
          release = resolve;
        }),
    );
    invoke.mockClear();

    act(() => {
      result.current.loadMore();
      result.current.loadMore();
    });

    expect(requests()).toHaveLength(1);
    await act(async () => {
      release?.({ sessions: [], total: 3000, groups: [] });
    });
  });

  it("retries a batch that failed rather than skipping past it", async () => {
    const errors = vi.spyOn(console, "error").mockImplementation(() => {});
    const result = await loaded(3000);
    invoke.mockImplementationOnce(async () => {
      throw new Error("backend down");
    });

    await act(async () => {
      result.current.loadMore();
    });
    await act(async () => {
      result.current.loadMore();
    });

    // Both attempts ask for the same batch: a lost batch must not leave a hole.
    const offsets = requests().map((r) => r.offset);
    expect(offsets).toEqual([0, SESSION_BATCH, SESSION_BATCH]);
    errors.mockRestore();
  });

  it("sends the search to the backend instead of filtering what is loaded", async () => {
    vi.useFakeTimers();
    mockBackend(3000);
    const { result } = renderHook(() => usePicker());
    await act(async () => {
      await result.current.discoverSessions("/sessions");
    });
    invoke.mockClear();

    act(() => {
      result.current.setSearchQuery("needle");
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(300);
    });

    // Only the backend can see the sessions that have not been fetched yet.
    expect(requests().at(-1)).toMatchObject({ offset: 0, query: "needle" });
    vi.useRealTimers();
  });

  it("waits for a pause before searching", async () => {
    vi.useFakeTimers();
    mockBackend(3000);
    const { result } = renderHook(() => usePicker());
    await act(async () => {
      await result.current.discoverSessions("/sessions");
    });
    invoke.mockClear();

    act(() => {
      result.current.setSearchQuery("n");
      result.current.setSearchQuery("ne");
      result.current.setSearchQuery("nee");
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(300);
    });

    expect(requests()).toHaveLength(1);
    expect(requests()[0].query).toBe("nee");
    vi.useRealTimers();
  });

  it("starts a search from the top of the list", async () => {
    vi.useFakeTimers();
    mockBackend(3000);
    const { result } = renderHook(() => usePicker());
    await act(async () => {
      await result.current.discoverSessions("/sessions");
    });
    await act(async () => {
      result.current.loadMore();
    });
    expect(result.current.sessions).toHaveLength(SESSION_BATCH * 2);

    act(() => {
      result.current.setSearchQuery("needle");
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(300);
    });

    // The old results must be replaced, not appended to.
    expect(result.current.sessions).toHaveLength(SESSION_BATCH);
    vi.useRealTimers();
  });

  it("carries the date counts the backend reported", async () => {
    const result = await loaded(3000);

    // The sidebar draws these, so they have to describe the directory rather than
    // the batch that happens to be loaded.
    expect(result.current.groups).toEqual([{ date_group: "2026/09/16", count: 3000 }]);
  });

  it("keeps everything loaded when the live-update signal fires", async () => {
    const result = await loaded(3000);
    await act(async () => {
      result.current.loadMore();
    });
    expect(result.current.sessions).toHaveLength(SESSION_BATCH * 2);
    invoke.mockClear();

    await act(async () => {
      await handlers.get("picker-refresh")?.({});
    });

    // Refreshing must not throw the user back to the first batch.
    expect(requests()[0]).toMatchObject({ offset: 0, limit: SESSION_BATCH * 2 });
    expect(result.current.sessions).toHaveLength(SESSION_BATCH * 2);
  });

  it("marks a session as ongoing without refetching", async () => {
    const result = await loaded(10);
    invoke.mockClear();

    act(() => {
      result.current.updateSessionOngoing("/sessions/rollout-2.jsonl", true);
    });

    expect(result.current.sessions[2].is_ongoing).toBe(true);
    expect(requests()).toHaveLength(0);
  });

  it("drops a batch that arrives after the directory changed", async () => {
    let release: ((value: unknown) => void) | undefined;
    const { result } = renderHook(() => usePicker());
    invoke.mockImplementation(
      async () =>
        new Promise((resolve) => {
          release = resolve;
        }),
    );

    let pending: Promise<void>;
    act(() => {
      pending = result.current.discoverSessions("/old");
    });
    mockBackend(5);
    await act(async () => {
      await result.current.discoverSessions("/new");
      release?.({ sessions: [info(99)], total: 1, groups: [] });
      await pending;
    });

    expect(result.current.sessions).toHaveLength(5);
    expect(result.current.total).toBe(5);
  });
});
