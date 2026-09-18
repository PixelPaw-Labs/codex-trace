import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CodexSessionInfo, DateGroupCount } from "../../shared/types";
import { timeAgo } from "../../shared/format";
import { sessionDisplayName } from "../lib/sessionDisplay";
import { OngoingDots } from "./OngoingDots";
import { isNearEnd } from "./SessionPicker";

interface SidebarTreeProps {
  sessions: CodexSessionInfo[];
  selectedPath: string | null;
  collapsedDates: Set<string>;
  /** How many sessions each date really has, counted across the whole directory rather
   * than across the batches fetched so far. */
  groupCounts?: DateGroupCount[];
  loadingMore?: boolean;
  hasMore?: boolean;
  onSelectSession: (info: CodexSessionInfo) => void;
  onToggleDate: (dateGroup: string) => void;
  onReachEnd?: () => void;
}

interface Lineage {
  /** Each orchestrator's session id → the loaded sessions it spawned, newest first. */
  workers: Map<string, CodexSessionInfo[]>;
  /** Sessions drawn under an orchestrator, so the date groups can leave them out. */
  nested: Set<string>;
}

function buildLineage(sessions: CodexSessionInfo[]): Lineage {
  const loaded = new Set(sessions.map((s) => s.id));
  const workers = new Map<string, CodexSessionInfo[]>();
  const nested = new Set<string>();
  for (const s of sessions) {
    const parentId = s.parent_session_id;
    // A session whose orchestrator has not been fetched yet stays at the top level rather
    // than vanishing — the orchestrator can be thousands of rows further down the list.
    if (!parentId || !loaded.has(parentId)) continue;
    const siblings = workers.get(parentId);
    if (siblings) siblings.push(s);
    else workers.set(parentId, [s]);
    nested.add(s.id);
  }
  return { workers, nested };
}

/** Group top-level sessions by date_group, preserving order. */
function groupByDate(
  sessions: CodexSessionInfo[],
  nested: Set<string>,
): Map<string, CodexSessionInfo[]> {
  const map = new Map<string, CodexSessionInfo[]>();
  for (const s of sessions) {
    if (nested.has(s.id)) continue;
    const dg = s.date_group || "unknown";
    if (!map.has(dg)) map.set(dg, []);
    map.get(dg)!.push(s);
  }
  return map;
}

interface SessionRowProps {
  session: CodexSessionInfo;
  /** 0 for a top-level session, one more for each orchestrator above it. */
  depth: number;
  workers: Map<string, CodexSessionInfo[]>;
  expandedWorkers: Set<string>;
  selectedPath: string | null;
  onSelectSession: (info: CodexSessionInfo) => void;
  onToggleWorkers: (e: React.MouseEvent, sessionId: string) => void;
}

function SessionRow({
  session,
  depth,
  workers,
  expandedWorkers,
  selectedPath,
  onSelectSession,
  onToggleWorkers,
}: SessionRowProps) {
  const spawned = workers.get(session.id);
  const expanded = expandedWorkers.has(session.id);
  const isChild = depth > 0;

  return (
    <div>
      <div
        className={[
          "sidebar-tree__session",
          isChild ? "sidebar-tree__session--child" : "",
          session.path === selectedPath ? "sidebar-tree__session--selected" : "",
          session.is_ongoing ? "sidebar-tree__session--ongoing" : "",
        ]
          .filter(Boolean)
          .join(" ")}
        style={isChild ? ({ "--tree-depth": depth } as React.CSSProperties) : undefined}
        onClick={() => onSelectSession(session)}
        role="button"
        tabIndex={0}
        onKeyDown={(e) => {
          if (e.key === "Enter") onSelectSession(session);
        }}
      >
        <div className="sidebar-tree__session-row">
          {isChild && (
            <span className="sidebar-tree__badge sidebar-tree__badge--worker">worker</span>
          )}
          <span className="sidebar-tree__session-label">{sessionDisplayName(session)}</span>
          {session.is_ongoing && <OngoingDots count={1} />}
          <span className="sidebar-tree__time">{timeAgo(session.start_time)}</span>
        </div>
        {((!isChild && session.is_external_worker) || spawned) && (
          <div className="sidebar-tree__session-meta">
            {!isChild && session.is_external_worker && (
              <span className="sidebar-tree__badge sidebar-tree__badge--external-worker">
                worker
              </span>
            )}
            {spawned && (
              <button
                className="sidebar-tree__workers-toggle"
                onClick={(e) => onToggleWorkers(e, session.id)}
              >
                {expanded ? "▼" : "▶"} {spawned.length} workers
              </button>
            )}
          </div>
        )}
      </div>

      {spawned &&
        expanded &&
        spawned.map((w) => (
          <SessionRow
            key={w.path}
            session={w}
            depth={depth + 1}
            workers={workers}
            expandedWorkers={expandedWorkers}
            selectedPath={selectedPath}
            onSelectSession={onSelectSession}
            onToggleWorkers={onToggleWorkers}
          />
        ))}
    </div>
  );
}

