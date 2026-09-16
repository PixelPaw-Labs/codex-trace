import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";

import { FONT_SCALE_KEY, MAX_FONT_SCALE, MIN_FONT_SCALE } from "../lib/fontScale";
import { useFontScale } from "./useFontScale";

function zoom() {
  return document.documentElement.style.getPropertyValue("zoom");
}

beforeEach(() => {
  localStorage.clear();
  document.documentElement.removeAttribute("style");
});

describe("useFontScale", () => {
  it("starts at 100% when nothing is stored", () => {
    const { result } = renderHook(() => useFontScale());
    expect(result.current[0]).toBe(1);
    expect(zoom()).toBe("1");
  });

  it("starts from the stored scale and applies it on mount", () => {
    localStorage.setItem(FONT_SCALE_KEY, "1.25");
    const { result } = renderHook(() => useFontScale());
    expect(result.current[0]).toBe(1.25);
    expect(zoom()).toBe("1.25");
  });

  it("applies and persists a new scale", () => {
    const { result } = renderHook(() => useFontScale());
    act(() => result.current[1](1.5));
    expect(result.current[0]).toBe(1.5);
    expect(zoom()).toBe("1.5");
    expect(localStorage.getItem(FONT_SCALE_KEY)).toBe("1.5");
  });

  it("clamps a scale beyond the supported range", () => {
    const { result } = renderHook(() => useFontScale());
    act(() => result.current[1](99));
    expect(result.current[0]).toBe(MAX_FONT_SCALE);
    act(() => result.current[1](0));
    expect(result.current[0]).toBe(MIN_FONT_SCALE);
    expect(localStorage.getItem(FONT_SCALE_KEY)).toBe(String(MIN_FONT_SCALE));
  });

  it("keeps the stored scale across remounts", () => {
    const first = renderHook(() => useFontScale());
    act(() => first.result.current[1](1.75));
    first.unmount();

    const second = renderHook(() => useFontScale());
    expect(second.result.current[0]).toBe(1.75);
    expect(zoom()).toBe("1.75");
  });
});
