import { act, fireEvent, render, screen } from "@testing-library/react";
import { forwardRef, useEffect, useImperativeHandle, type ReactNode, type Ref } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { TurnSummary } from "../../shared/types";

// react-virtuoso measures rows against real geometry, which jsdom does not
// provide, so it would render nothing. Render every row instead — these tests
// are about what a row shows, not about which rows are on screen.
const scrollToIndex = vi.fn();
/** The `rangeChanged` callback the component handed to Virtuoso, so a test can
 *  simulate the user scrolling the list. */
let rangeChanged: ((range: { startIndex: number; endIndex: number }) => void) | null = null;

vi.mock("react-virtuoso", async () => {
  const actual = await vi.importActual<typeof import("react-virtuoso")>("react-virtuoso");
  return {
    ...actual,
    Virtuoso: forwardRef(function VirtuosoMock(
      {
        totalCount,
        itemContent,
        className,
        context,
        rangeChanged: onRangeChanged,
      }: {
        totalCount: number;
        itemContent: (index: number, data: unknown, context: unknown) => ReactNode;
        className?: string;
        context?: unknown;
        rangeChanged?: (range: { startIndex: number; endIndex: number }) => void;
      },
      ref: Ref<unknown>,
    ) {
      useImperativeHandle(ref, () => ({ scrollToIndex }));
      useEffect(() => {
        rangeChanged = onRangeChanged ?? null;
      });
      return (
        <div className={className}>
          {Array.from({ length: totalCount }, (_, i) => (
            <div key={i}>{itemContent(i, undefined, context)}</div>
          ))}
        </div>
      );
    }),
  };
});

const { TurnList, selectionScrollTarget } = await import("./TurnList");

function makeSummary(overrides: Partial<TurnSummary> = {}): TurnSummary {
  return {
    turn_id: "turn-1",
    status: "complete",
    started_at: 1745661600,
    completed_at: 1745661660,
    duration_ms: 60000,
    user_message: "Hello Codex",
    agent_preview: "Hi there!",
    last_agent_timestamp: "2026-04-26T10:01:00Z",
    tool_call_count: 0,
    reasoning_count: 0,
    total_tokens: 150,
    model: "gpt-4",
    has_detail: true,
    ...overrides,
  };
}

function renderList(summaries: TurnSummary[], selectedIndex = -1, onSelectTurn = vi.fn()) {
  render(
    <TurnList summaries={summaries} selectedIndex={selectedIndex} onSelectTurn={onSelectTurn} />,
  );
  return onSelectTurn;
}

describe("TurnList", () => {
  it("shows empty state message when there are no turns", () => {
    renderList([]);
    expect(screen.getByText("No turns in this session.")).toBeInTheDocument();
  });

  it("renders the user message text", () => {
    renderList([makeSummary()]);
    expect(screen.getByText("Hello Codex")).toBeInTheDocument();
  });

  it("renders the agent preview", () => {
    renderList([makeSummary()]);
    expect(screen.getByText("Hi there!")).toBeInTheDocument();
  });

  it("shows tool count for a single tool call", () => {
    renderList([makeSummary({ tool_call_count: 1 })]);
    expect(screen.getByText("1 tool")).toBeInTheDocument();
  });

  it("pluralises tool count for multiple tool calls", () => {
    renderList([makeSummary({ tool_call_count: 2 })]);
    expect(screen.getByText("2 tools")).toBeInTheDocument();
  });

  it("shows ongoing dot for an ongoing turn", () => {
    renderList([makeSummary({ status: "ongoing", completed_at: null })]);
    expect(document.querySelector(".ongoing-dots")).toBeInTheDocument();
  });

  it("does not show ongoing dot for a completed turn", () => {
    renderList([makeSummary()]);
    expect(document.querySelector(".ongoing-dots")).not.toBeInTheDocument();
  });

  it("shows token stat when total_tokens is set", () => {
    renderList([makeSummary()]);
    expect(screen.getByText("150 tok")).toBeInTheDocument();
  });

  it("shows duration stat when duration_ms is set", () => {
    renderList([makeSummary()]);
    expect(screen.getByText("1m")).toBeInTheDocument();
  });

  it("calls onSelectTurn with the turn index when Detail button is clicked", () => {
    const onSelect = renderList([makeSummary()]);
    fireEvent.click(screen.getByText(/Detail/));
    expect(onSelect).toHaveBeenCalledWith(0);
  });

  it("hides the Detail button for a turn with nothing to show", () => {
    renderList([makeSummary({ has_detail: false })]);
    expect(screen.queryByText(/Detail/)).not.toBeInTheDocument();
  });

  it("applies selected class to the currently selected turn", () => {
    renderList([makeSummary()], 0);
    expect(document.querySelectorAll(".message--selected").length).toBeGreaterThan(0);
  });

  it("shows reasoning count when reasoning messages are present", () => {
    renderList([makeSummary({ reasoning_count: 1 })]);
    expect(screen.getByText("1 think")).toBeInTheDocument();
  });

  it("falls back to the last agent timestamp when the turn never completed", () => {
    renderList([makeSummary({ completed_at: null, last_agent_timestamp: "2026-04-26T10:01:00Z" })]);
    // Two timestamps render: the user header and the agent header.
    expect(document.querySelectorAll(".message__timestamp").length).toBe(2);
  });

  it("renders every turn it is given", () => {
    renderList([
      makeSummary({ turn_id: "a", user_message: "first" }),
      makeSummary({ turn_id: "b", user_message: "second" }),
    ]);
    expect(screen.getByText("first")).toBeInTheDocument();
    expect(screen.getByText("second")).toBeInTheDocument();
  });
});

