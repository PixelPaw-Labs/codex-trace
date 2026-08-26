import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { CodexSession, TokenInfo } from "../../shared/types";
import { InfoBar } from "./InfoBar";

const TOKEN_INFO: TokenInfo = {
  input_tokens: 1000,
  cached_input_tokens: 0,
  output_tokens: 500,
  reasoning_output_tokens: 0,
  total_tokens: 1500,
  context_window_tokens: 1500,
  model_context_window: 8000,
  rate_limits: null,
};

function makeSession(overrides: Partial<CodexSession> = {}): CodexSession {
  return {
    id: "sess-1",
    timestamp: "2026-04-26T10:00:00Z",
    cwd: "/Users/user/myproject",
    originator: "codex-tui",
    cli_version: "0.121.0",
    model_provider: "openai",
    git: { branch: "main", commit_hash: "abc123" },
    instructions: null,
    turns: [],
    is_ongoing: false,
    total_tokens: null,
    thread_name: null,
    spawned_worker_ids: [],
    path: "/sessions/2026/04/26/rollout-abc.jsonl",
    ai_title: null,
    is_headless: false,
    has_missing_spawn_metadata: false,
    is_archived: false,
    approval_mode: null,
    history_base_thread_id: null,
    forked_from_thread_id: null,
    ...overrides,
  };
}

describe("InfoBar", () => {
  it("renders the cwd basename", () => {
    render(<InfoBar session={makeSession()} />);
    expect(screen.getByText("myproject")).toBeInTheDocument();
  });

  it("renders originator with 'via' prefix", () => {
    render(<InfoBar session={makeSession()} />);
    expect(screen.getByText("via codex-tui")).toBeInTheDocument();
  });

  it("renders the git branch", () => {
    render(<InfoBar session={makeSession()} />);
    expect(screen.getByText("main")).toBeInTheDocument();
  });

  it("shows 'active' indicator when session is ongoing", () => {
    render(<InfoBar session={makeSession({ is_ongoing: true })} />);
    expect(screen.getByText("active")).toBeInTheDocument();
  });

  it("does not show 'active' indicator when session is not ongoing", () => {
    render(<InfoBar session={makeSession({ is_ongoing: false })} />);
    expect(screen.queryByText("active")).not.toBeInTheDocument();
  });

  it("renders token count when total_tokens is set", () => {
    render(<InfoBar session={makeSession({ total_tokens: TOKEN_INFO })} />);
    expect(screen.getByText("1.5k tok")).toBeInTheDocument();
  });

  it("does not render token count when total_tokens is null", () => {
    render(<InfoBar session={makeSession({ total_tokens: null })} />);
    expect(screen.queryByText(/tok/)).not.toBeInTheDocument();
  });

  it("omits originator section when originator is null", () => {
    render(<InfoBar session={makeSession({ originator: null })} />);
    expect(screen.queryByText(/via/)).not.toBeInTheDocument();
  });

  it("omits branch when git is null", () => {
    render(<InfoBar session={makeSession({ git: null })} />);
    expect(screen.queryByText("main")).not.toBeInTheDocument();
  });

  // Codex v0.137.0 (PR #26114): hide_spawn_agent_metadata defaults to true.
  it("shows spawn metadata warning when has_missing_spawn_metadata is true", () => {
    render(<InfoBar session={makeSession({ has_missing_spawn_metadata: true })} />);
    expect(screen.getByText(/spawn metadata hidden/)).toBeInTheDocument();
  });

  it("does not show spawn metadata warning when has_missing_spawn_metadata is false", () => {
    render(<InfoBar session={makeSession({ has_missing_spawn_metadata: false })} />);
    expect(screen.queryByText(/spawn metadata hidden/)).not.toBeInTheDocument();
  });
});
