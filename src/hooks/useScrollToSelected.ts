import { useRef, useEffect } from "react";

/**
 * Keep the selected list item in view by scrolling its own nearest scroll
 * container, never the page.
 *
 * `element.scrollIntoView()` scrolls every scrollable ancestor, and an
 * `overflow: hidden` element is still programmatically scrollable. The app
 * shell (`.app`, `#root`, `body`) is `overflow: hidden`, so any layout that
 * gives it a scroll range lets `scrollIntoView` push the toolbars off the top
 * with no scrollbar to bring them back. Adjusting the container's own
 * `scrollTop` cannot reach an ancestor; when there is no dedicated scroll
 * container we do nothing rather than risk scrolling the shell.
 */
export function useScrollToSelected(selected: number) {
  const ref = useRef<HTMLDivElement>(null);
  // The selection this hook last scrolled for. Callers move `ref` onto whichever
  // item is selected and refs are not reactive, so the selected index is the
  // signal that the ref now points somewhere else — and the effect reads it
  // rather than merely listing it.
  const scrolledFor = useRef<number | null>(null);

  useEffect(() => {
    if (scrolledFor.current === selected) return;

    const el = ref.current;
    if (!el) return;
    scrolledFor.current = selected;

    let container = el.parentElement;
    while (container && container !== document.body) {
      const style = window.getComputedStyle(container);
      if (
        style.overflowY === "auto" ||
        style.overflowY === "scroll" ||
        style.overflow === "auto" ||
        style.overflow === "scroll"
      ) {
        break;
      }
      container = container.parentElement;
    }

    if (!container || container === document.body) return;

    const elRect = el.getBoundingClientRect();
    const containerRect = container.getBoundingClientRect();

    if (elRect.top < containerRect.top || el.offsetHeight > container.clientHeight) {
      container.scrollTop += elRect.top - containerRect.top;
    } else if (elRect.bottom > containerRect.bottom) {
      container.scrollTop += elRect.bottom - containerRect.bottom;
    }
  }, [selected]);

  return ref;
}
