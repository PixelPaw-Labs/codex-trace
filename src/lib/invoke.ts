import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { isTauri } from "./isTauri";
import { API_BASE } from "./config";
import { authHeaders } from "./apiToken";

interface Route {
  method?: "POST";
  path: string | ((args: Record<string, unknown>) => string);
  body?: (args: Record<string, unknown>) => unknown;
}

/**
 * HTTP equivalents of the Tauri commands, for the browser and Docker builds.
 *
 * Every command registered in `src-tauri/src/lib.rs` needs an entry here or it
 * silently fails outside the desktop app — nothing at compile time connects the
 * two lists, so `invoke.test.ts` reads the Rust source and diffs them.
 */
export const routes: Record<string, Route> = {
  get_settings: { path: "/api/settings" },
  set_allowed_origins: {
    method: "POST",
    path: "/api/settings/origins",
    body: (a) => ({ origins: a.origins ?? [] }),
  },
  list_clients: { path: "/api/clients" },
  register_client: {
    method: "POST",
    path: "/api/clients",
    body: (a) => ({ name: a.name }),
  },
  reissue_client: {
    method: "POST",
    path: (a) => `/api/clients/${encodeURIComponent(String(a.id))}/reissue`,
  },
  revoke_client: {
    method: "POST",
    path: (a) => `/api/clients/${encodeURIComponent(String(a.id))}/revoke`,
  },
  set_sessions_dir: {
    method: "POST",
    path: "/api/settings/dir",
    body: (a) => ({ path: a.path ?? null }),
  },
  list_sessions: {
    method: "POST",
    path: "/api/sessions",
    body: (a) => ({ dir: a.sessionsDir as string }),
  },
  load_session: {
    method: "POST",
    path: "/api/session/load",
    body: (a) => ({ path: a.path }),
  },
  load_turn: {
    method: "POST",
    path: "/api/session/turn",
    body: (a) => ({ path: a.path, index: a.index }),
  },
  watch_session: {
    method: "POST",
    path: "/api/session/watch",
    body: (a) => ({ path: a.path }),
  },
  unwatch_session: { method: "POST", path: "/api/session/unwatch" },
  watch_picker: {
    method: "POST",
    path: "/api/picker/watch",
    body: (a) => ({ sessionsDir: a.sessionsDir }),
  },
  unwatch_picker: { method: "POST", path: "/api/picker/unwatch" },
};

async function fetchJson<T>(url: string, init?: RequestInit): Promise<T> {
  const res = await fetch(url, {
    ...init,
    // Sends the HttpOnly credential cookie the server set on the HTML shell,
    // which is what the Docker same-origin bundle authenticates with. Not
    // "include": a cross-origin caller (the dev server) uses the header carrier
    // instead, and a credentialed cross-origin request would need the API to
    // allow credentials, which it deliberately does not.
    credentials: "same-origin",
    headers: { "Content-Type": "application/json", ...authHeaders(), ...init?.headers },
  });
  if (!res.ok) {
    const body = await res.json().catch(() => ({ error: res.statusText }));
    throw new Error((body as { error?: string }).error ?? res.statusText);
  }
  const text = await res.text();
  return text ? (JSON.parse(text) as T) : (undefined as T);
}

async function httpInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const route = routes[cmd];
  if (!route) throw new Error(`[web] Unknown command "${cmd}"`);
  const a = args ?? {};
  const path = typeof route.path === "function" ? route.path(a) : route.path;
  const init: RequestInit = {};
  if (route.method) init.method = route.method;
  if (route.body) init.body = JSON.stringify(route.body(a));
  return fetchJson<T>(`${API_BASE}${path}`, init);
}

export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (isTauri) return tauriInvoke<T>(cmd, args);
  return httpInvoke<T>(cmd, args);
}
