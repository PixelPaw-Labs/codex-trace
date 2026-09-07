import type { CodexSessionInfo } from "../../shared/types";
import { shortPath } from "../../shared/format";

export interface ProjectSessionGroup {
  projectDir: string;
  label: string;
  items: CodexSessionInfo[];
}

/** Group sessions by their exact working directory while preserving first-seen order. */
export function groupSessionsByProject(sessions: CodexSessionInfo[]): ProjectSessionGroup[] {
  const groups = new Map<string, CodexSessionInfo[]>();
  for (const session of sessions) {
    const projectDir = session.cwd?.trim() ?? "";
    if (!groups.has(projectDir)) groups.set(projectDir, []);
    groups.get(projectDir)!.push(session);
  }

  return Array.from(groups.entries()).map(([projectDir, items]) => ({
    projectDir,
    label: projectDir ? shortPath(projectDir) : "Unknown project",
    items,
  }));
}

/** Match keyboard selection order to the project-grouped visual order. */
export function orderSessionsByProject(sessions: CodexSessionInfo[]): CodexSessionInfo[] {
  return groupSessionsByProject(sessions).flatMap((group) => group.items);
}
