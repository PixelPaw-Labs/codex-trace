import { useState, useCallback, useEffect, useRef } from "react";
import { Virtuoso, type VirtuosoHandle } from "react-virtuoso";
import type { TurnSummary } from "../../shared/types";
import { formatDuration, formatTokens } from "../../shared/format";
import { formatExactTime } from "../lib/format";
import { OngoingDots } from "./OngoingDots";
import {
  UserIcon,
  CodexIcon,
  ForwardIcon,
  TokensIcon,
  ToolsIcon,
  DurationIcon,
  ThinkingIcon,
} from "./Icons";

interface TurnListProps {
  summaries: TurnSummary[];
  selectedIndex: number;
  onSelectTurn: (index: number) => void;
}

function statusIcon(status: TurnSummary["status"]): string {
  if (status === "complete") return "✓";
  if (status === "aborted") return "✗";
  if (status === "cancelled") return "⊘";
  return "!";
}

/**
 * How to bring the selected row into view given the rows currently rendered.
 * `null` when no scroll is needed. Above the window → align to the top so the
 * user header stays visible; below → align to the end.
 */
export function selectionScrollTarget(
  selectedIndex: number,
  range: { startIndex: number; endIndex: number },
): { index: number; align: "start" | "end" } | null {
  if (selectedIndex < 0) return null;
  if (selectedIndex < range.startIndex) return { index: selectedIndex, align: "start" };
  if (selectedIndex > range.endIndex) return { index: selectedIndex, align: "end" };
  return null;
}

/** The row timestamp: when the turn finished, else its last agent message. */
function agentTimestamp(summary: TurnSummary): string | null {
  if (summary.completed_at) {
    return formatExactTime(new Date(summary.completed_at * 1000).toISOString());
  }
  return summary.last_agent_timestamp ? formatExactTime(summary.last_agent_timestamp) : null;
}

/** Per-render data threaded to the row renderer via Virtuoso's `context`, so the
 * renderer can stay a stable module-level function instead of an inline one. */
interface TurnRowContext {
  summaries: TurnSummary[];
  selectedIndex: number;
  expandedUsers: Set<number>;
  expandedCodex: Set<number>;
  onToggleUser: (index: number) => void;
  onCodexClick: (index: number) => void;
  onSelectTurn: (index: number) => void;
}

