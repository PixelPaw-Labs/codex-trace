/**
 * Pure formatting utilities shared between Tauri UI and TUI.
 * No React, DOM, or framework dependencies.
 */

/** Formats a token count: 1234 -> "1.2k", 1234567 -> "1.2M" */
export function formatTokens(n: number): string {
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + "M";
  if (n >= 1_000) return (n / 1_000).toFixed(1) + "k";
  return String(n);
}

const CODEX_CONTEXT_BASELINE_TOKENS = 12_000;

/** Matches Codex TUI's "Context XX% left" calculation. */
export function contextRemainingPercent(
  contextWindowTokens: number | null | undefined,
  modelContextWindow: number,
): number | null {
  if (contextWindowTokens === null || contextWindowTokens === undefined) return null;
  if (modelContextWindow <= CODEX_CONTEXT_BASELINE_TOKENS) return null;

  const effectiveWindow = modelContextWindow - CODEX_CONTEXT_BASELINE_TOKENS;
  const used = Math.max(contextWindowTokens - CODEX_CONTEXT_BASELINE_TOKENS, 0);
  const remaining = Math.max(effectiveWindow - used, 0);
  const percent = (remaining / effectiveWindow) * 100;
  return Math.round(Math.max(0, Math.min(100, percent)));
}

/** Formats USD cost: 1.5 -> "$1.50" */
export function formatCost(usd: number): string {
  return "$" + usd.toFixed(2);
}

/** Formats duration: 1500 -> "1.5s", 90000 -> "1m 30s" */
export function formatDuration(ms: number): string {
  if (ms < 1) return "< 1ms";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(1)}s`;
  const m = Math.floor(s / 60);
  const rem = Math.floor(s % 60);
  return rem > 0 ? `${m}m ${rem}s` : `${m}m`;
}

/** Truncate string to max length with ellipsis, collapsing newlines. */
export function truncate(s: string, max: number): string {
  const line = s.replace(/\n/g, " ").trim();
  return line.length > max ? line.slice(0, max - 1) + "…" : line;
}

/** Extract the encoded project directory key from a session path. */
export function projectKey(path: string): string {
  const match = path.match(/[/\\]\.claude[/\\]projects[/\\]([^/\\]+)/);
  return match ? match[1] : "unknown";
}

/** Decode a project key to a display name (last path segment). */
export function projectDisplayName(key: string): string {
  const path = key.replace(/^-/, "/").replaceAll("-", "/");
  const parts = path.split("/").filter(Boolean);
  return parts[parts.length - 1] ?? key;
}

/** Extract the last path segment. */
export function shortPath(cwd: string): string {
  if (!cwd) return "";
  const parts = cwd.split("/").filter(Boolean);
  return parts[parts.length - 1] ?? cwd;
}

const MIN_JSON_TRANSFORM_LENGTH = 15;

/**
 * Scans each line of text for bare JSON objects/arrays (not already inside a
 * code fence) and replaces them using the provided wrap callback.
 *
 * wrap(prefix, formattedJson) → replacement line
 *   prefix  — text before the JSON blob on the same line (may be empty)
 *   formattedJson — JSON.stringify(parsed, null, 2)
 *
 * Used by both platforms:
 *   - GUI wraps in ```json fences for ReactMarkdown
 *   - TUI wraps as indented plain text
 */
export function transformInlineJson(
  text: string,
  wrap: (prefix: string, formatted: string) => string,
): string {
  const lines = text.split("\n");
  let inCodeBlock = false;
  const result: string[] = [];

  for (const line of lines) {
    if (line.trimStart().startsWith("```")) {
      inCodeBlock = !inCodeBlock;
      result.push(line);
      continue;
    }

    if (inCodeBlock) {
      result.push(line);
      continue;
    }

    const trimmed = line.trim();
    if (!trimmed.includes("{") && !trimmed.includes("[")) {
      result.push(line);
      continue;
    }

    let transformed = false;
    for (let i = 0; i < trimmed.length; i++) {
      const ch = trimmed[i];
      if (ch !== "{" && ch !== "[") continue;
      const candidate = trimmed.slice(i);
      if (candidate.length < MIN_JSON_TRANSFORM_LENGTH) break;
      try {
        const parsed = JSON.parse(candidate);
        if (
          parsed !== null &&
          typeof parsed === "object" &&
          (Array.isArray(parsed) ? parsed.length > 0 : Object.keys(parsed).length > 0)
        ) {
          const prefix = trimmed.slice(0, i).trimEnd();
          const formatted = JSON.stringify(parsed, null, 2);
          result.push(wrap(prefix, formatted));
          transformed = true;
          break;
        }
      } catch {
        // not valid JSON from this position — try next {/[ character
      }
    }

    if (!transformed) {
      result.push(line);
    }
  }

  return result.join("\n");
}

/**
 * Codex v0.148.0 (issue #237) made several previously-silent sandbox checks fail
 * closed (unreadable Linux globs, Windows deny-read rules, Windows managed
 * networking, app file uploads): a call that used to run to completion, or to
 * silently bypass the permission profile, can now fail instead. There is no
 * dedicated event type for this — it just shows up as a normal failed tool
 * call whose output happens to contain a denial message. Codex's own runtime
 * uses this same keyword heuristic (codex-rs/sandboxing/src/denial.rs) to
 * recognize a denial after the fact; mirror it here so a failed call can be
 * labeled "blocked by sandbox" instead of a generic failure. Callers decide
 * whether the tool call failed at all (exec exit code, MCP status, ...) —
 * this only judges whether the output text looks like a sandbox denial.
 */
const SANDBOX_DENIED_KEYWORDS = [
  "operation not permitted",
  "permission denied",
  "read-only file system",
  "seccomp",
  "sandbox",
  "landlock",
  "failed to write file",
  // Codex v0.148.0 PR #38026: an unreadable Linux glob now fails sandbox
  // construction itself (`error building bubblewrap command: unreadable glob
  // ... cannot be safely expanded`), which doesn't contain any of the generic
  // keywords above.
  "cannot be safely expanded",
];

export function isLikelySandboxDenied(output: string | null): boolean {
  if (!output) return false;
  const lower = output.toLowerCase();
  return SANDBOX_DENIED_KEYWORDS.some((keyword) => lower.includes(keyword));
}

/** Relative time: "3m ago", "2h ago", "5d ago" */
export function timeAgo(iso: string): string {
  const ms = Date.now() - new Date(iso).getTime();
  const mins = Math.floor(ms / 60000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  const days = Math.floor(hrs / 24);
  return `${days}d ago`;
}
