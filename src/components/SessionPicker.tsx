import { useRef, useMemo, useCallback, useEffect } from "react";
import type { CodexSessionInfo, IndexProgress } from "../../shared/types";
import { formatTokens, truncate } from "../../shared/format";
import { shortModel, formatExactTime } from "../lib/format";
import { sessionDisplayName } from "../lib/sessionDisplay";
import { getModelColor } from "../lib/theme";
import { OngoingDots } from "./OngoingDots";
import { useScrollToSelected } from "../hooks/useScrollToSelected";
import { TokensIcon, ForwardIcon } from "./Icons";
import { VscTerminal } from "react-icons/vsc";

interface SessionPickerProps {
  sessions: CodexSessionInfo[];
  /** How far the backend has got reading the sessions directory. */
  index: IndexProgress;
  loading: boolean;
  loadingMore?: boolean;
  hasMore?: boolean;
  searchQuery: string;
  selectedIndex: number;
  onSelectSession: (info: CodexSessionInfo) => void;
  onSearchChange: (q: string) => void;
  onReachEnd?: () => void;
}

/** How close to the bottom of a list counts as "at the end", in pixels. Generous enough
 * that the next batch is on its way before the user runs out of rows to read. */
export const LOAD_MORE_THRESHOLD_PX = 600;

/** Whether a scroller is close enough to its end to ask for the next batch. */
export function isNearEnd(el: {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
}): boolean {
  return el.scrollHeight - el.scrollTop - el.clientHeight <= LOAD_MORE_THRESHOLD_PX;
}

function groupByDate(
  sessions: CodexSessionInfo[],
): Array<{ category: string; items: CodexSessionInfo[] }> {
  const map = new Map<string, CodexSessionInfo[]>();
  for (const s of sessions) {
    const key = s.date_group || "unknown";
    if (!map.has(key)) map.set(key, []);
    map.get(key)!.push(s);
  }
  return Array.from(map.entries()).map(([category, items]) => ({ category, items }));
}

export function SessionPicker({
  sessions,
  index,
  loading,
  loadingMore = false,
  hasMore = false,
  searchQuery,
  selectedIndex,
  onSelectSession,
  onSearchChange,
  onReachEnd,
}: SessionPickerProps) {
  const listRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const selectedRef = useScrollToSelected(selectedIndex);

  const handleScroll = useCallback(() => {
    const el = listRef.current;
    if (el && isNearEnd(el)) onReachEnd?.();
  }, [onReachEnd]);

  // Re-checked whenever the number of rows changes: a batch that does not fill the
  // viewport leaves nothing to scroll, so no scroll event would ever arrive to ask for
  // the rest and the list would strand at one batch.
  useEffect(() => {
    const el = listRef.current;
    if (!el || !hasMore) return;
    if (sessions.length === 0 || isNearEnd(el)) onReachEnd?.();
  }, [hasMore, sessions.length, onReachEnd]);

  const totalTokens = useMemo(
    () => sessions.reduce((acc, s) => acc + (s.total_tokens ?? 0), 0),
    [sessions],
  );

  const dateGroups = groupByDate(sessions);
  let flatIndex = 0;

  return (
    <div className="picker">
      <div className="picker__header">
        <div className="picker__title">
          Sessions
          {totalTokens > 0 && (
            <span className="picker__total-tokens">
              <TokensIcon /> {formatTokens(totalTokens)} tok
            </span>
          )}
        </div>
        <input
          ref={searchRef}
          className="picker__search"
          type="text"
          placeholder="Search sessions…"
          value={searchQuery}
          onChange={(e) => onSearchChange(e.target.value)}
          spellCheck={false}
        />
      </div>

      <div ref={listRef} className="picker__list" onScroll={handleScroll}>
        {loading && sessions.length === 0 && <div className="picker__loading">Loading…</div>}

        {!loading && sessions.length === 0 && index.done && (
          <div className="picker__empty">
            {searchQuery ? "No matching sessions" : "No sessions found"}
          </div>
        )}

        {dateGroups.map((group) => (
          <div key={group.category}>
            <div className="picker__group-header">{group.category}</div>
            {group.items.map((s) => {
              const idx = flatIndex++;
              const isSelected = idx === selectedIndex;
              const model = shortModel(s.model ?? "");
              const modelClr = s.model ? getModelColor(s.model) : undefined;

              return (
                <div
                  key={s.path}
                  ref={isSelected ? selectedRef : undefined}
                  className={`picker__session${isSelected ? " picker__session--selected" : ""}${s.is_ongoing ? " picker__session--ongoing" : ""}`}
                  onClick={() => onSelectSession(s)}
                  role="button"
                  tabIndex={0}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") onSelectSession(s);
                  }}
                >
                  <div className="picker__session-top">
                    <span className="picker__session-icon">
                      <VscTerminal />
                    </span>
                    <span className="picker__session-preview">
                      {truncate(sessionDisplayName(s), 80)}
                    </span>
                    {s.is_ongoing && (
                      <span className="picker__session-ongoing">
                        <OngoingDots count={1} />
                        ACTIVE
                      </span>
                    )}
                    <button
                      className="message__detail-btn"
                      onClick={(e) => {
                        e.stopPropagation();
                        onSelectSession(s);
                      }}
                    >
                      Detail <ForwardIcon />
                    </button>
                  </div>
                  <div className="picker__session-meta">
                    {model && (
                      <span className="picker__session-model" style={{ color: modelClr }}>
                        {model}
                      </span>
                    )}
                    {s.git_branch && <span className="picker__session-branch">{s.git_branch}</span>}
                    {s.turn_count > 0 && (
                      <span className="picker__session-stat">{s.turn_count} turns</span>
                    )}
                    {(s.total_tokens ?? 0) > 0 && (
                      <span className="picker__session-stat">
                        {formatTokens(s.total_tokens!)} tok
                      </span>
                    )}
                    {s.spawned_worker_ids.length > 0 && (
                      <span className="picker__session-badge picker__session-badge--collab">
                        +{s.spawned_worker_ids.length} workers
                      </span>
                    )}
                    {s.is_external_worker && (
                      <span className="picker__session-badge picker__session-badge--external-worker">
                        worker
                      </span>
                    )}
                    {s.is_inline_worker && (
                      <span className="picker__session-badge picker__session-badge--inline-worker">
                        inline worker
                      </span>
                    )}
                    <span className="picker__session-time">{formatExactTime(s.start_time)}</span>
                  </div>
                </div>
              );
            })}
          </div>
        ))}

        {hasMore && (
          <div className="picker__loading-more">
            {loadingMore ? "Loading more…" : "Scroll for more"}
          </div>
        )}
      </div>
    </div>
  );
}
