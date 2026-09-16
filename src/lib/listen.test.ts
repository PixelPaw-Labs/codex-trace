import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

/**
 * Stand-in for the browser's `EventSource` with the three things a real one
 * does that `listen.ts` has to cope with: open a stream (200), *fail the
 * connection* on a non-200 reply (`readyState` CLOSED, one `error`, no retry —
 * the 401 case), and retry a network failure by itself (`readyState`
 * CONNECTING, one `error` per attempt).
 */
class FakeEventSource {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSED = 2;
  static instances: FakeEventSource[] = [];

  readyState = FakeEventSource.CONNECTING;
  closed = false;
  private listeners = new Map<string, Set<EventListener>>();

  constructor(public url: string) {
    FakeEventSource.instances.push(this);
  }

  addEventListener(type: string, fn: EventListener) {
    let set = this.listeners.get(type);
    if (!set) {
      set = new Set();
      this.listeners.set(type, set);
    }
    set.add(fn);
  }

  removeEventListener(type: string, fn: EventListener) {
    this.listeners.get(type)?.delete(fn);
  }

  close() {
    this.closed = true;
    this.readyState = FakeEventSource.CLOSED;
  }

  /** The server answered `200 text/event-stream`. */
  open() {
    this.readyState = FakeEventSource.OPEN;
    this.dispatch("open");
  }

  /** The server answered anything else (e.g. 401): the browser gives up for good. */
  fail() {
    this.readyState = FakeEventSource.CLOSED;
    this.dispatch("error");
  }

  /** The connection dropped: the browser reconnects on its own. */
  drop() {
    this.readyState = FakeEventSource.CONNECTING;
    this.dispatch("error");
  }

  message(event: string, data: string) {
    this.dispatch(event, { data } as MessageEvent);
  }

  private dispatch(type: string, e: unknown = {}) {
    for (const fn of this.listeners.get(type) ?? []) fn(e as Event);
  }
}

vi.stubGlobal("EventSource", FakeEventSource);
vi.mock("./isTauri", () => ({ isTauri: false }));

/** Streams still usable: neither closed by us nor failed by the browser. */
const live = () => FakeEventSource.instances.filter((s) => s.readyState !== FakeEventSource.CLOSED);
const latest = () => FakeEventSource.instances[FakeEventSource.instances.length - 1];

// `listen.ts` keeps its connection, ref count and backoff delay in module
// scope, so every test gets a freshly imported copy rather than inheriting the
// previous test's reconnect state.
let listen: typeof import("./listen").listen;
let reconnectSse: typeof import("./listen").reconnectSse;
let setApiToken: typeof import("./apiToken").setApiToken;
let SSE_REOPEN_MIN_MS: number;
let SSE_REOPEN_MAX_MS: number;

