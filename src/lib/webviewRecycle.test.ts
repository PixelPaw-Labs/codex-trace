import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";

const inFlight = vi.fn(() => 0);
vi.mock("./invoke", () => ({ inFlightInvokeCount: () => inFlight() }));

const {
  RELOAD_AFTER_N_SWITCHES,
  shouldRecycle,
  saveRestoreState,
  takeRestoreState,
  waitForQuietInvokes,
  reloadWebview,
} = await import("./webviewRecycle");

describe("shouldRecycle", () => {
  it("does not recycle before the threshold", () => {
    expect(shouldRecycle(0)).toBe(false);
    expect(shouldRecycle(RELOAD_AFTER_N_SWITCHES - 1)).toBe(false);
  });

  it("recycles at and past the threshold", () => {
    expect(shouldRecycle(RELOAD_AFTER_N_SWITCHES)).toBe(true);
    expect(shouldRecycle(RELOAD_AFTER_N_SWITCHES + 5)).toBe(true);
  });
});

describe("restore state", () => {
  beforeEach(() => {
    sessionStorage.clear();
  });

  it("round-trips the open session path", () => {
    saveRestoreState({ sessionPath: "/s/a.jsonl" });
    expect(takeRestoreState()).toEqual({ sessionPath: "/s/a.jsonl" });
  });

  it("is consumed by the first read, so a later reload does not reopen it", () => {
    saveRestoreState({ sessionPath: "/s/a.jsonl" });
    takeRestoreState();
    expect(takeRestoreState()).toBeNull();
  });

  it("is null when nothing was saved", () => {
    expect(takeRestoreState()).toBeNull();
  });

  it("ignores stored data that is not a restore record", () => {
    sessionStorage.setItem("codextrace.pendingRestore", "{ not json");
    expect(takeRestoreState()).toBeNull();

    sessionStorage.setItem("codextrace.pendingRestore", JSON.stringify({ sessionPath: 42 }));
    expect(takeRestoreState()).toBeNull();
  });

  it("does not throw when storage is unavailable", () => {
    const setItem = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("quota");
    });
    const getItem = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("blocked");
    });

    expect(() => saveRestoreState({ sessionPath: "/s/a.jsonl" })).not.toThrow();
    expect(takeRestoreState()).toBeNull();

    setItem.mockRestore();
    getItem.mockRestore();
  });
});

describe("waitForQuietInvokes", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    inFlight.mockReturnValue(0);
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("returns immediately when nothing is in flight", async () => {
    await expect(waitForQuietInvokes()).resolves.toBeUndefined();
  });

  it("waits for in-flight calls to settle", async () => {
    inFlight.mockReturnValue(1);
    let settled = false;
    const wait = waitForQuietInvokes(3000, 50).then(() => {
      settled = true;
    });

    await vi.advanceTimersByTimeAsync(200);
    expect(settled).toBe(false);

    inFlight.mockReturnValue(0);
    await vi.advanceTimersByTimeAsync(50);
    await wait;
    expect(settled).toBe(true);
  });

  it("gives up at the timeout so a stuck call cannot block forever", async () => {
    inFlight.mockReturnValue(1);
    let settled = false;
    const wait = waitForQuietInvokes(300, 50).then(() => {
      settled = true;
    });

    await vi.advanceTimersByTimeAsync(400);
    await wait;
    expect(settled).toBe(true);
  });
});

describe("reloadWebview", () => {
  it("reloads only once the in-flight calls have settled", async () => {
    vi.useFakeTimers();
    const reload = vi.fn();
    const original = window.location;
    Object.defineProperty(window, "location", {
      configurable: true,
      value: { ...original, reload },
    });
    inFlight.mockReturnValue(1);

    const done = reloadWebview();
    await vi.advanceTimersByTimeAsync(100);
    expect(reload).not.toHaveBeenCalled();

    inFlight.mockReturnValue(0);
    await vi.advanceTimersByTimeAsync(50);
    await done;
    expect(reload).toHaveBeenCalledTimes(1);

    Object.defineProperty(window, "location", { configurable: true, value: original });
    vi.useRealTimers();
  });
});
