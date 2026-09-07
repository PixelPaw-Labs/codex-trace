import type { CodexSessionInfo } from "../../shared/types";
import { shortPath } from "../../shared/format";

export function sessionDisplayName(session: CodexSessionInfo): string {
  if (session.is_inline_worker || session.is_external_worker) {
    const shortId = session.id.slice(0, 8);
    if (session.worker_nickname) return `${session.worker_nickname} (${shortId})`;
    if (session.worker_role) return `${session.worker_role} ${shortId}`;
    return `worker ${shortId}`;
  }

  // Codex Desktop writes the user-facing title to session_index.jsonl. The
  // backend merges that value into thread_name; ai_title remains the fallback
  // for older external-agent rollouts that carry their title in session_meta.
  if (session.thread_name?.trim()) return session.thread_name.trim();
  if (session.ai_title?.trim()) return session.ai_title.trim();
  if (session.cwd) return shortPath(session.cwd);
  return session.id.slice(0, 8);
}