describe("selectionScrollTarget", () => {
  const range = { startIndex: 10, endIndex: 20 };

  it("does not scroll when the selection is already visible", () => {
    expect(selectionScrollTarget(15, range)).toBeNull();
    expect(selectionScrollTarget(10, range)).toBeNull();
    expect(selectionScrollTarget(20, range)).toBeNull();
  });

  it("aligns to the top when the selection is above the window", () => {
    expect(selectionScrollTarget(3, range)).toEqual({ index: 3, align: "start" });
  });

  it("aligns to the end when the selection is below the window", () => {
    expect(selectionScrollTarget(30, range)).toEqual({ index: 30, align: "end" });
  });

  it("does not scroll when nothing is selected", () => {
    expect(selectionScrollTarget(-1, range)).toBeNull();
  });
});

describe("TurnList scrolling", () => {
  const many = Array.from({ length: 60 }, (_, i) =>
    makeSummary({ turn_id: `t${i}`, user_message: `prompt ${i}` }),
  );

  beforeEach(() => {
    scrollToIndex.mockClear();
    rangeChanged = null;
  });

  it("does not scroll when the user scrolls the selection out of view", () => {
    render(<TurnList summaries={many} selectedIndex={0} onSelectTurn={vi.fn()} />);
    scrollToIndex.mockClear();

    // The user wheels down: Virtuoso reports the new window continuously.
    act(() => rangeChanged?.({ startIndex: 20, endIndex: 30 }));
    act(() => rangeChanged?.({ startIndex: 30, endIndex: 40 }));

    // Scrolling from here would drag the list straight back to turn 0 and make
    // the list impossible to scroll away from.
    expect(scrollToIndex).not.toHaveBeenCalled();
  });

  it("brings the selection back into view when the selection moves", () => {
    const { rerender } = render(
      <TurnList summaries={many} selectedIndex={0} onSelectTurn={vi.fn()} />,
    );
    act(() => rangeChanged?.({ startIndex: 0, endIndex: 10 }));
    scrollToIndex.mockClear();

    rerender(<TurnList summaries={many} selectedIndex={40} onSelectTurn={vi.fn()} />);

    expect(scrollToIndex).toHaveBeenCalledWith({ index: 40, align: "end" });
  });

  it("leaves the list alone when the new selection is already on screen", () => {
    const { rerender } = render(
      <TurnList summaries={many} selectedIndex={12} onSelectTurn={vi.fn()} />,
    );
    act(() => rangeChanged?.({ startIndex: 10, endIndex: 20 }));
    scrollToIndex.mockClear();

    rerender(<TurnList summaries={many} selectedIndex={15} onSelectTurn={vi.fn()} />);

    expect(scrollToIndex).not.toHaveBeenCalled();
  });
});