function renderTurnRow(i: number, _data: unknown, ctx: TurnRowContext) {
  const summary = ctx.summaries[i];
  if (!summary) return null;
  const isSelected = i === ctx.selectedIndex;
  const userMsg = summary.user_message ?? "";
  const userExpanded = ctx.expandedUsers.has(i);
  const userTs = summary.started_at
    ? formatExactTime(new Date(summary.started_at * 1000).toISOString())
    : null;
  const agentTs = agentTimestamp(summary);

  return (
    <div className="turn-list__turn">
      {/* User message */}
      <div
        className={`message message--user${isSelected ? " message--selected" : ""}`}
        onClick={() => ctx.onToggleUser(i)}
        role="button"
        tabIndex={0}
        onKeyDown={(e) => {
          if (e.key === "Enter") ctx.onToggleUser(i);
        }}
      >
        <div className="message__header">
          <span className="message__role-icon">
            <UserIcon />
          </span>
          <span className="message__role message__role--user">User</span>
          {userTs && <span className="message__timestamp">{userTs}</span>}
        </div>
        {userMsg && (
          <div className={`message__content${!userExpanded ? " message__content--collapsed" : ""}`}>
            {userMsg}
          </div>
        )}
      </div>

      {/* Agent (Codex) message */}
      <div
        className={`message message--claude${isSelected ? " message--selected" : ""}`}
        onClick={() => ctx.onCodexClick(i)}
        role="button"
        tabIndex={0}
        onKeyDown={(e) => {
          if (e.key === "Enter") ctx.onSelectTurn(i);
        }}
      >
        <div className="message__header">
          <span className="message__role-icon">
            <CodexIcon />
          </span>
          <span className="message__role message__role--claude">Codex</span>
          {summary.status === "ongoing" && <OngoingDots />}
          {summary.has_detail && (
            <button
              className="message__detail-btn"
              onClick={(e) => {
                e.stopPropagation();
                ctx.onSelectTurn(i);
              }}
            >
              Detail <ForwardIcon />
            </button>
          )}
          {agentTs && <span className="message__timestamp">{agentTs}</span>}
        </div>

        {summary.agent_preview && (
          <div
            className={`message__content${!ctx.expandedCodex.has(i) ? " message__content--collapsed" : ""}`}
          >
            {summary.agent_preview}
          </div>
        )}

        {((summary.total_tokens ?? 0) > 0 ||
          summary.tool_call_count > 0 ||
          summary.duration_ms !== null) && (
          <div className="message__stats">
            {summary.status !== "ongoing" && (
              <span className={`message__stat turn-list__status--${summary.status}`}>
                {statusIcon(summary.status)}
              </span>
            )}
            {(summary.total_tokens ?? 0) > 0 && (
              <span className="message__stat">
                <span className="message__stat-icon">
                  <TokensIcon />
                </span>
                {formatTokens(summary.total_tokens!)} tok
              </span>
            )}
            {summary.tool_call_count > 0 && (
              <span className="message__stat">
                <span className="message__stat-icon">
                  <ToolsIcon />
                </span>
                {summary.tool_call_count} tool{summary.tool_call_count > 1 ? "s" : ""}
              </span>
            )}
            {summary.reasoning_count > 0 && (
              <span className="message__stat">
                <span className="message__stat-icon">
                  <ThinkingIcon />
                </span>
                {summary.reasoning_count} think
              </span>
            )}
            {summary.duration_ms !== null && (
              <span className="message__stat">
                <span className="message__stat-icon">
                  <DurationIcon />
                </span>
                {formatDuration(summary.duration_ms)}
              </span>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

export function TurnList({ summaries, selectedIndex, onSelectTurn }: TurnListProps) {
  const virtuosoRef = useRef<VirtuosoHandle>(null);
  const [expandedUsers, setExpandedUsers] = useState<Set<number>>(new Set());
  const [expandedCodex, setExpandedCodex] = useState<Set<number>>(new Set());
  const clickTimers = useRef<Map<number, ReturnType<typeof setTimeout>>>(new Map());

  const toggleUser = useCallback((i: number) => {
    setExpandedUsers((prev) => {
      const next = new Set(prev);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });
  }, []);

  const toggleCodex = useCallback((i: number) => {
    setExpandedCodex((prev) => {
      const next = new Set(prev);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });
  }, []);

  const handleCodexClick = useCallback(
    (i: number) => {
      if (clickTimers.current.has(i)) {
        clearTimeout(clickTimers.current.get(i)!);
        clickTimers.current.delete(i);
        onSelectTurn(i);
      } else {
        clickTimers.current.set(
          i,
          setTimeout(() => {
            clickTimers.current.delete(i);
            toggleCodex(i);
          }, 250),
        );
      }
    },
    [onSelectTurn, toggleCodex],
  );

  // The window currently on screen. Recording it is all `rangeChanged` may do:
  // it fires continuously while the user scrolls, so scrolling from here would
  // drag the list straight back to the selected row and make the list
  // impossible to scroll away from.
  const rangeRef = useRef({ startIndex: 0, endIndex: 0 });
  const handleRangeChanged = useCallback((range: { startIndex: number; endIndex: number }) => {
    rangeRef.current = range;
  }, []);

  // Keyboard navigation moves the selection without scrolling, so bring the
  // selected row back into view when — and only when — the selection changes.
  // Virtuoso owns the scroller, so this scrolls the list and nothing above it.
  useEffect(() => {
    const target = selectionScrollTarget(selectedIndex, rangeRef.current);
    if (target) virtuosoRef.current?.scrollToIndex(target);
  }, [selectedIndex]);

  if (summaries.length === 0) {
    return (
      <div className="message-list">
        <div className="message-list__empty">No turns in this session.</div>
      </div>
    );
  }

  return (
    <Virtuoso<unknown, TurnRowContext>
      ref={virtuosoRef}
      className="message-list"
      totalCount={summaries.length}
      // Keep the view pinned to the newest turn while a session is live, but
      // only when the user is already at the bottom.
      followOutput="smooth"
      // Render rows this far outside the viewport so a row is built before it
      // scrolls into view, rather than visibly appearing at the viewport edge.
      increaseViewportBy={{ top: 600, bottom: 600 }}
      rangeChanged={handleRangeChanged}
      computeItemKey={(index) => summaries[index]?.turn_id ?? index}
      context={{
        summaries,
        selectedIndex,
        expandedUsers,
        expandedCodex,
        onToggleUser: toggleUser,
        onCodexClick: handleCodexClick,
        onSelectTurn,
      }}
      itemContent={renderTurnRow}
    />
  );
}
