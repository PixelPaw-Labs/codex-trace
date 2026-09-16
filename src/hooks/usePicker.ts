import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "../lib/invoke";
import type {
  CodexSessionInfo,
  DateGroupCount,
  SessionPage,
  SettingsResponse,
} from "../../shared/types";
import { useTauriEvent } from "./useTauriEvent";

/** How many sessions each request asks for. A real sessions directory holds thousands, and
 * drawing them all costs several seconds before the first row is usable. */
export const SESSION_BATCH = 200;

/** How long to wait after the last keystroke before searching. */
const SEARCH_DEBOUNCE_MS = 200;

interface PickerState {
  sessions: CodexSessionInfo[];
  /** How many sessions match the current search, across the whole directory. */
  total: number;
  groups: DateGroupCount[];
  loading: boolean;
  loadingMore: boolean;
  searchQuery: string;
  sessionsDir: string;
}

const emptyState: PickerState = {
  sessions: [],
  total: 0,
  groups: [],
  loading: false,
  loadingMore: false,
  searchQuery: "",
  sessionsDir: "",
};

/**
 * The session list, fetched a batch at a time.
 *
 * Searching and counting happen on the backend, over every session it discovered, so a
 * search still reaches sessions that have not been fetched yet and the sidebar's date
 * headers are right before the rows under them arrive.
 */
export function usePicker() {
  const [state, setState] = useState<PickerState>(emptyState);

  // Bumped whenever the list is rebuilt from the top — a new directory, a new search —
  // so a batch still in flight for the previous list is dropped when it resolves.
  const listIdRef = useRef(0);
  const dirRef = useRef("");
  const queryRef = useRef("");
  // How many sessions have been asked for so far. Kept out of state so `loadMore` can be
  // called repeatedly from a scroll handler without re-reading a stale render.
  const requestedRef = useRef(0);
  const inflightRef = useRef(false);
  const searchTimerRef = useRef<ReturnType<typeof setTimeout>>(undefined);

  /**
   * Fetch `limit` sessions from `offset`, replacing or appending as asked. Reports whether
   * the batch landed, so the caller only moves the cursor on for a batch it actually got.
   */
  const fetchBatch = useCallback(
    async (offset: number, limit: number, mode: "replace" | "append"): Promise<boolean> => {
      const dir = dirRef.current;
      if (!dir) return false;
      const listId = listIdRef.current;
      inflightRef.current = true;
      setState((prev) => ({
        ...prev,
        loading: mode === "replace",
        loadingMore: mode === "append",
      }));
      try {
        const page = await invoke<SessionPage>("list_sessions", {
          sessionsDir: dir,
          offset,
          limit,
          query: queryRef.current || null,
        });
        if (listIdRef.current !== listId) return false;
        setState((prev) => ({
          ...prev,
          sessions: mode === "replace" ? page.sessions : [...prev.sessions, ...page.sessions],
          total: page.total,
          groups: page.groups,
          loading: false,
          loadingMore: false,
        }));
        return true;
      } catch (err) {
        console.error("Failed to discover sessions:", err);
        if (listIdRef.current !== listId) return false;
        setState((prev) => ({ ...prev, loading: false, loadingMore: false }));
        return false;
      } finally {
        if (listIdRef.current === listId) inflightRef.current = false;
      }
    },
    [],
  );

  /** Throw away what is loaded and fetch the first batch again. */
  const restart = useCallback(async () => {
    listIdRef.current++;
    inflightRef.current = false;
    requestedRef.current = 0;
    if (await fetchBatch(0, SESSION_BATCH, "replace")) {
      requestedRef.current = SESSION_BATCH;
    }
  }, [fetchBatch]);

  const discoverSessions = useCallback(
    async (sessionsDir: string) => {
      if (!sessionsDir) return;
      dirRef.current = sessionsDir;
      setState((prev) => ({ ...prev, sessionsDir }));
      await restart();
      try {
        await invoke<void>("watch_picker", { sessionsDir });
      } catch {
        // watcher is optional
      }
    },
    [restart],
  );

  /**
   * Fetch the next batch. Safe to call on every scroll event: it does nothing while a
   * request is in flight or once everything matching has been fetched.
   */
  const loadMore = useCallback(() => {
    if (inflightRef.current) return;
    if (!dirRef.current) return;
    const offset = requestedRef.current;
    if (offset === 0 || offset >= state.total) return;
    void fetchBatch(offset, SESSION_BATCH, "append").then((landed) => {
      // A failed batch leaves the cursor where it was, so scrolling again retries it
      // rather than skipping past those sessions for good.
      if (landed) requestedRef.current = offset + SESSION_BATCH;
    });
  }, [fetchBatch, state.total]);

  const setSearchQuery = useCallback(
    (query: string) => {
      queryRef.current = query;
      setState((prev) => ({ ...prev, searchQuery: query }));
      // Searching runs on the backend, so wait for a pause rather than re-scanning the
      // whole list on every keystroke.
      clearTimeout(searchTimerRef.current);
      searchTimerRef.current = setTimeout(() => {
        void restart();
      }, SEARCH_DEBOUNCE_MS);
    },
    [restart],
  );

  const updateSessionOngoing = useCallback((path: string, ongoing: boolean) => {
    setState((prev) => {
      const idx = prev.sessions.findIndex((s) => s.path === path);
      if (idx === -1 || prev.sessions[idx].is_ongoing === ongoing) return prev;
      const sessions = [...prev.sessions];
      sessions[idx] = { ...sessions[idx], is_ongoing: ongoing };
      return { ...prev, sessions };
    });
  }, []);

  // picker-refresh carries no session data — the watcher sends only a lightweight signal.
  // Re-fetch everything that is already on screen in one request, so the list neither
  // shrinks back to the first batch nor loses the user's place.
  useTauriEvent("picker-refresh", async () => {
    if (!dirRef.current || inflightRef.current) return;
    await fetchBatch(0, requestedRef.current || SESSION_BATCH, "replace");
  });

  useEffect(() => {
    return () => {
      clearTimeout(searchTimerRef.current);
      invoke<void>("unwatch_picker").catch(() => {});
    };
  }, []);

  return {
    sessions: state.sessions,
    allSessions: state.sessions,
    total: state.total,
    groups: state.groups,
    loading: state.loading,
    loadingMore: state.loadingMore,
    hasMore: state.sessions.length < state.total,
    searchQuery: state.searchQuery,
    sessionsDir: state.sessionsDir,
    setSearchQuery,
    discoverSessions,
    loadMore,
    updateSessionOngoing,
  };
}

export async function resolveSessionsDir(): Promise<string> {
  const settings = await invoke<SettingsResponse>("get_settings");
  return settings.sessions_dir ?? settings.default_dir;
}