export function SidebarTree({
  sessions,
  selectedPath,
  collapsedDates,
  groupCounts,
  loadingMore = false,
  hasMore = false,
  onSelectSession,
  onToggleDate,
  onReachEnd,
}: SidebarTreeProps) {
  const [expandedWorkers, setExpandedWorkers] = useState<Set<string>>(new Set());
  const treeRef = useRef<HTMLDivElement>(null);

  const lineage = useMemo(() => buildLineage(sessions), [sessions]);
  const grouped = useMemo(() => groupByDate(sessions, lineage.nested), [sessions, lineage]);
  const totalByDate = useMemo(
    () => new Map((groupCounts ?? []).map((g) => [g.date_group, g.count])),
    [groupCounts],
  );

  const handleScroll = useCallback(() => {
    const el = treeRef.current;
    if (el && isNearEnd(el)) onReachEnd?.();
  }, [onReachEnd]);

  // Re-checked whenever the number of rows changes: a batch that does not fill the
  // sidebar leaves nothing to scroll, so no scroll event would ever arrive to ask for
  // the rest and the tree would strand at one batch.
  useEffect(() => {
    const el = treeRef.current;
    if (!el || !hasMore) return;
    if (sessions.length === 0 || isNearEnd(el)) onReachEnd?.();
  }, [hasMore, sessions.length, onReachEnd]);

  const handleToggleDate = useCallback(
    (e: React.MouseEvent, dateGroup: string) => {
      e.stopPropagation();
      onToggleDate(dateGroup);
    },
    [onToggleDate],
  );

  const handleToggleWorkers = useCallback((e: React.MouseEvent, sessionId: string) => {
    e.stopPropagation();
    setExpandedWorkers((prev) => {
      const next = new Set(prev);
      if (next.has(sessionId)) next.delete(sessionId);
      else next.add(sessionId);
      return next;
    });
  }, []);

  if (sessions.length === 0) {
    return (
      <div className="sidebar-tree sidebar-tree--empty">
        <span className="sidebar-tree__empty">No sessions</span>
      </div>
    );
  }

  return (
    <div ref={treeRef} className="sidebar-tree" onScroll={handleScroll}>
      {Array.from(grouped.entries()).map(([dateGroup, group]) => {
        const collapsed = collapsedDates.has(dateGroup);
        return (
          <div key={dateGroup} className="sidebar-tree__group">
            <div
              className="sidebar-tree__date-header"
              onClick={(e) => handleToggleDate(e, dateGroup)}
              role="button"
              tabIndex={0}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") onToggleDate(dateGroup);
              }}
            >
              <span className="sidebar-tree__chevron">{collapsed ? "▶" : "▼"}</span>
              <span className="sidebar-tree__date">{dateGroup}</span>
              <span className="sidebar-tree__count">
                {totalByDate.get(dateGroup) ?? group.length}
              </span>
            </div>

            {!collapsed &&
              group.map((s) => (
                <SessionRow
                  key={s.path}
                  session={s}
                  depth={0}
                  workers={lineage.workers}
                  expandedWorkers={expandedWorkers}
                  selectedPath={selectedPath}
                  onSelectSession={onSelectSession}
                  onToggleWorkers={handleToggleWorkers}
                />
              ))}
          </div>
        );
      })}

      {hasMore && (
        <div className="sidebar-tree__loading-more">
          {loadingMore ? "Loading more…" : "Scroll for more"}
        </div>
      )}
    </div>
  );
}
