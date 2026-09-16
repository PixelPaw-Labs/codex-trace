import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { CodexSession, CodexTurn, TurnSummary } from "../../shared/types";

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

const { useSession } = await import("./useSession");

function summary(i: number): TurnSummary {
  return {
    turn_id: `turn-${i}`,
    status: "complete",
    started_at: null,
    completed_at: null,
    duration_ms: null,
    user_message: `prompt ${i}`,
    agent_preview: null,
    last_agent_timestamp: null,
    tool_call_count: 0,
    reasoning_count: 0,
    total_tokens: null,
    model: null,
    has_detail: true,
  };
}

function turn(i: number): CodexTurn {
  return { turn_id: `turn-${i}` } as CodexTurn;
}

function index(count: number) {
  return {
    session: { id: "s1", turns: [] } as unknown as CodexSession,
    summaries: Array.from({ length: count }, (_, i) => summary(i)),
  };
}

/** Open turns one after another: eviction order depends on the sequence. */
async function openInOrder(ensureTurn: (i: number) => Promise<void>, indexes: number[]) {
  await indexes.reduce((chain, i) => chain.then(() => ensureTurn(i)), Promise.resolve());
}

/** Which turn indexes `load_turn` was asked for, in order. */
function requestedTurns() {
  return invoke.mock.calls
    .filter(([cmd]) => cmd === "load_turn")
    .map(([, args]) => (args as { index: number }).index);
}

function mockBackend(turnCount = 10) {
  invoke.mockImplementation(async (cmd: string, args?: unknown) => {
    if (cmd === "load_session") return index(turnCount);
    if (cmd === "load_turn") return turn((args as { index: number }).index);
    return undefined;
  });
}

