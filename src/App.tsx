import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import type { ViewState, CodexSessionInfo, CodexToolCall } from "../shared/types";
import { useSession } from "./hooks/useSession";
import { usePicker, resolveSessionsDir } from "./hooks/usePicker";
import { useToggleSet } from "./hooks/useToggleSet";
import { useFontScale } from "./hooks/useFontScale";
import { useKeyboard } from "./hooks/useKeyboard";
import { SidebarTree } from "./components/SidebarTree";
import { SessionPicker } from "./components/SessionPicker";
import { TurnList } from "./components/TurnList";
import { TurnDetail } from "./components/TurnDetail";
import { WorkerPanel } from "./components/WorkerPanel";
import { InfoBar } from "./components/InfoBar";
import { KeybindBar } from "./components/KeybindBar";
import { ViewToolbar } from "./components/ViewToolbar";
import { ResizeHandle } from "./components/ResizeHandle";
import { SettingsModal } from "./components/SettingsModal";
import {
  shouldRecycle,
  saveRestoreState,
  takeRestoreState,
  reloadWebview,
} from "./lib/webviewRecycle";

function findToolByCallId(tools: CodexToolCall[], callId: string): CodexToolCall | null {
  for (const tool of tools) {
    if (tool.call_id === callId) return tool;
    const childTurns = tool.worker_session?.turns ?? [];
    for (const turn of childTurns) {
      const found = findToolByCallId(turn.tool_calls, callId);
      if (found) return found;
    }
  }
  return null;
}

