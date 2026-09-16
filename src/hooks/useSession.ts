import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "../lib/invoke";
import type { CodexSession, CodexTurn, SessionIndex, TurnSummary } from "../../shared/types";
import { useTauriEvent } from "./useTauriEvent";

interface SessionState {
  session: CodexSession | null;
  summaries: TurnSummary[];
  loading: boolean;
  sessionPath: string;
}

/** How many opened turns keep their bodies. A turn's tool output can be
 * megabytes, so the detail view holds a handful rather than everything the user
 * has clicked through. */
const MAX_LOADED_TURNS = 4;

const emptyState: SessionState = {
  session: null,
  summaries: [],
  loading: false,
  sessionPath: "",
};

/**
 * The open session's metadata and turn index, plus the bodies of whichever
 * turns have actually been opened.
 *
 * The list view draws from `summaries`, which carry no tool output, so the heap
 * holds one small record per turn however large the transcript is. A turn's
 * bodies are fetched only when the detail view asks for them, and only the most
 * recently opened ones are kept.
 */
export function useSession() {
  const [state, setState] = useState<SessionState>(emptyState);
  // Bodies of the turns opened so far, keyed by index into `summaries`.
  // Replaced (new Map) on every change so React re-renders.
  const [turns, setTurns] = useState<Map<number, CodexTurn>>(new Map());

  // Bumped on every load, so a fetch still in flight for the previous session
  // is discarded when it resolves instead of writing into the new one.
  const loadIdRef = useRef(0);
  const pathRef = useRef("");
  const inflightRef = useRef<Set<number>>(new Set());
  // Which turns are loaded, most-recently-opened last. Mirrors the map's keys
  // so `ensureTurn` can check membership without reading state during render.
  const loadedRef = useRef<number[]>([]);

  const resetTurns = useCallback(() => {
    inflightRef.current.clear();
    loadedRef.current = [];
    setTurns(new Map());
  }, []);

  const loadSession = useCallback(
    async (path: string) => {
      const loadId = ++loadIdRef.current;
      pathRef.current = path;
      resetTurns();
      setState((prev) => ({ ...prev, loading: true }));
      try {
        try {
          await invoke<void>("unwatch_session");
        } catch {
          // ignore
        }
        const index = await invoke<SessionIndex>("load_session", { path });
        if (loadIdRef.current !== loadId) return;
        setState({
          session: index.session,
          summaries: index.summaries,
          loading: false,
          sessionPath: path,
        });
        try {
          await invoke<void>("watch_session", { path });
        } catch {
          // watcher is optional
        }
      } catch (err) {
        console.error("Failed to load session:", err);
        if (loadIdRef.current !== loadId) return;
        setState((prev) => ({ ...prev, loading: false }));
      }
    },
    [resetTurns],
  );

  /**
   * Make sure turn `index` has its bodies loaded. Safe to call repeatedly: an
   * already-loaded or already-in-flight turn is a no-op.
   */
  const ensureTurn = useCallback(async (index: number) => {
    const path = pathRef.current;
    if (!path || index < 0) return;
    if (inflightRef.current.has(index)) return;
    if (loadedRef.current.includes(index)) {
      // Already here: move it to the back of the queue so revisiting a turn
      // does not leave it first in line to be evicted.
      loadedRef.current = [...loadedRef.current.filter((i) => i !== index), index];
      return;
    }

    const loadId = loadIdRef.current;
    inflightRef.current.add(index);
    try {
      const turn = await invoke<CodexTurn>("load_turn", { path, index });
      if (loadIdRef.current !== loadId) return;

      const keep = [...loadedRef.current.filter((i) => i !== index), index].slice(
        -MAX_LOADED_TURNS,
      );
      loadedRef.current = keep;
      setTurns((prev) => {
        const next = new Map(prev);
        next.set(index, turn);
        for (const loaded of next.keys()) {
          if (!keep.includes(loaded)) next.delete(loaded);
        }
        return next;
      });
    } catch (err) {
      console.error(`Failed to load turn ${index}:`, err);
    } finally {
      inflightRef.current.delete(index);
    }
  }, []);

  /** Re-read the index for the open session. Used by the live-update signal. */
  const refreshIndex = useCallback(async () => {
    const path = pathRef.current;
    if (!path) return;
    const loadId = loadIdRef.current;
    try {
      const index = await invoke<SessionIndex>("load_session", { path });
      if (loadIdRef.current !== loadId) return;
      setState((prev) => ({
        ...prev,
        session: index.session,
        summaries: index.summaries,
      }));
    } catch (err) {
      console.error("Failed to refresh session:", err);
    }
  }, []);

  // `session-refresh` carries no data — the watcher sends only a signal, having
  // already re-read the file into the backend's cache. Fetching here keeps the
  // session off the wire once per connected client per write.
  useTauriEvent("session-refresh", () => {
    // The file changed, so every body fetched from it may be out of date.
    resetTurns();
    void refreshIndex();
  });

  useEffect(() => {
    return () => {
      invoke<void>("unwatch_session").catch(() => {});
    };
  }, []);

  return {
    ...state,
    turns,
    loadSession,
    ensureTurn,
  };
}
