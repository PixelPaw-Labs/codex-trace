import type { CodexSession } from "../../shared/types";
import { formatTokens, shortPath, timeAgo } from "../../shared/format";
import { shortModel } from "../lib/format";
import { getModelColor } from "../lib/theme";

interface InfoBarProps {
  session: CodexSession;
}

export function InfoBar({ session }: InfoBarProps) {
  const cwd = session.cwd ? shortPath(session.cwd) : null;
  const branch = session.git?.branch ?? null;
  const totalTok = session.total_tokens?.total_tokens ?? 0;
  const lastTurn = session.turns.at(-1);
  const model = lastTurn?.model ?? null;
  const modelClr = model ? getModelColor(model) : undefined;
  const sessionId = session.path.split("/").pop()?.replace(".jsonl", "") || session.id;
  const title = session.thread_name?.trim() || session.ai_title?.trim() || null;

  return (
    <div className="info-bar">
      {title && (
        <span className="info-bar__title" title={title}>
          {title}
        </span>
      )}
      {cwd && <span className="info-bar__project">{cwd}</span>}
      {sessionId && <span className="info-bar__session-id">{sessionId}</span>}
      {session.originator && <span className="info-bar__originator">via {session.originator}</span>}
      {branch && <span className="info-bar__branch">{branch}</span>}
      {model && (
        <span className="info-bar__model" style={{ color: modelClr }}>
          {shortModel(model)}
        </span>
      )}
      {totalTok > 0 && <span className="info-bar__tokens">{formatTokens(totalTok)} tok</span>}
      <span className="info-bar__time">{timeAgo(session.timestamp)}</span>
      {session.is_ongoing && (
        <span className="info-bar__ongoing">
          <span className="braille-spinner" /> active
        </span>
      )}
      {session.has_missing_spawn_metadata && (
        <span
          className="info-bar__warn"
          title="Spawn-agent metadata is hidden (Codex v0.137.0+). Set hide_spawn_agent_metadata = false in your Codex config to enable multi-agent trace coverage."
        >
          ⚠ spawn metadata hidden
        </span>
      )}
    </div>
  );
}