export function App() {
  const [view, setView] = useState<ViewState>("picker");
  const [selectedTurn, setSelectedTurn] = useState(0);
  const [pickerSelected, setPickerSelected] = useState(0);
  const [showKeybinds, setShowKeybinds] = useState(true);
  const [sidebarWidth, setSidebarWidth] = useState(200);
  const [showSettings, setShowSettings] = useState(false);
  const [fontScale, setFontScale] = useFontScale();
  const [collapsedDates, setCollapsedDates] = useState<Set<string>>(new Set());
  const [workerPanelWidth, setWorkerPanelWidth] = useState(380);
  const [workerPanelCallId, setWorkerPanelCallId] = useState<string | null>(null);

  const session = useSession();
  const picker = usePicker();
  const {
    set: expandedTools,
    toggle: toggleTool,
    clear: clearTools,
    addAll: addAllTools,
  } = useToggleSet();

  const { loadSession } = session;
  const { discoverSessions, updateSessionOngoing } = picker;

  // Auto-discover sessions on mount
  const discoveredRef = useRef(false);
  useEffect(() => {
    if (discoveredRef.current) return;
    discoveredRef.current = true;
    resolveSessionsDir()
      .then((dir) => {
        if (dir) discoverSessions(dir);
      })
      .catch(() => setShowSettings(true));
  }, [discoverSessions]);

  // Sync session watcher ongoing status into picker
  useEffect(() => {
    if (session.sessionPath) {
      updateSessionOngoing(session.sessionPath, session.session?.is_ongoing ?? false);
    }
  }, [session.sessionPath, session.session?.is_ongoing, updateSessionOngoing]);

  // Changing view always dismisses the worker panel — it is only meaningful inside
  // the detail view, for the turn currently selected. Clearing it here, at the event
  // that causes the change, avoids doing it from an effect (react(set-state-in-effect)).
  const changeView = useCallback((next: ViewState) => {
    setView(next);
    setWorkerPanelCallId(null);
  }, []);

  const openSessionByPath = useCallback(
    (path: string) => {
      loadSession(path);
      changeView("list");
      setSelectedTurn(0);
      clearTools();
    },
    [loadSession, clearTools, changeView],
  );

  // How many sessions have been opened this page lifetime, for deciding when to
  // recycle the webview (see lib/webviewRecycle.ts for why).
  const switchCountRef = useRef(0);

  const handleSelectSession = useCallback(
    (info: CodexSessionInfo) => {
      switchCountRef.current += 1;
      if (shouldRecycle(switchCountRef.current)) {
        saveRestoreState({ sessionPath: info.path });
        // The reload waits for in-flight invokes to settle first, so it is
        // async. If it ever throws, open the session normally rather than
        // silently doing nothing.
        void reloadWebview().catch(() => openSessionByPath(info.path));
        return;
      }
      openSessionByPath(info.path);
    },
    [openSessionByPath],
  );

  // Restore whichever session was open right before a memory-driven reload, so
  // the reload is not disruptive.
  const restoredRef = useRef(false);
  useEffect(() => {
    if (restoredRef.current) return;
    restoredRef.current = true;
    const pending = takeRestoreState();
    // The "synchronizing with an external system" case the rule carves out:
    // takeRestoreState() reads and *consumes* session storage, so it cannot run
    // during render, and opening the session is inherently a state update.
    // oxlint-disable-next-line react/set-state-in-effect
    if (pending) openSessionByPath(pending.sessionPath);
  }, [openSessionByPath]);

  const handleOpenDetail = useCallback(
    (index: number) => {
      setSelectedTurn(index);
      changeView("detail");
    },
    [changeView],
  );

  // The detail view is the only place a turn's bodies are needed, so they are
  // fetched when it opens rather than with the session.
  const ensureTurn = session.ensureTurn;
  useEffect(() => {
    if (view !== "detail") return;
    void ensureTurn(selectedTurn);
  }, [view, selectedTurn, ensureTurn]);

  const handleToggleDate = useCallback((dateGroup: string) => {
    setCollapsedDates((prev) => {
      const next = new Set(prev);
      if (next.has(dateGroup)) next.delete(dateGroup);
      else next.add(dateGroup);
      return next;
    });
  }, []);

  const summaries = session.summaries;
  const selectedTurnData = session.turns.get(selectedTurn) ?? null;
  const workerPanelTool = useMemo(() => {
    if (!workerPanelCallId || !selectedTurnData) return null;
    return findToolByCallId(selectedTurnData.tool_calls, workerPanelCallId);
  }, [selectedTurnData, workerPanelCallId]);

  const expandAll = useCallback(() => {
    if (view === "detail" && selectedTurnData) {
      addAllTools(selectedTurnData.tool_calls.map((_, i) => i));
    }
  }, [view, selectedTurnData, addAllTools]);

  const collapseAll = useCallback(() => clearTools(), [clearTools]);

  const goToSessions = useCallback(() => changeView("picker"), [changeView]);

  const closeWorkerPanel = useCallback(() => setWorkerPanelCallId(null), []);

  const handleOpenWorkerPanel = useCallback((tool: CodexToolCall) => {
    if (!tool.worker_session) return;
    setWorkerPanelCallId((current) => (current === tool.call_id ? null : tool.call_id));
  }, []);

  // A panel whose tool call no longer resolves to a worker session (e.g. the selected
  // turn changed under it) is simply not shown, derived during render.
  const activeWorkerPanelTool =
    view === "detail" && workerPanelTool?.worker_session ? workerPanelTool : null;

  // Keyboard navigation
  useKeyboard({
    j: () => {
      if (view === "list") setSelectedTurn((i) => Math.min(i + 1, summaries.length - 1));
      if (view === "picker") setPickerSelected((i) => Math.min(i + 1, picker.sessions.length - 1));
    },
    k: () => {
      if (view === "list") setSelectedTurn((i) => Math.max(i - 1, 0));
      if (view === "picker") setPickerSelected((i) => Math.max(i - 1, 0));
    },
    Enter: () => {
      if (view === "list" && summaries.length > 0) handleOpenDetail(selectedTurn);
      if (view === "picker" && picker.sessions.length > 0)
        handleSelectSession(picker.sessions[pickerSelected]);
    },
    Escape: () => {
      if (activeWorkerPanelTool) {
        closeWorkerPanel();
        return;
      }
      if (view === "detail") changeView("list");
      else if (view === "list") changeView("picker");
    },
    q: () => {
      if (activeWorkerPanelTool) {
        closeWorkerPanel();
        return;
      }
      if (view === "detail") changeView("list");
      else if (view === "list") changeView("picker");
    },
    ",": () => setShowSettings(true),
    "?": () => setShowKeybinds((p) => !p),
  });

  return (
    <div className="app">
      {/* Info bar — only when session loaded and not in picker */}
      {session.sessionPath && view !== "picker" && session.session && (
        <InfoBar session={session.session} />
      )}

      {/* View toolbar */}
      <ViewToolbar
        view={view}
        hasSession={!!session.sessionPath}
        onGoToSessions={goToSessions}
        onExpandAll={expandAll}
        onCollapseAll={collapseAll}
        onOpenSettings={() => setShowSettings(true)}
      />

      <div className="app-body">
        {/* Left sidebar */}
        <div className="app__sidebar" style={{ width: sidebarWidth, minWidth: sidebarWidth }}>
          <div className="app__sidebar-header">
            <span className="app__sidebar-title">SESSIONS</span>
          </div>
          <SidebarTree
            sessions={picker.allSessions}
            selectedPath={session.sessionPath || null}
            collapsedDates={collapsedDates}
            onSelectSession={handleSelectSession}
            onToggleDate={handleToggleDate}
          />
        </div>

        <ResizeHandle onResize={setSidebarWidth} />

        {/* Main content */}
        <div className="main-content">
          {view === "picker" && (
            <SessionPicker
              sessions={picker.sessions}
              loading={picker.loading}
              searchQuery={picker.searchQuery}
              selectedIndex={pickerSelected}
              onSelectSession={handleSelectSession}
              onSearchChange={picker.setSearchQuery}
            />
          )}

          {view === "list" && session.loading && (
            <div className="app__loading">Loading session…</div>
          )}

          {view === "list" && !session.loading && session.session && (
            <TurnList
              // Root CSS zoom invalidates Virtuoso's cached row geometry.
              // Remount at the new scale so its first measurements all use one
              // coordinate space.
              key={fontScale}
              summaries={summaries}
              selectedIndex={selectedTurn}
              onSelectTurn={(i) => {
                setSelectedTurn(i);
                changeView("detail");
              }}
            />
          )}

          {view === "detail" && !selectedTurnData && summaries[selectedTurn] && (
            <div className="app__loading">Loading turn…</div>
          )}

          {view === "detail" && selectedTurnData && (
            <TurnDetail
              turn={selectedTurnData}
              expanded={expandedTools}
              onToggle={toggleTool}
              onBack={() => changeView("list")}
              openWorkerCallId={activeWorkerPanelTool ? workerPanelCallId : null}
              onOpenWorkerPanel={handleOpenWorkerPanel}
            />
          )}
        </div>

        {activeWorkerPanelTool?.worker_session && (
          <>
            <ResizeHandle onResize={setWorkerPanelWidth} side="right" />
            <WorkerPanel
              session={activeWorkerPanelTool.worker_session}
              sourceTool={activeWorkerPanelTool}
              activeWorkerCallId={workerPanelCallId}
              style={{ flex: `0 0 ${workerPanelWidth}px`, maxWidth: workerPanelWidth }}
              onClose={closeWorkerPanel}
              onOpenWorker={handleOpenWorkerPanel}
            />
          </>
        )}
      </div>

      {/* Bottom keybind bar */}
      <KeybindBar
        view={view}
        showHints={showKeybinds}
        onToggle={() => setShowKeybinds((p) => !p)}
      />

      {showSettings && (
        <SettingsModal
          fontScale={fontScale}
          onFontScaleChange={setFontScale}
          onClose={() => setShowSettings(false)}
          onSaved={(dir) => {
            discoverSessions(dir);
          }}
        />
      )}
    </div>
  );
}
