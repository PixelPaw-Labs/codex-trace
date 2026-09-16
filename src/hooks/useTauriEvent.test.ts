import { renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const listeners = new Map<string, (e: { payload: unknown }) => void | Promise<void>>();
const unlisten = vi.fn();

vi.mock("../lib/listen", () => ({
  listen: vi.fn(
    async (event: string, handler: (e: { payload: unknown }) => void | Promise<void>) => {
      listeners.set(event, handler);
      return unlisten;
    },
  ),
}));

const { useTauriEvent } = await import("./useTauriEvent");

function emit(event: string, payload: unknown) {
  listeners.get(event)?.({ payload });
}

describe("useTauriEvent", () => {
  afterEach(() => {
    listeners.clear();
    vi.clearAllMocks();
  });

  it("invokes the handler with the event payload", async () => {
    const handler = vi.fn();
    renderHook(() => useTauriEvent<string>("session-updated", handler));

    await waitFor(() => expect(listeners.has("session-updated")).toBe(true));
    emit("session-updated", "payload-1");

    expect(handler).toHaveBeenCalledWith("payload-1");
  });

  // The handler is held in a ref so the subscription is not torn down on every
  // render. That ref is synced from an effect rather than during render, so this
  // asserts the listener still sees the newest closure after a re-render.
  it("calls the latest handler after a re-render without re-subscribing", async () => {
    const first = vi.fn();
    const second = vi.fn();
    const { rerender } = renderHook(({ fn }) => useTauriEvent<string>("session-updated", fn), {
      initialProps: { fn: first },
    });

    await waitFor(() => expect(listeners.has("session-updated")).toBe(true));
    rerender({ fn: second });
    emit("session-updated", "payload-2");

    expect(second).toHaveBeenCalledWith("payload-2");
    expect(first).not.toHaveBeenCalled();
  });

  it("unsubscribes on unmount", async () => {
    const { unmount } = renderHook(() => useTauriEvent<string>("session-updated", vi.fn()));

    await waitFor(() => expect(listeners.has("session-updated")).toBe(true));
    unmount();

    expect(unlisten).toHaveBeenCalled();
  });
});

describe("useTauriEvent async handlers", () => {
  afterEach(() => {
    listeners.clear();
    vi.clearAllMocks();
    vi.restoreAllMocks();
  });

  it("returns an async handler's promise so the caller can settle it", async () => {
    const seen: string[] = [];
    renderHook(() =>
      useTauriEvent<string>("session-updated", async (payload) => {
        await Promise.resolve();
        seen.push(payload);
      }),
    );

    await waitFor(() => expect(listeners.has("session-updated")).toBe(true));
    await listeners.get("session-updated")?.({ payload: "late" });

    expect(seen).toEqual(["late"]);
  });

  it("does not run an async handler after unmount", async () => {
    const handler = vi.fn(async () => {});
    const { unmount } = renderHook(() => useTauriEvent<string>("session-updated", handler));

    await waitFor(() => expect(listeners.has("session-updated")).toBe(true));
    const forward = listeners.get("session-updated")!;
    unmount();
    await forward({ payload: "after-unmount" });

    expect(handler).not.toHaveBeenCalled();
  });

  it("reports a failure to attach instead of leaving it unhandled", async () => {
    const onError = vi.spyOn(console, "error").mockImplementation(() => {});
    const { listen } = await import("../lib/listen");
    vi.mocked(listen).mockRejectedValueOnce(new Error("bridge is gone"));

    renderHook(() => useTauriEvent<string>("session-updated", vi.fn()));

    await waitFor(() => expect(onError).toHaveBeenCalled());
    expect(String(onError.mock.calls[0][0])).toContain("failed to listen for session-updated");
  });
});
