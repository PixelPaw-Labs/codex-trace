import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  DEFAULT_FONT_SCALE,
  FONT_SCALE_KEY,
  FONT_SCALE_PRESETS,
  MAX_FONT_SCALE,
  MIN_FONT_SCALE,
  applyFontScale,
  clampFontScale,
  formatFontScale,
  readStoredFontScale,
  storeFontScale,
} from "./fontScale";

beforeEach(() => {
  localStorage.clear();
  document.documentElement.removeAttribute("style");
});

describe("clampFontScale", () => {
  it("keeps values inside the supported range", () => {
    expect(clampFontScale(1)).toBe(1);
    expect(clampFontScale(1.25)).toBe(1.25);
  });

  it("clamps values outside the range", () => {
    expect(clampFontScale(0.1)).toBe(MIN_FONT_SCALE);
    expect(clampFontScale(99)).toBe(MAX_FONT_SCALE);
  });

  it("falls back to the default for non-finite input", () => {
    expect(clampFontScale(Number.NaN)).toBe(DEFAULT_FONT_SCALE);
    expect(clampFontScale(Number.POSITIVE_INFINITY)).toBe(DEFAULT_FONT_SCALE);
  });
});

describe("stored scale", () => {
  it("returns the default when nothing is stored", () => {
    expect(readStoredFontScale()).toBe(DEFAULT_FONT_SCALE);
  });

  it("round-trips a stored scale", () => {
    storeFontScale(1.5);
    expect(localStorage.getItem(FONT_SCALE_KEY)).toBe("1.5");
    expect(readStoredFontScale()).toBe(1.5);
  });

  it("clamps an out-of-range stored scale", () => {
    localStorage.setItem(FONT_SCALE_KEY, "8");
    expect(readStoredFontScale()).toBe(MAX_FONT_SCALE);
  });

  it("returns the default for a malformed stored value", () => {
    localStorage.setItem(FONT_SCALE_KEY, "not-a-number");
    expect(readStoredFontScale()).toBe(DEFAULT_FONT_SCALE);
  });

  it("survives storage throwing on read", () => {
    const spy = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("denied");
    });
    expect(readStoredFontScale()).toBe(DEFAULT_FONT_SCALE);
    spy.mockRestore();
  });

  it("survives storage throwing on write", () => {
    const spy = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("quota");
    });
    expect(() => storeFontScale(1.25)).not.toThrow();
    spy.mockRestore();
  });
});

describe("applyFontScale", () => {
  it("zooms the document root", () => {
    applyFontScale(1.5);
    expect(document.documentElement.style.getPropertyValue("zoom")).toBe("1.5");
  });

  it("compensates viewport height so the shell still fits the window", () => {
    applyFontScale(2);
    // CSS zoom scales vh too, so a 2x-zoomed 100vh shell would be twice the
    // window's height without this inverse.
    expect(document.documentElement.style.getPropertyValue("--app-viewport-height")).toBe("50vh");
  });

  it("clamps before applying", () => {
    applyFontScale(99);
    expect(document.documentElement.style.getPropertyValue("zoom")).toBe(String(MAX_FONT_SCALE));
  });
});

describe("formatFontScale", () => {
  it("renders a percentage label", () => {
    expect(formatFontScale(1)).toBe("100%");
    expect(formatFontScale(1.25)).toBe("125%");
    expect(formatFontScale(0.8)).toBe("80%");
  });

  it("labels every preset", () => {
    expect(FONT_SCALE_PRESETS.map(formatFontScale)).toEqual([
      "80%",
      "90%",
      "100%",
      "110%",
      "125%",
      "150%",
      "175%",
      "200%",
    ]);
  });

  it("offers only presets inside the supported range", () => {
    for (const preset of FONT_SCALE_PRESETS) {
      expect(clampFontScale(preset)).toBe(preset);
    }
  });
});
