import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { CodexSession, CodexToolCall } from "../../shared/types";
import { ToolCallItem } from "./ToolCallItem";

function makeTool(overrides: Partial<CodexToolCall> = {}): CodexToolCall {
  return {
    call_id: "call-1",
    kind: "exec_command",
    name: "shell",
    arguments: {},
    input_text: null,
    output: "hello output",
    exit_code: 0,
    command: ["echo", "hello"],
    cwd: "/tmp",
    duration_secs: 0.5,
    mcp_server: null,
    mcp_tool: null,
    plugin_id: null,
    script_path: null,
    patch_success: null,
    patch_changes: null,
    web_query: null,
    web_url: null,
    image_prompt: null,
    image_file_path: null,
    worker_session: null,
    status: "completed",
    subagent_id: null,
    subagent_name: null,
    output_truncated: null,
    ...overrides,
  };
}

function makeWorkerSession(toolCalls: CodexToolCall[]): CodexSession {
  return {
    id: "worker-session",
    timestamp: "2026-04-27T04:50:46Z",
    cwd: "/tmp/worker",
    originator: null,
    cli_version: null,
    model_provider: null,
    git: null,
    instructions: null,
    turns: [
      {
        turn_id: "worker-turn",
        started_at: null,
        completed_at: null,
        duration_ms: null,
        status: "complete",
        user_message: "Worker prompt",
        agent_messages: [
          {
            text: "Nested final",
            phase: "final_answer",
            timestamp: "2026-04-27T04:51:00Z",
            is_reasoning: false,
          },
        ],
        tool_calls: toolCalls,
        final_answer: "Nested final",
        total_tokens: null,
        model: null,
        cwd: "/tmp/worker",
        reasoning_effort: null,
        error: null,
        has_compaction: false,
        thread_name: "Worker thread",
        collab_spawns: [],
        trace_id: null,
        forked_from_thread_id: null,
        compaction_meta: null,
      },
    ],
    is_ongoing: false,
    total_tokens: null,
    thread_name: "Worker thread",
    spawned_worker_ids: [],
    path: "/tmp/worker.jsonl",
    ai_title: null,
    is_headless: false,
    has_missing_spawn_metadata: false,
    is_archived: false,
    approval_mode: null,
    history_base_thread_id: null,
    forked_from_thread_id: null,
  };
}

