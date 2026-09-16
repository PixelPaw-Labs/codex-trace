import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const tauriOpenUrl = vi.fn();
vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: (url: string) => tauriOpenUrl(url),
}));

let isTauri = false;
vi.mock("./isTauri", () => ({
  get isTauri() {
    return isTauri;
  },
}));

async function load() {
  vi.resetModules();
  return (await import("./openUrl")).openUrl;
}

beforeEach(() => {
  tauriOpenUrl.mockReset();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("openUrl in a plain browser", () => {
  it("opens a new tab without an opener", async () => {
    isTauri = false;
    const open = vi.spyOn(window, "open").mockReturnValue(null);
    const openUrl = await load();

    openUrl("https://example.com");

    expect(open).toHaveBeenCalledWith("https://example.com", "_blank", "noopener");
    expect(tauriOpenUrl).not.toHaveBeenCalled();
  });
});

describe("openUrl in the Tauri webview", () => {
  it("hands the URL to the system browser", async () => {
    isTauri = true;
    tauriOpenUrl.mockResolvedValue(undefined);
    const openUrl = await load();

    openUrl("https://example.com");

    expect(tauriOpenUrl).toHaveBeenCalledWith("https://example.com");
  });

  it("reports a failure instead of leaving the rejection unhandled", async () => {
    isTauri = true;
    const onError = vi.spyOn(console, "error").mockImplementation(() => {});
    tauriOpenUrl.mockRejectedValue(new Error("no opener available"));
    const openUrl = await load();

    // Returns void, so a rejection here has nowhere to go but an unhandled
    // rejection unless the wrapper catches it.
    expect(() => openUrl("https://example.com")).not.toThrow();

    await vi.waitFor(() => expect(onError).toHaveBeenCalled());
    expect(String(onError.mock.calls[0][0])).toContain("could not open the URL");
  });
});