describe("listen (SSE)", () => {
  beforeEach(async () => {
    vi.useFakeTimers();
    FakeEventSource.instances = [];
    vi.resetModules();
    const mod = await import("./listen");
    ({ listen, reconnectSse, SSE_REOPEN_MIN_MS, SSE_REOPEN_MAX_MS } = mod);
    ({ setApiToken } = await import("./apiToken"));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("opens one shared stream and delivers parsed payloads", async () => {
    const seen: unknown[] = [];
    const off = await listen<{ a: number }>("session-update", (e) => {
      seen.push(e.payload);
    });

    expect(FakeEventSource.instances).toHaveLength(1);
    latest().message("session-update", JSON.stringify({ a: 1 }));
    expect(seen).toEqual([{ a: 1 }]);

    off();
  });

  it("ignores a malformed payload instead of throwing", async () => {
    const handler = vi.fn();
    const off = await listen("session-update", handler);

    latest().message("session-update", "{not json");
    expect(handler).not.toHaveBeenCalled();

    off();
  });

  it("shares one connection across listeners and closes it on the last release", async () => {
    const offA = await listen("session-update", vi.fn());
    const offB = await listen("picker-refresh", vi.fn());
    expect(FakeEventSource.instances).toHaveLength(1);

    offA();
    expect(live()).toHaveLength(1);
    offB();
    expect(live()).toHaveLength(0);
  });

  it("carries the credential in the query string", async () => {
    setApiToken("abc");
    const off = await listen("session-update", vi.fn());
    expect(latest().url).toContain("token=abc");
    off();
  });

  // -- the bug this fixes ---------------------------------------------------

  it("reopens a stream the browser refused, and keeps delivering events", async () => {
    const seen: unknown[] = [];
    const off = await listen<{ n: number }>("session-update", (e) => {
      seen.push(e.payload);
    });

    // A reconnect refused with 401: EventSource closes and never retries.
    latest().fail();
    expect(live()).toHaveLength(0);

    vi.advanceTimersByTime(SSE_REOPEN_MIN_MS);
    expect(live()).toHaveLength(1);

    // The replacement carries the listener across, so the live tail resumes.
    latest().open();
    latest().message("session-update", JSON.stringify({ n: 7 }));
    expect(seen).toEqual([{ n: 7 }]);

    off();
  });

  it("backs off between refused attempts and caps the delay", async () => {
    const off = await listen("session-update", vi.fn());

    let expected = SSE_REOPEN_MIN_MS;
    for (let i = 0; i < 8; i++) {
      latest().fail();
      // Nothing happens a tick early...
      vi.advanceTimersByTime(expected - 1);
      const before = FakeEventSource.instances.length;
      vi.advanceTimersByTime(1);
      expect(FakeEventSource.instances.length).toBe(before + 1);
      expected = Math.min(expected * 2, SSE_REOPEN_MAX_MS);
    }
    expect(expected).toBe(SSE_REOPEN_MAX_MS);

    off();
  });

  it("resets the backoff once a stream opens again", async () => {
    const off = await listen("session-update", vi.fn());

    latest().fail();
    vi.advanceTimersByTime(SSE_REOPEN_MIN_MS);
    latest().fail();
    vi.advanceTimersByTime(SSE_REOPEN_MIN_MS * 2);
    latest().open();

    // Back to the minimum delay, not the escalated one.
    latest().fail();
    const before = FakeEventSource.instances.length;
    vi.advanceTimersByTime(SSE_REOPEN_MIN_MS);
    expect(FakeEventSource.instances.length).toBe(before + 1);

    off();
  });

  it("leaves a network drop to the browser's own retry", async () => {
    const off = await listen("session-update", vi.fn());

    latest().drop(); // readyState CONNECTING — the browser is still trying
    vi.advanceTimersByTime(SSE_REOPEN_MAX_MS);
    expect(FakeEventSource.instances).toHaveLength(1);

    off();
  });

  it("stops reopening once nothing is listening", async () => {
    const off = await listen("session-update", vi.fn());
    latest().fail();
    off();

    vi.advanceTimersByTime(SSE_REOPEN_MAX_MS * 2);
    expect(live()).toHaveLength(0);
  });

  it("does not schedule a second reopen while one is pending", async () => {
    const off = await listen("session-update", vi.fn());

    const source = latest();
    source.fail();
    source.fail();
    source.fail();

    const before = FakeEventSource.instances.length;
    vi.advanceTimersByTime(SSE_REOPEN_MIN_MS);
    expect(FakeEventSource.instances.length).toBe(before + 1);

    off();
  });

  // -- credential changes ---------------------------------------------------

  it("reopens with the new credential when it changes", async () => {
    setApiToken("old");
    const off = await listen("session-update", vi.fn());
    expect(latest().url).toContain("token=old");

    setApiToken("new");
    expect(latest().url).toContain("token=new");
    expect(live()).toHaveLength(1);

    off();
  });

  it("keeps listeners attached across a credential change", async () => {
    const seen: unknown[] = [];
    setApiToken("old");
    const off = await listen<{ n: number }>("session-update", (e) => {
      seen.push(e.payload);
    });

    setApiToken("new");
    latest().message("session-update", JSON.stringify({ n: 1 }));
    expect(seen).toEqual([{ n: 1 }]);

    off();
  });

  it("unsubscribing after a reconnect still releases the connection", async () => {
    const handler = vi.fn();
    const off = await listen("session-update", handler);

    reconnectSse();
    off();

    expect(live()).toHaveLength(0);
    // The listener is gone from the replacement too.
    latest().message("session-update", "{}");
    expect(handler).not.toHaveBeenCalled();
  });

  it("does nothing when the credential changes with no listeners", () => {
    setApiToken("whatever");
    expect(FakeEventSource.instances).toHaveLength(0);
  });

  it("starts each reconnect cycle from the minimum delay", async () => {
    const off = await listen("session-update", vi.fn());
    latest().fail();

    const before = FakeEventSource.instances.length;
    vi.advanceTimersByTime(SSE_REOPEN_MIN_MS);
    expect(FakeEventSource.instances.length).toBe(before + 1);

    off();
  });
});

describe("listen handler errors", () => {
  beforeEach(async () => {
    vi.resetModules();
    FakeEventSource.instances = [];
    const mod = await import("./listen");
    listen = mod.listen;
    reconnectSse = mod.reconnectSse;
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("awaits an async handler's rejection instead of leaking it", async () => {
    const onError = vi.spyOn(console, "error").mockImplementation(() => {});
    const off = await listen("session-update", async () => {
      await Promise.resolve();
      throw new Error("handler blew up");
    });

    latest().message("session-update", "{}");
    await vi.waitFor(() => expect(onError).toHaveBeenCalled());
    expect(String(onError.mock.calls[0][0])).toContain("session-update handler failed");

    off();
  });

  it("reports a handler that throws synchronously", async () => {
    const onError = vi.spyOn(console, "error").mockImplementation(() => {});
    const off = await listen("session-update", () => {
      throw new Error("sync blow up");
    });

    latest().message("session-update", "{}");

    expect(onError).toHaveBeenCalled();
    expect(String(onError.mock.calls[0][0])).toContain("session-update handler failed");

    off();
  });

  it("drops a malformed frame without calling the handler or logging", async () => {
    const onError = vi.spyOn(console, "error").mockImplementation(() => {});
    const handler = vi.fn();
    const off = await listen("session-update", handler);

    latest().message("session-update", "not json");

    expect(handler).not.toHaveBeenCalled();
    expect(onError).not.toHaveBeenCalled();

    off();
  });

  it("delivers to an async handler", async () => {
    const seen: unknown[] = [];
    const off = await listen<{ n: number }>("session-update", async (e) => {
      await Promise.resolve();
      seen.push(e.payload);
    });

    latest().message("session-update", JSON.stringify({ n: 7 }));
    await vi.waitFor(() => expect(seen).toEqual([{ n: 7 }]));

    off();
  });
});
