import type { AgentMessage, CodexToolCall, CodexTurn } from "../../shared/types";
import { ToolCallItem } from "./ToolCallItem";
import { ComplementaryItem } from "./ComplementaryItem";
import { OngoingDots } from "./OngoingDots";
import { BackIcon, CodexIcon } from "./Icons";
import { MarkdownRenderer } from "./MarkdownRenderer";
import { shortModel, formatExactTime } from "../lib/format";
import { getContextColor, getModelColor } from "../lib/theme";
import { contextRemainingPercent, formatTokens, formatDuration } from "../../shared/format";

interface TurnDetailProps {
  turn: CodexTurn;
  expanded: Set<number>;
  onToggle: (i: number) => void;
  onBack: () => void;
  openWorkerCallId?: string | null;
  onOpenWorkerPanel?: (tool: CodexToolCall) => void;
}

export function TurnDetail({
  turn,
  expanded,
  onToggle,
  onBack,
  openWorkerCallId,
  onOpenWorkerPanel,
}: TurnDetailProps) {
  const commentary = turn.agent_messages.filter(
    (m) => m.phase !== "final_answer" && !m.is_reasoning,
  );
  const reasoning = turn.agent_messages.filter((m) => m.is_reasoning);
  const finalAnswer = turn.agent_messages.find((m) => m.phase === "final_answer");

  // Interleave commentary messages with tool calls by their stream order, so each tool call
  // shows up inline where it actually happened instead of being dumped at the end of the turn.
  // When order data is missing (old cached sessions), messages keep order 0 and tools sort last,
  // which reproduces the previous "messages first, tools after" layout.
  type TimelineItem =
    | { order: number; kind: "msg"; msg: AgentMessage }
    | { order: number; kind: "tool"; tool: CodexToolCall; index: number };
  const timeline: TimelineItem[] = [];
  commentary.forEach((msg) => {
    timeline.push({ order: msg.order ?? 0, kind: "msg", msg });
  });
  turn.tool_calls.forEach((tool, index) => {
    const order = turn.tool_call_orders?.[index] ?? Number.MAX_SAFE_INTEGER;
    timeline.push({ order, kind: "tool", tool, index });
  });
  timeline.sort((a, b) => a.order - b.order);
  const model = turn.model ? shortModel(turn.model) : "";
  const modelColor = turn.model ? getModelColor(turn.model) : undefined;

  const metaParts: string[] = [];
  if (turn.total_tokens?.total_tokens)
    metaParts.push(`${formatTokens(turn.total_tokens.total_tokens)} tok`);
  if (turn.duration_ms) metaParts.push(formatDuration(turn.duration_ms));
  const tokenInfo = turn.total_tokens;
  const contextLeftPercent = tokenInfo
    ? contextRemainingPercent(tokenInfo.context_window_tokens, tokenInfo.model_context_window)
    : null;
  const contextUsedPercent = contextLeftPercent === null ? null : 100 - contextLeftPercent;
  const contextTitle =
    tokenInfo && tokenInfo.context_window_tokens !== null
      ? `${formatTokens(tokenInfo.context_window_tokens)} / ${formatTokens(
          tokenInfo.model_context_window,
        )} context tokens`
      : undefined;

  return (
    <div className="turn-detail">
      <div className="message-detail__header">
        <button className="message-detail__back" onClick={onBack}>
          <BackIcon /> Back
        </button>
        <span className="message-detail__role-icon">
          <CodexIcon />
        </span>
        <span className="message-detail__title">Codex</span>
        {model && <span style={{ color: modelColor, fontWeight: 600, fontSize: 12 }}>{model}</span>}
        {turn.status === "ongoing" && <OngoingDots count={3} />}
        {(contextLeftPercent !== null || metaParts.length > 0) && (
          <div className="message-detail__meta">
            {contextLeftPercent !== null && contextUsedPercent !== null && (
              <div className="message-detail__context info-bar__context" title={contextTitle}>
                <span>ctx {contextLeftPercent}% left</span>
                <div className="info-bar__context-bar">
                  <div
                    className="info-bar__context-fill"
                    style={{
                      width: `${contextUsedPercent}%`,
                      backgroundColor: getContextColor(contextUsedPercent),
                    }}
                  />
                </div>
              </div>
            )}
            {metaParts.length > 0 && (
              <span className="message-detail__meta-text">{metaParts.join(" · ")}</span>
            )}
          </div>
        )}
      </div>

      <div className="turn-detail__body">
        <div className="turn-detail__content">
          {turn.error && (
            <div className="turn-detail__section turn-detail__section--error">
              <div className="turn-detail__section-label">Error</div>
              <pre className="turn-detail__error">{turn.error}</pre>
            </div>
          )}

          {reasoning.length > 0 && (
            <div className="turn-detail__section turn-detail__section--reasoning">
              <div
                className="turn-detail__section-label"
                style={{ color: "var(--reasoning-text)" }}
              >
                Reasoning (encrypted)
              </div>
              <div className="turn-detail__reasoning-note">
                (reasoning encrypted — cannot display)
              </div>
            </div>
          )}

          {timeline.length > 0 && (
            <div className="turn-detail__section turn-detail__section--activity">
              {timeline.map((item, i) =>
                item.kind === "msg" ? (
                  <ComplementaryItem key={`m-${item.msg.timestamp || i}`} msg={item.msg} />
                ) : (
                  <ToolCallItem
                    key={`t-${item.tool.call_id || item.index}`}
                    tool={item.tool}
                    expanded={expanded.has(item.index)}
                    onToggle={() => onToggle(item.index)}
                    isWorkerOpen={item.tool.call_id === openWorkerCallId}
                    onOpenWorker={onOpenWorkerPanel}
                  />
                ),
              )}
            </div>
          )}

          {finalAnswer && (
            <div className="turn-detail__section turn-detail__section--final">
              <div className="turn-detail__section-label">Final answer</div>
              <div className="turn-detail__msg">
                {finalAnswer.timestamp && (
                  <div className="turn-detail__msg-header">
                    <span className="turn-detail__msg-time">
                      {formatExactTime(finalAnswer.timestamp)}
                    </span>
                  </div>
                )}
                <div className="turn-detail__markdown">
                  <MarkdownRenderer content={finalAnswer.text} />
                </div>
              </div>
            </div>
          )}

          {turn.has_compaction && (
            <div className="turn-detail__compaction-note">Context was compacted in this turn.</div>
          )}
        </div>
      </div>
    </div>
  );
}
