import { renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useScrollToSelected } from "./useScrollToSelected";

function rect(top: number, bottom: number): DOMRect {
  return { top, bottom, left: 0, right: 100, width: 100, height: bottom - top } as DOMRect;
}

interface MountOptions {
  containerRect: DOMRect;
  clientHeight: number;
  scrollTop: number;
  elRect: DOMRect;
  offsetHeight: number;
  /** Wrap the container in a taller scrollable shell, as `.app` / `#root` are. */
  shell?: boolean;
  /** Give the wrapper `overflow: hidden` so it is not a scroll container. */
  noScrollContainer?: boolean;
}

function mount(opts: MountOptions) {
  const shell = document.createElement("div");
  shell.style.overflow = "hidden";
  shell.scrollTop = 0;

  const container = document.createElement("div");
  container.style.overflowY = opts.noScrollContainer ? "hidden" : "auto";
  container.getBoundingClientRect = () => opts.containerRect;
  Object.defineProperty(container, "clientHeight", {
    value: opts.clientHeight,
    configurable: true,
  });
  container.scrollTop = opts.scrollTop;

  const el = document.createElement("div");
  el.getBoundingClientRect = () => opts.elRect;
  Object.defineProperty(el, "offsetHeight", { value: opts.offsetHeight, configurable: true });
  const scrollIntoView = vi.fn();
  el.scrollIntoView = scrollIntoView;

  container.appendChild(el);
  if (opts.shell) {
    shell.appendChild(container);
    document.body.appendChild(shell);
  } else {
    document.body.appendChild(container);
  }
  return { shell, container, el, scrollIntoView };
}

function attach(el: HTMLElement, dep = 1) {
  const { result, rerender } = renderHook(({ d }) => useScrollToSelected(d), {
    initialProps: { d: 0 },
  });
  Object.defineProperty(result.current, "current", { value: el, writable: true });
  rerender({ d: dep });
}

afterEach(() => {
  document.body.innerHTML = "";
});

describe("useScrollToSelected", () => {
  it("returns a ref object", () => {
    const { result } = renderHook(() => useScrollToSelected(0));
    expect(result.current).toHaveProperty("current");
  });

  it("scrolls the container up when the item sits above it", () => {
    const { container, el, scrollIntoView } = mount({
      containerRect: rect(100, 300),
      clientHeight: 200,
      scrollTop: 500,
      elRect: rect(50, 90),
      offsetHeight: 40,
    });

    attach(el);

    expect(container.scrollTop).toBe(450);
    expect(scrollIntoView).not.toHaveBeenCalled();
  });

  it("scrolls the container down when the item sits below it", () => {
    const { container, el, scrollIntoView } = mount({
      containerRect: rect(100, 300),
      clientHeight: 200,
      scrollTop: 0,
      elRect: rect(320, 360),
      offsetHeight: 40,
    });

    attach(el);

    expect(container.scrollTop).toBe(60);
    expect(scrollIntoView).not.toHaveBeenCalled();
  });

  it("aligns the top when the item is taller than the container", () => {
    const { container, el } = mount({
      containerRect: rect(100, 300),
      clientHeight: 200,
      scrollTop: 0,
      elRect: rect(150, 650),
      offsetHeight: 500,
    });

    attach(el);

    expect(container.scrollTop).toBe(50);
  });

  it("leaves a fully visible item alone", () => {
    const { container, el, scrollIntoView } = mount({
      containerRect: rect(100, 300),
      clientHeight: 200,
      scrollTop: 40,
      elRect: rect(150, 190),
      offsetHeight: 40,
    });

    attach(el);

    expect(container.scrollTop).toBe(40);
    expect(scrollIntoView).not.toHaveBeenCalled();
  });

  it("never scrolls the overflow-hidden shell around the container", () => {
    const { shell, container, el } = mount({
      containerRect: rect(100, 300),
      clientHeight: 200,
      scrollTop: 0,
      elRect: rect(320, 360),
      offsetHeight: 40,
      shell: true,
    });

    attach(el);

    expect(container.scrollTop).toBe(60);
    expect(shell.scrollTop).toBe(0);
  });

  it("does nothing when there is no dedicated scroll container", () => {
    const { container, el, scrollIntoView } = mount({
      containerRect: rect(100, 300),
      clientHeight: 200,
      scrollTop: 0,
      elRect: rect(320, 360),
      offsetHeight: 40,
      noScrollContainer: true,
    });

    attach(el);

    expect(container.scrollTop).toBe(0);
    expect(scrollIntoView).not.toHaveBeenCalled();
  });

  it("does nothing while the ref is empty", () => {
    const { result, rerender } = renderHook(({ d }) => useScrollToSelected(d), {
      initialProps: { d: 0 },
    });
    expect(() => rerender({ d: 1 })).not.toThrow();
    expect(result.current.current).toBeNull();
  });
  it("does not re-scroll when the selection has not changed", () => {
    const { container, el } = mount({
      containerRect: rect(100, 300),
      clientHeight: 200,
      scrollTop: 0,
      elRect: rect(320, 360),
      offsetHeight: 40,
    });

    const { result, rerender } = renderHook(({ d }) => useScrollToSelected(d), {
      initialProps: { d: 0 },
    });
    Object.defineProperty(result.current, "current", { value: el, writable: true });

    rerender({ d: 3 });
    expect(container.scrollTop).toBe(60);

    // The user scrolls away by hand; a re-render for an unrelated reason must
    // not yank the list back.
    container.scrollTop = 0;
    rerender({ d: 3 });
    expect(container.scrollTop).toBe(0);
  });
});