describe("useSession", () => {
  beforeEach(() => {
    invoke.mockReset();
    handlers.clear();
    mockBackend();
  });

  it("loads the turn index without any turn bodies", async () => {
    const { result } = renderHook(() => useSession());

    await act(async () => {
      await result.current.loadSession("/s.jsonl");
    });

    expect(result.current.summaries).toHaveLength(10);
    expect(result.current.session?.id).toBe("s1");
    expect(result.current.turns.size).toBe(0);
    expect(requestedTurns()).toEqual([]);
  });

  it("fetches a turn's bodies only when asked", async () => {
    const { result } = renderHook(() => useSession());
    await act(async () => {
      await result.current.loadSession("/s.jsonl");
    });

    await act(async () => {
      await result.current.ensureTurn(3);
    });

    expect(requestedTurns()).toEqual([3]);
    expect(result.current.turns.get(3)?.turn_id).toBe("turn-3");
  });

  it("does not refetch a turn it already has", async () => {
    const { result } = renderHook(() => useSession());
    await act(async () => {
      await result.current.loadSession("/s.jsonl");
      await result.current.ensureTurn(1);
      await result.current.ensureTurn(1);
    });

    expect(requestedTurns()).toEqual([1]);
  });

  it("does not issue a second fetch while one is in flight", async () => {
    const { result } = renderHook(() => useSession());
    await act(async () => {
      await result.current.loadSession("/s.jsonl");
    });

    await act(async () => {
      await Promise.all([result.current.ensureTurn(2), result.current.ensureTurn(2)]);
    });

    expect(requestedTurns()).toEqual([2]);
  });

  it("keeps only the most recently opened turns", async () => {
    const { result } = renderHook(() => useSession());
    await act(async () => {
      await result.current.loadSession("/s.jsonl");
      await openInOrder(result.current.ensureTurn, [0, 1, 2, 3, 4]);
    });

    // Four is the cap, so the first one opened is gone.
    expect([...result.current.turns.keys()].toSorted((a, b) => a - b)).toEqual([1, 2, 3, 4]);
  });

  it("re-opening a kept turn refreshes its place in the queue", async () => {
    const { result } = renderHook(() => useSession());
    await act(async () => {
      await result.current.loadSession("/s.jsonl");
      // 0 is opened again before 4, so 1 becomes the oldest and is the one dropped.
      await openInOrder(result.current.ensureTurn, [0, 1, 2, 3, 0, 4]);
    });

    // Turn 1 was the oldest by the time 4 arrived, so it is the one dropped.
    expect([...result.current.turns.keys()].toSorted((a, b) => a - b)).toEqual([0, 2, 3, 4]);
    expect(requestedTurns()).toEqual([0, 1, 2, 3, 4]);
  });

  it("drops loaded turns when a different session is opened", async () => {
    const { result } = renderHook(() => useSession());
    await act(async () => {
      await result.current.loadSession("/a.jsonl");
      await result.current.ensureTurn(0);
    });
    expect(result.current.turns.size).toBe(1);

    await act(async () => {
      await result.current.loadSession("/b.jsonl");
    });

    expect(result.current.turns.size).toBe(0);
  });

  it("ignores a turn that resolves after a different session was opened", async () => {
    let releaseTurn: ((t: CodexTurn) => void) | undefined;
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "load_session") return index(10);
      if (cmd === "load_turn") {
        return new Promise<CodexTurn>((resolve) => {
          releaseTurn = resolve;
        });
      }
      return undefined;
    });

    const { result } = renderHook(() => useSession());
    await act(async () => {
      await result.current.loadSession("/a.jsonl");
    });

    let pending: Promise<void>;
    act(() => {
      pending = result.current.ensureTurn(0);
    });
    await act(async () => {
      await result.current.loadSession("/b.jsonl");
      releaseTurn?.(turn(0));
      await pending;
    });

    expect(result.current.turns.size).toBe(0);
  });

  it("ignores a negative index", async () => {
    const { result } = renderHook(() => useSession());
    await act(async () => {
      await result.current.loadSession("/s.jsonl");
      await result.current.ensureTurn(-1);
    });

    expect(requestedTurns()).toEqual([]);
  });

  it("does nothing when no session is open", async () => {
    const { result } = renderHook(() => useSession());
    await act(async () => {
      await result.current.ensureTurn(0);
    });

    expect(requestedTurns()).toEqual([]);
  });

  it("survives a failed turn fetch and can retry it", async () => {
    const { result } = renderHook(() => useSession());
    const errors = vi.spyOn(console, "error").mockImplementation(() => {});
    await act(async () => {
      await result.current.loadSession("/s.jsonl");
    });

    invoke.mockImplementationOnce(async () => {
      throw new Error("no turn at index 0");
    });
    await act(async () => {
      await result.current.ensureTurn(0);
    });
    expect(result.current.turns.size).toBe(0);

    // The failure did not leave the index marked as in flight.
    await act(async () => {
      await result.current.ensureTurn(0);
    });
    await waitFor(() => expect(result.current.turns.get(0)?.turn_id).toBe("turn-0"));
    errors.mockRestore();
  });

  it("re-fetches the index on the live-update signal", async () => {
    const { result } = renderHook(() => useSession());
    await act(async () => {
      await result.current.loadSession("/s.jsonl");
    });

    invoke.mockImplementation(async (cmd: string) =>
      cmd === "load_session" ? index(12) : undefined,
    );
    await act(async () => {
      handlers.get("session-refresh")?.({});
    });

    await waitFor(() => expect(result.current.summaries).toHaveLength(12));
  });

  it("refreshes an open turn in place rather than blanking it", async () => {
    const { result } = renderHook(() => useSession());
    await act(async () => {
      await result.current.loadSession("/s.jsonl");
      await result.current.ensureTurn(0);
    });
    expect(result.current.turns.size).toBe(1);

    // The body the backend will serve after the file grew.
    invoke.mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "load_session") return index(10);
      if (cmd === "load_turn") {
        return { ...turn((args as { index: number }).index), status: "complete" } as CodexTurn;
      }
      return undefined;
    });

    await act(async () => {
      await handlers.get("session-refresh")?.({});
    });

    // Never empty: blanking it makes the detail view fall back to
    // "Loading turn…" on every appended line of a live session.
    expect(result.current.turns.size).toBe(1);
    expect(result.current.turns.get(0)).toMatchObject({ turn_id: "turn-0", status: "complete" });
  });

  it("re-reads every open turn on the live-update signal", async () => {
    const { result } = renderHook(() => useSession());
    await act(async () => {
      await result.current.loadSession("/s.jsonl");
      await openInOrder(result.current.ensureTurn, [2, 5]);
    });
    invoke.mockClear();

    await act(async () => {
      await handlers.get("session-refresh")?.({});
    });

    expect(requestedTurns().toSorted()).toEqual([2, 5]);
  });

  it("keeps a turn on screen when its refresh fails", async () => {
    const errors = vi.spyOn(console, "error").mockImplementation(() => {});
    const { result } = renderHook(() => useSession());
    await act(async () => {
      await result.current.loadSession("/s.jsonl");
      await result.current.ensureTurn(0);
    });

    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "load_session") return index(10);
      if (cmd === "load_turn") throw new Error("no turn at index 0");
      return undefined;
    });

    await act(async () => {
      await handlers.get("session-refresh")?.({});
    });

    expect(result.current.turns.get(0)).toMatchObject({ turn_id: "turn-0" });
    errors.mockRestore();
  });

  it("ignores the live-update signal when no session is open", async () => {
    renderHook(() => useSession());
    await act(async () => {
      handlers.get("session-refresh")?.({});
    });

    expect(invoke).not.toHaveBeenCalledWith("load_session", expect.anything());
  });

  it("reports a failed session load without leaving it loading", async () => {
    const errors = vi.spyOn(console, "error").mockImplementation(() => {});
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "load_session") throw new Error("broken");
      return undefined;
    });

    const { result } = renderHook(() => useSession());
    await act(async () => {
      await result.current.loadSession("/s.jsonl");
    });

    expect(result.current.loading).toBe(false);
    expect(result.current.summaries).toEqual([]);
    errors.mockRestore();
  });
});
