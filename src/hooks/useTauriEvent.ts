import { useEffect, useRef } from "react";
import { listen, type EventHandler, type UnlistenFn } from "../lib/listen";

/**
 * Subscribe to a Tauri event with automatic setup/teardown and cancellation safety.
 * The handler is kept in a ref so it always sees fresh closures without re-subscribing.
 */
export function useTauriEvent<T>(event: string, handler: (payload: T) => void | Promise<void>) {
  const handlerRef = useRef(handler);
  // Sync the latest handler from an effect rather than during render: writing to a
  // ref while rendering is what react(refs) flags. useRef already seeds the first
  // handler, so the listener never sees a stale value on mount.
  useEffect(() => {
    handlerRef.current = handler;
  });

  const unlistenRef = useRef<UnlistenFn | null>(null);

  useEffect(() => {
    const teardown = new AbortController();

    const forward: EventHandler<T> = (e) => {
      if (!teardown.signal.aborted) return handlerRef.current(e.payload);
    };

    const setupListener = async () => {
      const unlisten = await listen<T>(event, forward);

      if (!teardown.signal.aborted) {
        unlistenRef.current = unlisten;
      } else {
        unlisten();
      }
    };

    // Attaching can fail — the Tauri bridge or the SSE stream may be gone. A
    // bare call would turn that into an unhandled rejection and a listener
    // that never attaches, with nothing said about it.
    setupListener().catch((err) => console.error(`failed to listen for ${event}:`, err));

    return () => {
      teardown.abort();
      unlistenRef.current?.();
      unlistenRef.current = null;
    };
  }, [event]);
}
