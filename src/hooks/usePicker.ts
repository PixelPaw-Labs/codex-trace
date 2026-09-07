import { useState, useEffect, useCallback } from "react";
import { invoke } from "../lib/invoke";
import type { CodexSessionInfo, SettingsResponse } from "../../shared/types";
import { orderSessionsByProject } from "../lib/sessionGrouping";
import { useTauriEvent } from "./useTauriEvent";

interface PickerState {
  sessions: CodexSessionInfo[];
  loading: boolean;
  searchQuery: string;
  sessionsDir: string;
}

export function usePicker() {
  const [state, setState] = useState<PickerState>({
    sessions: [],
    loading: false,
    searchQuery: "",
    sessionsDir: "",
  });

  const discoverSessions = useCallback(async (sessionsDir: string) => {
    if (!sessionsDir) return;
    setState((prev) => ({ ...prev, loading: true, sessionsDir }));
    try {
      const sessions = await invoke<CodexSessionInfo[]>("list_sessions", { sessionsDir });
      setState((prev) => ({ ...prev, sessions, loading: false }));
      try {
        await invoke<void>("watch_picker", { sessionsDir });
      } catch {
        // watcher is optional
      }
    } catch (err) {
      console.error("Failed to discover sessions:", err);
      setState((prev) => ({ ...prev, loading: false }));
    }
  }, []);

  const setSearchQuery = useCallback((query: string) => {
    setState((prev) => ({ ...prev, searchQuery: query }));
  }, []);

  const updateSessionOngoing = useCallback((path: string, ongoing: boolean) => {
    setState((prev) => {
      const idx = prev.sessions.findIndex((s) => s.path === path);
      if (idx === -1 || prev.sessions[idx].is_ongoing === ongoing) return prev;
      const sessions = [...prev.sessions];
      sessions[idx] = { ...sessions[idx], is_ongoing: ongoing };
      return { ...prev, sessions };
    });
  }, []);

  // picker-refresh carries no session data — the watcher sends only a lightweight
  // signal. Re-fetch via the API so the expensive discover_sessions scan runs
  // only on demand and is coalesced by the server-side cache.
  useTauriEvent("picker-refresh", () => {
    setState((prev) => {
      if (!prev.sessionsDir) return prev;
      invoke<CodexSessionInfo[]>("list_sessions", { sessionsDir: prev.sessionsDir })
        .then((sessions) => setState((s) => ({ ...s, sessions })))
        .catch(() => {});
      return prev;
    });
  });

  useEffect(() => {
    return () => {
      invoke<void>("unwatch_picker").catch(() => {});
    };
  }, []);

  const filteredSessions = state.searchQuery
    ? state.sessions.filter(
        (s) =>
          (s.thread_name ?? "").toLowerCase().includes(state.searchQuery.toLowerCase()) ||
          (s.ai_title ?? "").toLowerCase().includes(state.searchQuery.toLowerCase()) ||
          s.id.toLowerCase().includes(state.searchQuery.toLowerCase()) ||
          (s.cwd ?? "").toLowerCase().includes(state.searchQuery.toLowerCase()),
      )
    : state.sessions;
  const orderedSessions = orderSessionsByProject(filteredSessions);

  return {
    sessions: orderedSessions,
    allSessions: state.sessions,
    loading: state.loading,
    searchQuery: state.searchQuery,
    sessionsDir: state.sessionsDir,
    setSearchQuery,
    discoverSessions,
    updateSessionOngoing,
  };
}

export async function resolveSessionsDir(): Promise<string> {
  const settings = await invoke<SettingsResponse>("get_settings");
  return settings.sessions_dir ?? settings.default_dir;
}
