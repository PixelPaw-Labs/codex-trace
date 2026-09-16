import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { routes } from "./invoke";

/**
 * Commands that are deliberately desktop-only. Everything else must have an
 * HTTP route, or it works in the Tauri app and silently fails in the browser
 * and Docker builds.
 */
const DESKTOP_ONLY = new Set([
  // Relaunches the desktop app into web mode; meaningless to a browser client.
  "switch_to_browser",
]);

/** Command names registered in `tauri::generate_handler![...]`. */
function registeredCommands(): string[] {
  // Vitest runs from the repo root, and `import.meta.url` is an http URL
  // under the dev-server transform, so resolve from the cwd instead.
  const src = readFileSync(join(process.cwd(), "src-tauri/src/lib.rs"), "utf8");
  const block = src.match(/generate_handler!\[([\s\S]*?)\]/);
  expect(block, "generate_handler! block present in lib.rs").not.toBeNull();
  return block![1]
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean)
    .map((entry) => entry.split("::").pop()!);
}

describe("HTTP route table", () => {
  it("covers every Tauri command that is not desktop-only", () => {
    const missing = registeredCommands().filter(
      (cmd) => !DESKTOP_ONLY.has(cmd) && !(cmd in routes),
    );
    expect(missing).toEqual([]);
  });

  it("does not route commands the backend no longer registers", () => {
    const registered = new Set(registeredCommands());
    const stale = Object.keys(routes).filter((cmd) => !registered.has(cmd));
    expect(stale).toEqual([]);
  });

  it("finds the commands it is checking against", () => {
    // Guards the parsing above: an empty list would make both tests vacuous.
    expect(registeredCommands().length).toBeGreaterThan(5);
    expect(registeredCommands()).toContain("load_session");
  });
});

describe("in-flight invoke counter", () => {
  it("counts a call while it is pending and clears it afterwards", async () => {
    const { invoke, inFlightInvokeCount } = await import("./invoke");
    expect(inFlightInvokeCount()).toBe(0);

    // No backend here, so the call rejects — the counter must still come back
    // down, or the webview recycle would wait out its timeout every time.
    const pending = invoke("get_settings").catch(() => {});
    expect(inFlightInvokeCount()).toBe(1);

    await pending;
    expect(inFlightInvokeCount()).toBe(0);
  });

  it("counts concurrent calls", async () => {
    const { invoke, inFlightInvokeCount } = await import("./invoke");
    const calls = [invoke("get_settings").catch(() => {}), invoke("list_sessions").catch(() => {})];
    expect(inFlightInvokeCount()).toBe(2);

    await Promise.all(calls);
    expect(inFlightInvokeCount()).toBe(0);
  });
});
