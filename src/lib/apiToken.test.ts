import { beforeEach, describe, expect, it, vi } from "vitest";

const { getApiToken, setApiToken, onApiTokenChange, authHeaders, withTokenQuery } =
  await import("./apiToken");

describe("apiToken", () => {
  beforeEach(() => {
    setApiToken(null);
  });

  it("starts empty when the plugin injected nothing", () => {
    expect(getApiToken()).toBeNull();
    expect(authHeaders()).toEqual({});
  });

  it("sends the credential as a header once set", () => {
    setApiToken("abc.def.ghi");
    expect(authHeaders()).toEqual({ "X-CodexTrace-Token": "abc.def.ghi" });
  });

  it("treats an empty string as no credential", () => {
    setApiToken("");
    expect(getApiToken()).toBeNull();
    expect(authHeaders()).toEqual({});
  });

  it("appends the credential as a query parameter, url-encoded", () => {
    setApiToken("a+b/c");
    expect(withTokenQuery("http://x/api/events")).toBe("http://x/api/events?token=a%2Bb%2Fc");
  });

  it("uses & when the url already has a query string", () => {
    setApiToken("t");
    expect(withTokenQuery("http://x/api/events?a=1")).toBe("http://x/api/events?a=1&token=t");
  });

  it("leaves the url alone when there is no credential", () => {
    expect(withTokenQuery("http://x/api/events")).toBe("http://x/api/events");
  });

  it("notifies subscribers when the value changes", () => {
    const seen: (string | null)[] = [];
    const off = onApiTokenChange((t) => seen.push(t));

    setApiToken("first");
    setApiToken("second");
    expect(seen).toEqual(["first", "second"]);

    off();
    setApiToken("third");
    expect(seen).toEqual(["first", "second"]);
  });

  it("does not notify when the value is unchanged", () => {
    setApiToken("same");
    const listener = vi.fn();
    onApiTokenChange(listener);

    setApiToken("same");
    expect(listener).not.toHaveBeenCalled();
  });
});
