# Changelog

All notable changes to codex-trace are documented here. Versions follow
[semantic versioning](https://semver.org/), and this file follows
[Keep a Changelog](https://keepachangelog.com/) conventions.

## [0.1.0] — 2026-06-08

The first release of Codex Trace — a desktop app for browsing and inspecting your
local Codex CLI sessions. Point it at `~/.codex/sessions` and it parses the rollout
JSONL files into a date-grouped session list and a per-session detail view, so you can
read a run turn-by-turn instead of scrolling raw logs.

### Added

- **Session browser and detail view.** Sessions are discovered from
  `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`, grouped by date in the sidebar, and
  opened into a turn-by-turn detail view. Tool calls render inline in chronological
  order, each with an inline summary after the call name so you can skim a run at a
  glance ([#100](https://github.com/PixelPaw-Labs/codex-trace/pull/100)).
- **Broad Codex CLI version coverage.** The parser understands rollout formats across
  many Codex releases — goal lifecycle events
  ([#73](https://github.com/PixelPaw-Labs/codex-trace/pull/73)), `UserInput` /
  `ThreadSettings` items ([#87](https://github.com/PixelPaw-Labs/codex-trace/pull/87)),
  MCP `plugin_id` ([#74](https://github.com/PixelPaw-Labs/codex-trace/pull/74)),
  `trace_id` / `forked_from_thread_id` / compaction metadata
  ([#94](https://github.com/PixelPaw-Labs/codex-trace/pull/94)), memory context from
  `turn_context` ([#95](https://github.com/PixelPaw-Labs/codex-trace/pull/95)),
  `shell_hook_output` events
  ([#113](https://github.com/PixelPaw-Labs/codex-trace/pull/113)), and subagent
  identity fields ([#114](https://github.com/PixelPaw-Labs/codex-trace/pull/114)).
- **Headless / Docker mode.** The app can run without a desktop WebView, making it
  usable on servers and in containers.
- **macOS app bundle installer.** Installing on macOS now produces a proper `.app`
  bundle rather than a bare binary
  ([#99](https://github.com/PixelPaw-Labs/codex-trace/pull/99)).
- **`cut-release` skill.** A project-local Claude Code skill that automates cutting,
  tagging, and publishing a release end-to-end.

### Fixed

- **Compressed rollouts are now readable.** zstd-compressed rollout files (Codex
  v0.137.0) are transparently decompressed instead of failing to parse
  ([#109](https://github.com/PixelPaw-Labs/codex-trace/pull/109)).
- **MCP tool calls resolve correctly.** Tool calls are resolved from `tool_id` in
  v0.130.0 sessions ([#44](https://github.com/PixelPaw-Labs/codex-trace/pull/44)) and
  `mcp_tool_call` turn items from v0.129.0 are handled
  ([#39](https://github.com/PixelPaw-Labs/codex-trace/pull/39)).
- **Image-generation calls are classified correctly** rather than showing as a generic
  tool call ([#112](https://github.com/PixelPaw-Labs/codex-trace/pull/112)).
- **Forward-compatibility guards.** Hidden spawn-agent metadata
  ([#111](https://github.com/PixelPaw-Labs/codex-trace/pull/111)) and `assign_task` /
  `followup_task` items
  ([#108](https://github.com/PixelPaw-Labs/codex-trace/pull/108)) from newer Codex
  builds are now recognised instead of silently dropped.

### Performance

- **No more full session-list streaming on every file-system event** — the session list
  updates incrementally instead of being re-sent on each change.
- **WebKit and Xvfb are skipped in headless/Docker mode**, cutting startup cost and
  dependencies where no GUI is needed.

[0.1.0]: https://github.com/PixelPaw-Labs/codex-trace/releases/tag/v0.1.0
