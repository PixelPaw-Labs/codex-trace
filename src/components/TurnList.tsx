import { useState, useCallback, useRef } from "react";
import type { CodexTurn } from "../../shared/types";
import { formatDuration, formatTokens } from "../../shared/format";
import { formatExactTime } from "../lib/format";
import { useAutoScroll } from "../hooks/useAutoScroll";
import { useScrollToSelected } from "../hooks/useScrollToSelected";
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
  turns: CodexTurn[];
  selectedIndex: number;
  onSelectTurn: (index: number) => void;
}

function statusIcon(status: CodexTurn["status"]): string {
  if (status === "complete") return "✓";
  if (status === "aborted") return "✗";
  if (status === "cancelled") return "⊘";
  return "!";
}

export function TurnList({ turns, selectedIndex, onSelectTurn }: TurnListProps) {
  const listRef = useAutoScroll<HTMLDivElement>(turns.length);
  const selectedRef = useScrollToSelected(selectedIndex);
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

  return (
    <div ref={listRef} className="message-list">
      {turns.map((turn, i) => {
        const isSelected = i === selectedIndex;
        const userMsg = turn.user_message ?? "";
        const userExpanded = expandedUsers.has(i);
        const agentPreview =
          turn.agent_messages.find((m) => m.phase === "final_answer")?.text ??
          turn.final_answer ??
          turn.agent_messages.find((m) => !m.is_reasoning)?.text ??
          null;
        const hasDetail =
          turn.agent_messages.length > 0 ||
          turn.tool_calls.length > 0 ||
          turn.final_answer !== null;
        const reasoningCount = turn.agent_messages.filter((m) => m.is_reasoning).length;
        const userTs = turn.started_at
          ? formatExactTime(new Date(turn.started_at * 1000).toISOString())
          : null;
        const agentTs = turn.completed_at
          ? formatExactTime(new Date(turn.completed_at * 1000).toISOString())
          : turn.agent_messages.at(-1)?.timestamp
            ? formatExactTime(turn.agent_messages.at(-1)!.timestamp)
            : null;

        return (
          <div
            key={turn.turn_id}
            ref={isSelected ? selectedRef : undefined}
            className="turn-list__turn"
          >
            {/* User message */}
            <div
              className={`message message--user${isSelected ? " message--selected" : ""}`}
              onClick={() => toggleUser(i)}
              role="button"
              tabIndex={0}
              onKeyDown={(e) => {
                if (e.key === "Enter") toggleUser(i);
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
                <div
                  className={`message__content${!userExpanded ? " message__content--collapsed" : ""}`}
                >
                  {userMsg}
                </div>
              )}
            </div>

            {/* Agent (Codex) message */}
            <div
              className={`message message--claude${isSelected ? " message--selected" : ""}`}
              onClick={() => handleCodexClick(i)}
              role="button"
              tabIndex={0}
              onKeyDown={(e) => {
                if (e.key === "Enter") onSelectTurn(i);
              }}
            >
              <div className="message__header">
                <span className="message__role-icon">
                  <CodexIcon />
                </span>
                <span className="message__role message__role--claude">Codex</span>
                {turn.status === "ongoing" && <OngoingDots />}
                {hasDetail && (
                  <button
                    className="message__detail-btn"
                    onClick={(e) => {
                      e.stopPropagation();
                      onSelectTurn(i);
                    }}
                  >
                    Detail <ForwardIcon />
                  </button>
                )}
                {agentTs && <span className="message__timestamp">{agentTs}</span>}
              </div>

              {agentPreview && (
                <div
                  className={`message__content${!expandedCodex.has(i) ? " message__content--collapsed" : ""}`}
                >
                  {agentPreview}
                </div>
              )}

              {(turn.total_tokens || turn.tool_calls.length > 0 || turn.duration_ms !== null) && (
                <div className="message__stats">
                  {turn.status !== "ongoing" && (
                    <span className={`message__stat turn-list__status--${turn.status}`}>
                      {statusIcon(turn.status)}
                    </span>
                  )}
                  {(turn.total_tokens?.total_tokens ?? 0) > 0 && (
                    <span className="message__stat">
                      <span className="message__stat-icon">
                        <TokensIcon />
                      </span>
                      {formatTokens(turn.total_tokens!.total_tokens)} tok
                    </span>
                  )}
                  {turn.tool_calls.length > 0 && (
                    <span className="message__stat">
                      <span className="message__stat-icon">
                        <ToolsIcon />
                      </span>
                      {turn.tool_calls.length} tool{turn.tool_calls.length > 1 ? "s" : ""}
                    </span>
                  )}
                  {reasoningCount > 0 && (
                    <span className="message__stat">
                      <span className="message__stat-icon">
                        <ThinkingIcon />
                      </span>
                      {reasoningCount} think
                    </span>
                  )}
                  {turn.duration_ms !== null && (
                    <span className="message__stat">
                      <span className="message__stat-icon">
                        <DurationIcon />
                      </span>
                      {formatDuration(turn.duration_ms)}
                    </span>
                  )}
                </div>
              )}
            </div>
          </div>
        );
      })}
      {turns.length === 0 && <div className="message-list__empty">No turns in this session.</div>}
    </div>
  );
}