describe("ToolCallItem", () => {
  it("renders the tool name in the header", () => {
    render(<ToolCallItem tool={makeTool()} expanded={false} onToggle={vi.fn()} />);
    expect(screen.getByText("shell")).toBeInTheDocument();
  });

  it("calls onToggle when the header is clicked", () => {
    const onToggle = vi.fn();
    render(<ToolCallItem tool={makeTool()} expanded={false} onToggle={onToggle} />);
    fireEvent.click(screen.getByText("shell").closest(".tool-call__header")!);
    expect(onToggle).toHaveBeenCalledOnce();
  });

  it("shows the command and output when expanded", () => {
    const { container } = render(
      <ToolCallItem tool={makeTool()} expanded={true} onToggle={vi.fn()} />,
    );
    // Command appears in both the summary and the expanded body; assert the body block specifically.
    expect(container.querySelector(".tool-call__cmd")).toBeInTheDocument();
    expect(container.querySelector(".tool-call__cmd")!.textContent).toBe("echo hello");
    expect(screen.getByText("hello output")).toBeInTheDocument();
  });

  it("hides command and output when collapsed", () => {
    render(<ToolCallItem tool={makeTool()} expanded={false} onToggle={vi.fn()} />);
    expect(screen.queryByText("hello output")).not.toBeInTheDocument();
  });

  it("shows the exit code in the header", () => {
    render(<ToolCallItem tool={makeTool({ exit_code: 1 })} expanded={false} onToggle={vi.fn()} />);
    expect(screen.getByText("exit 1")).toBeInTheDocument();
  });

  it("does not show exit code when exit_code is null", () => {
    render(
      <ToolCallItem tool={makeTool({ exit_code: null })} expanded={false} onToggle={vi.fn()} />,
    );
    expect(screen.queryByText(/exit/)).not.toBeInTheDocument();
  });

  it("applies tool-call--failed class on non-zero exit code", () => {
    const { container } = render(
      <ToolCallItem tool={makeTool({ exit_code: 1 })} expanded={false} onToggle={vi.fn()} />,
    );
    expect(container.querySelector(".tool-call--failed")).toBeInTheDocument();
  });

  it("does not apply failed class on zero exit code", () => {
    const { container } = render(
      <ToolCallItem tool={makeTool({ exit_code: 0 })} expanded={false} onToggle={vi.fn()} />,
    );
    expect(container.querySelector(".tool-call--failed")).not.toBeInTheDocument();
  });

  it("renders formatted duration in the header", () => {
    render(
      <ToolCallItem tool={makeTool({ duration_secs: 0.5 })} expanded={false} onToggle={vi.fn()} />,
    );
    expect(screen.getByText("500ms")).toBeInTheDocument();
  });

  it("places duration left of the popout button in DOM order", () => {
    const { container } = render(
      <ToolCallItem tool={makeTool({ duration_secs: 0.5 })} expanded={false} onToggle={vi.fn()} />,
    );
    const header = container.querySelector(".tool-call__header")!;
    const children = Array.from(header.children);
    const durIdx = children.findIndex((el) => el.classList.contains("tool-call__duration"));
    const popoutIdx = children.findIndex((el) => el.classList.contains("tool-call__popout-btn"));
    expect(durIdx).toBeGreaterThanOrEqual(0);
    expect(durIdx).toBeLessThan(popoutIdx);
  });

  it("applies push class to popout button when there is no duration", () => {
    const { container } = render(
      <ToolCallItem tool={makeTool({ duration_secs: null })} expanded={false} onToggle={vi.fn()} />,
    );
    expect(container.querySelector(".tool-call__popout-btn--push")).toBeInTheDocument();
  });

  it("applies error class to output on non-zero exit code", () => {
    const { container } = render(
      <ToolCallItem tool={makeTool({ exit_code: 2 })} expanded={true} onToggle={vi.fn()} />,
    );
    expect(container.querySelector(".tool-call__output--error")).toBeInTheDocument();
  });

  it("renders MCP server in header prefix and expanded body", () => {
    const { container } = render(
      <ToolCallItem
        tool={makeTool({
          kind: "mcp_tool",
          name: "github_search_prs",
          mcp_server: "codex_apps",
          mcp_tool: "github_search_prs",
        })}
        expanded={true}
        onToggle={vi.fn()}
      />,
    );
    const prefix = container.querySelector(".tool-call__mcp-prefix");
    expect(prefix).toBeInTheDocument();
    expect(prefix!.textContent).toBe("MCP codex_apps");
  });

  it("renders plugin_id and script_path for a plugin-attributed exec_command (v0.146.0+)", () => {
    const { container } = render(
      <ToolCallItem
        tool={makeTool({
          kind: "exec_command",
          plugin_id: "plugin-abc",
          script_path: "scripts/run.py",
        })}
        expanded={true}
        onToggle={vi.fn()}
      />,
    );
    const pluginInfo = container.querySelector(".tool-call__plugin-info");
    expect(pluginInfo).toBeInTheDocument();
    expect(pluginInfo!.textContent).toBe("plugin: plugin-abc (scripts/run.py)");
  });

  it("does not render plugin info when plugin_id is null", () => {
    const { container } = render(
      <ToolCallItem tool={makeTool({ kind: "exec_command" })} expanded={true} onToggle={vi.fn()} />,
    );
    expect(container.querySelector(".tool-call__plugin-info")).not.toBeInTheDocument();
  });

  it("renders plugin_id for an mcp_tool call", () => {
    const { container } = render(
      <ToolCallItem
        tool={makeTool({
          kind: "mcp_tool",
          mcp_server: "codex_apps",
          mcp_tool: "get_pr_info",
          plugin_id: "plugin-xyz",
        })}
        expanded={true}
        onToggle={vi.fn()}
      />,
    );
    const pluginInfo = container.querySelector(".tool-call__plugin-info");
    expect(pluginInfo).toBeInTheDocument();
    expect(pluginInfo!.textContent).toBe("plugin: plugin-xyz");
  });

  it("renders request_plugin_install as an agent_plugin tool call (issue #223)", () => {
    const { container } = render(
      <ToolCallItem
        tool={makeTool({
          kind: "agent_plugin",
          name: "request_plugin_install",
          arguments: { tool_id: "sample@openai-curated", suggest_reason: "Needed for calendar" },
          command: null,
          exit_code: null,
          output: '{"completed":true,"user_confirmed":true}',
        })}
        expanded={true}
        onToggle={vi.fn()}
      />,
    );
    const body = container.querySelector(".tool-call__body");
    expect(body).toBeInTheDocument();
    expect(body!.textContent).toContain("sample@openai-curated");
  });

  it("renders web query when kind is web_search", () => {
    const { container } = render(
      <ToolCallItem
        tool={makeTool({
          kind: "web_search",
          name: "web_search",
          web_query: "rust serde docs",
          command: null,
          output: null,
          exit_code: null,
          duration_secs: null,
        })}
        expanded={true}
        onToggle={vi.fn()}
      />,
    );
    // Query text appears in both the summary and the expanded body; assert the body block.
    const body = container.querySelector(".tool-call__body");
    expect(body).toBeInTheDocument();
    expect(body!.textContent).toContain("rust serde docs");
  });

  it("renders patch file paths when kind is patch_apply", () => {
    const { container } = render(
      <ToolCallItem
        tool={makeTool({
          kind: "patch_apply",
          name: "apply_patch",
          patch_changes: {
            "src/main.rs": { type: "update", unified_diff: "@@ -1 +1 @@\n-old\n+new" },
          },
          command: null,
          exit_code: null,
        })}
        expanded={true}
        onToggle={vi.fn()}
      />,
    );
    // File path appears in both the summary and the expanded body; assert the patch file entry.
    expect(container.querySelector(".tool-call__patch-file")).toBeInTheDocument();
    expect(container.querySelector(".tool-call__patch-file")!.textContent).toContain("src/main.rs");
  });

  it("renders a structured red/green diff from an apply_patch input_text", () => {
    const patch = [
      "*** Begin Patch",
      "*** Update File: src/main.rs",
      "@@",
      " fn main() {",
      '-    println!("old");',
      '+    println!("new");',
      " }",
      "*** End Patch",
    ].join("\n");
    const { container } = render(
      <ToolCallItem
        tool={makeTool({
          kind: "patch_apply",
          name: "apply_patch",
          input_text: patch,
          command: null,
          exit_code: null,
        })}
        expanded={true}
        onToggle={vi.fn()}
      />,
    );
    expect(container.querySelector(".tool-call__patch-file")!.textContent).toContain("src/main.rs");
    expect(container.querySelector(".tool-call__diff-line--removed")!.textContent).toContain("old");
    expect(container.querySelector(".tool-call__diff-line--added")!.textContent).toContain("new");
    // Word-level highlight isolates the changed token.
    expect(container.querySelector(".tool-call__diff-word")).toBeInTheDocument();
  });

  it("pretty-prints JSON output when output is a JSON object", () => {
    const { container } = render(
      <ToolCallItem
        tool={makeTool({ output: '{"url":"https://example.com","number":42}' })}
        expanded={true}
        onToggle={vi.fn()}
      />,
    );
    const code = container.querySelector(".tool-call__output code");
    expect(code).toBeInTheDocument();
    expect(code!.textContent).toContain('"url"');
    expect(code!.textContent).toContain('"https://example.com"');
    expect(code!.textContent).toContain('"number"');
  });

  it("renders plain text output when output is not JSON", () => {
    const { container } = render(
      <ToolCallItem
        tool={makeTool({ output: "plain text output" })}
        expanded={true}
        onToggle={vi.fn()}
      />,
    );
    expect(container.querySelector(".tool-call__output code")).not.toBeInTheDocument();
    expect(screen.getByText("plain text output")).toBeInTheDocument();
  });

  it("renders plain text output when output is a JSON primitive", () => {
    const { container } = render(
      <ToolCallItem
        tool={makeTool({ output: '"just a string"' })}
        expanded={true}
        onToggle={vi.fn()}
      />,
    );
    expect(container.querySelector(".tool-call__output code")).not.toBeInTheDocument();
  });

  it("shows an Open button for spawn_agent tools with embedded worker sessions", () => {
    const onOpenWorker = vi.fn();
    render(
      <ToolCallItem
        tool={makeTool({
          kind: "spawn_agent",
          name: "spawn_agent",
          command: null,
          exit_code: null,
          worker_session: makeWorkerSession([
            makeTool({
              call_id: "child-1",
              name: "nested_shell",
              command: ["pwd"],
              output: "/tmp/worker",
            }),
          ]),
        })}
        expanded={true}
        onToggle={vi.fn()}
        onOpenWorker={onOpenWorker}
      />,
    );
    expect(screen.getByText("Open")).toBeInTheDocument();
    expect(screen.queryByText("Worker prompt")).not.toBeInTheDocument();
  });

  it("shows Close when the worker panel for the tool is open", () => {
    render(
      <ToolCallItem
        tool={makeTool({
          kind: "spawn_agent",
          name: "spawn_agent",
          command: null,
          exit_code: null,
          worker_session: makeWorkerSession([
            makeTool({
              call_id: "child-1",
              name: "first_nested_tool",
              command: ["echo", "first"],
              output: "first",
            }),
          ]),
        })}
        expanded={false}
        onToggle={vi.fn()}
        isWorkerOpen={true}
        onOpenWorker={vi.fn()}
      />,
    );
    expect(screen.getByText("Close")).toBeInTheDocument();
  });

  it("calls onOpenWorker with the source tool when Open is clicked", () => {
    const onOpenWorker = vi.fn();
    const tool = makeTool({
      kind: "spawn_agent",
      name: "spawn_agent",
      command: null,
      exit_code: null,
      worker_session: makeWorkerSession([]),
    });
    render(
      <ToolCallItem tool={tool} expanded={false} onToggle={vi.fn()} onOpenWorker={onOpenWorker} />,
    );
    fireEvent.click(screen.getByText("Open"));
    expect(onOpenWorker).toHaveBeenCalledWith(tool);
  });

  it("does not show worker panel button when embedded worker session is absent", () => {
    render(
      <ToolCallItem
        tool={makeTool({
          kind: "spawn_agent",
          name: "spawn_agent",
          command: null,
          exit_code: null,
        })}
        expanded={false}
        onToggle={vi.fn()}
        onOpenWorker={vi.fn()}
      />,
    );
    expect(screen.queryByText("Open")).not.toBeInTheDocument();
  });

  describe("inline summary", () => {
    it("shows command string as summary for exec_command", () => {
      const { container } = render(
        <ToolCallItem
          tool={makeTool({ command: ["bash", "memory.sh", "read", "topic"] })}
          expanded={false}
          onToggle={vi.fn()}
        />,
      );
      const summary = container.querySelector(".tool-call__summary");
      expect(summary).toBeInTheDocument();
      expect(summary!.textContent).toBe("bash memory.sh read topic");
    });

    it("shows web_query as summary for web_search", () => {
      const { container } = render(
        <ToolCallItem
          tool={makeTool({
            kind: "web_search",
            name: "web_search",
            command: null,
            exit_code: null,
            web_query: "rust async traits",
          })}
          expanded={false}
          onToggle={vi.fn()}
        />,
      );
      const summary = container.querySelector(".tool-call__summary");
      expect(summary).toBeInTheDocument();
      expect(summary!.textContent).toBe("rust async traits");
    });

    it("shows changed file names as summary for patch_apply", () => {
      const { container } = render(
        <ToolCallItem
          tool={makeTool({
            kind: "patch_apply",
            name: "apply_patch",
            command: null,
            exit_code: null,
            patch_changes: {
              "src/lib.rs": { type: "update" },
              "src/main.rs": { type: "update" },
            },
          })}
          expanded={false}
          onToggle={vi.fn()}
        />,
      );
      const summary = container.querySelector(".tool-call__summary");
      expect(summary).toBeInTheDocument();
      expect(summary!.textContent).toContain("src/lib.rs");
      expect(summary!.textContent).toContain("src/main.rs");
    });

    it("shows first string argument as summary for mcp_tool", () => {
      const { container } = render(
        <ToolCallItem
          tool={makeTool({
            kind: "mcp_tool",
            name: "search",
            command: null,
            exit_code: null,
            mcp_server: "github",
            mcp_tool: "search",
            arguments: { query: "fix memory leak" },
          })}
          expanded={false}
          onToggle={vi.fn()}
        />,
      );
      const summary = container.querySelector(".tool-call__summary");
      expect(summary).toBeInTheDocument();
      expect(summary!.textContent).toBe("fix memory leak");
    });

    it("shows tool_id as summary for agent_plugin request_plugin_install (issue #223)", () => {
      const { container } = render(
        <ToolCallItem
          tool={makeTool({
            kind: "agent_plugin",
            name: "request_plugin_install",
            command: null,
            exit_code: null,
            arguments: { tool_id: "sample@openai-curated", suggest_reason: "Needed" },
          })}
          expanded={false}
          onToggle={vi.fn()}
        />,
      );
      const summary = container.querySelector(".tool-call__summary");
      expect(summary).toBeInTheDocument();
      expect(summary!.textContent).toBe("sample@openai-curated");
    });

    it("shows no summary for agent_plugin list_available_plugins_to_install (empty args)", () => {
      const { container } = render(
        <ToolCallItem
          tool={makeTool({
            kind: "agent_plugin",
            name: "list_available_plugins_to_install",
            command: null,
            exit_code: null,
            arguments: {},
          })}
          expanded={false}
          onToggle={vi.fn()}
        />,
      );
      expect(container.querySelector(".tool-call__summary")).not.toBeInTheDocument();
    });

    it("shows no summary for exec_command with null command", () => {
      const { container } = render(
        <ToolCallItem
          tool={makeTool({ command: null, arguments: {} })}
          expanded={false}
          onToggle={vi.fn()}
        />,
      );
      expect(container.querySelector(".tool-call__summary")).not.toBeInTheDocument();
    });

    it("shows image_prompt as summary for image_generation", () => {
      const { container } = render(
        <ToolCallItem
          tool={makeTool({
            kind: "image_generation",
            name: "image_generation",
            command: null,
            exit_code: null,
            image_prompt: "a sunset over mountains",
            image_file_path: null,
          })}
          expanded={false}
          onToggle={vi.fn()}
        />,
      );
      const summary = container.querySelector(".tool-call__summary");
      expect(summary).toBeInTheDocument();
      expect(summary!.textContent).toBe("a sunset over mountains");
    });
  });

  // Codex v0.145.0: bounded exec output — truncation notice in expanded view.
  describe("output truncation notice (v0.145.0+)", () => {
    it("shows truncation notice when output_truncated is true", () => {
      render(
        <ToolCallItem
          tool={makeTool({ output: "partial output...", output_truncated: true })}
          expanded={true}
          onToggle={vi.fn()}
        />,
      );
      expect(screen.getByText(/Output was truncated by the Codex runtime/)).toBeInTheDocument();
    });

    it("does not show truncation notice when output_truncated is null", () => {
      render(
        <ToolCallItem
          tool={makeTool({ output: "full output", output_truncated: null })}
          expanded={true}
          onToggle={vi.fn()}
        />,
      );
      expect(
        screen.queryByText(/Output was truncated by the Codex runtime/),
      ).not.toBeInTheDocument();
    });

    it("does not show truncation notice when output_truncated is false", () => {
      render(
        <ToolCallItem
          tool={makeTool({ output: "full output", output_truncated: false })}
          expanded={true}
          onToggle={vi.fn()}
        />,
      );
      expect(
        screen.queryByText(/Output was truncated by the Codex runtime/),
      ).not.toBeInTheDocument();
    });
  });

  // Codex v0.148.0 (issue #237): sandbox checks that used to silently allow or
  // degrade (unreadable Linux globs, Windows deny-read, Windows networking, app
  // file uploads) now fail closed. There is no dedicated event for this — it
  // surfaces as a normal failed exec_command or mcp_tool call whose output
  // happens to contain a sandbox-denial signature.
  describe("sandbox denial notice (v0.148.0+)", () => {
    it("shows the sandbox-denied notice for a denied exec command", () => {
      render(
        <ToolCallItem
          tool={makeTool({
            exit_code: 1,
            output: "touch: /etc/hosts: Permission denied",
          })}
          expanded={true}
          onToggle={vi.fn()}
        />,
      );
      expect(screen.getByText(/Blocked by sandbox/)).toBeInTheDocument();
    });

    it("does not show the sandbox-denied notice for an unrelated failure", () => {
      render(
        <ToolCallItem
          tool={makeTool({ exit_code: 1, output: "command not found: fooo" })}
          expanded={true}
          onToggle={vi.fn()}
        />,
      );
      expect(screen.queryByText(/Blocked by sandbox/)).not.toBeInTheDocument();
    });

    it("does not show the sandbox-denied notice for a successful command", () => {
      render(
        <ToolCallItem
          tool={makeTool({ exit_code: 0, output: "permission denied earlier in the log" })}
          expanded={true}
          onToggle={vi.fn()}
        />,
      );
      expect(screen.queryByText(/Blocked by sandbox/)).not.toBeInTheDocument();
    });

    // PR #38416: a denied app file upload has no exit code at all — only the
    // MCP call's "failed" status and its output text signal the denial.
    it("shows the sandbox-denied notice for a denied MCP file upload", () => {
      render(
        <ToolCallItem
          tool={makeTool({
            kind: "mcp_tool",
            exit_code: null,
            status: "failed",
            output: "failed to upload `report.txt`: Permission denied",
          })}
          expanded={true}
          onToggle={vi.fn()}
        />,
      );
      expect(screen.getByText(/Blocked by sandbox/)).toBeInTheDocument();
    });

    it("does not show the sandbox-denied notice for a completed MCP call", () => {
      render(
        <ToolCallItem
          tool={makeTool({
            kind: "mcp_tool",
            exit_code: null,
            output: "permission denied",
          })}
          expanded={true}
          onToggle={vi.fn()}
        />,
      );
      expect(screen.queryByText(/Blocked by sandbox/)).not.toBeInTheDocument();
    });
  });
});
