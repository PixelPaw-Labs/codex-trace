# Changelog

All notable changes to codex-trace are documented here. Versions follow
[semantic versioning](https://semver.org/), and this file follows
[Keep a Changelog](https://keepachangelog.com/) conventions.

## [0.5.0] — 2026-09-16

The release that makes codex-trace usable against a real sessions directory. A cold start
against 33GB of history went from about a minute and a half to under a fifth of a second,
opening a session no longer ships the whole transcript to the browser, and the local HTTP
API is no longer readable by anything that can reach port 11424. Along the way the parser
caught up with Codex v0.142 through v0.154, including Codex Desktop builds whose replies
were rendering blank.

### Added

- **An adjustable font size**
  ([`5336a88`](https://github.com/PixelPaw-Labs/codex-trace/commit/5336a88)). Settings
  gains a Font Size control that scales the whole interface the way browser zoom does,
  with presets from 80% to 200%. The choice is saved and applied before React mounts, so
  a reload comes back at the same size without flashing at 100%. The app's CSS is px-based
  throughout, so this is implemented as CSS `zoom` on the document root rather than a root
  font size, with an inverse viewport height so the bottom of the window stays reachable at
  larger scales.
- **Every `/api` route now requires a signed per-client credential**
  ([`24ab007`](https://github.com/PixelPaw-Labs/codex-trace/commit/24ab007)). Until now
  the only gate on the local HTTP API was CORS, which protects browsers and nothing else —
  any other local process, or any LAN host when the Docker image binds `0.0.0.0`, could read
  your session transcripts or rewrite `settings.json` by talking to `127.0.0.1:11424`
  directly. Each caller is now a registered client with its own HS256 credential, revocable
  and reissuable one at a time, and `GET /api/whoami` reports who is asking. The built-in
  `web-ui` client is registered on first start. Behaviour is fail-closed: if the config
  directory is unusable the server runs on a throwaway in-memory key rather than opening up,
  and an unparseable client registry is moved aside rather than overwritten. Set
  `CODEXTRACE_API_AUTH=off` to disable verification.
- **@-mentioned Codex tasks read as names instead of raw links**
  ([`f8de745`](https://github.com/PixelPaw-Labs/codex-trace/commit/f8de745)). Codex
  v0.150.0 lets you @-mention another task from the composer, and writes the reference into
  the session file as `[@Title](thread://id)`, often behind a CLI-injected "Referenced chats
  with Codex" preamble. Those prompts now render as plain `@Title`, matching what the Codex TUI
  itself shows, and the referenced thread IDs are captured alongside the existing
  spawned-worker tracking.
- **Clipped command output is called out**
  ([`362f2c6`](https://github.com/PixelPaw-Labs/codex-trace/commit/362f2c6)). Codex
  v0.145.0 reintroduced bounded exec output, so a long-running command's output can be cut
  short. A tool call whose output was truncated now says so, instead of presenting a partial
  result as if it were the whole thing.
- **More session metadata on the API payload**
  ([`62b7895`](https://github.com/PixelPaw-Labs/codex-trace/commit/62b7895),
  [`4b00fad`](https://github.com/PixelPaw-Labs/codex-trace/commit/4b00fad),
  [`00d65f0`](https://github.com/PixelPaw-Labs/codex-trace/commit/00d65f0),
  [`b65f4d1`](https://github.com/PixelPaw-Labs/codex-trace/commit/b65f4d1),
  [`1dba5ea`](https://github.com/PixelPaw-Labs/codex-trace/commit/1dba5ea)). Five fields
  Codex records but codex-trace used to drop on the floor are now parsed and exposed on the
  session and turn payloads: the approval mode a session ran under (including v0.144.0's new
  `writes` value), the per-credit type and expiry on rate-limit resets, the thread a `codex
exec` session was forked from, the thread a paginated continuation carries its history from,
  and the threads a turn @-mentioned. These are available to anything reading the HTTP API
  and are typed in `shared/types.ts`; the UI does not display them yet.

### Fixed

- **Startup against a large sessions directory no longer takes minutes**
  ([`18351eb`](https://github.com/PixelPaw-Labs/codex-trace/commit/18351eb),
  [`7b753a8`](https://github.com/PixelPaw-Labs/codex-trace/commit/7b753a8)). Discovery has
  to read every session file end to end, because the turn count, token totals and
  still-running flag are only knowable from the last line. Against 3,304 sessions totalling
  33GB that took 92 seconds before a single row appeared — and the picker's cache expired
  after two seconds, so the whole directory was re-read every few seconds for as long as
  anything was watching. Session files are append-only, so a file's modified time and length
  together identify its contents; scan results are now remembered against that pair and
  persisted to `scan-cache.json`, bringing a warm start to 0.15s and a start after a full
  restart to 0.19s. The list itself is also fetched 200 sessions at a time rather than all at
  once, with search and counting moved to the backend so a search still covers the whole
  directory rather than only the batches already loaded.
- **Opening a session no longer sends the whole transcript to the frontend**
  ([`3956c5b`](https://github.com/PixelPaw-Labs/codex-trace/commit/3956c5b)). The list view
  draws a user message, one agent preview line and a few counts — but it used to receive every
  turn with every command output, file body and patch, and hold all of it in the JS heap for as
  long as the session stayed open. Hundreds of megabytes to draw a list that shows none of it.
  The backend now returns metadata plus one summary per turn, and fetches a turn's body when
  you open it; on a 90MB transcript the initial payload drops from the whole file to 52KB. The
  list is virtualized, so the DOM holds eight rows for a 35-turn session and the same eight
  after scrolling.
- **A watched session no longer rebroadcasts itself on every appended line**
  ([`d04dfaf`](https://github.com/PixelPaw-Labs/codex-trace/commit/d04dfaf)). The session
  watcher put the whole session on the wire on every write, to every connected client, and
  parsed the file only to throw the result away — leaving the frontend's follow-up fetch to
  parse the same bytes again. It now sends a bare refresh signal and lets clients re-fetch,
  matching what the picker watcher already did. Two appends put 66 bytes on the stream instead
  of two full turn indexes, and one file change means one parse.
- **The app reclaims WebKit memory instead of growing all session**
  ([`009a1dc`](https://github.com/PixelPaw-Labs/codex-trace/commit/009a1dc)). WebKit, the
  webview engine Tauri uses on macOS, holds on to render-tree and layout memory across repeated
  large DOM swaps, and there is no public API to release it — so the footprint compounded no
  matter how tidy the JS side was. After 25 session opens the app now reloads and reopens the
  session you were on, so the reset restores your place rather than dumping you at an empty
  picker. It waits for in-flight IPC calls to settle first, since reloading mid-call is a known
  crash cause.
- **Highlighted code blocks survive a re-render**
  ([`3fd1c79`](https://github.com/PixelPaw-Labs/codex-trace/commit/3fd1c79)). The markdown
  renderer defined its code component inside the render body, so every render handed
  react-markdown a new component identity and React rebuilt each highlighted block from
  scratch — throwing away the highlighter's work and any scroll position inside a wide block.
- **The API no longer answers any origin that asks**
  ([`4341bcd`](https://github.com/PixelPaw-Labs/codex-trace/commit/4341bcd)). CORS was
  permissive, returning `Access-Control-Allow-Origin: *` from an unauthenticated server on
  loopback — so any site you visited could read your prompts, code and tool output
  cross-origin while the app was running. Origins are now an exact-match allowlist of the dev
  and web UI, extendable at launch via `CODEXTRACE_ALLOWED_ORIGINS` and at runtime from a new
  Settings section, with methods limited to GET and POST.
- **The live stream comes back after a refused reconnect**
  ([`b741197`](https://github.com/PixelPaw-Labs/codex-trace/commit/b741197)). `EventSource`
  retries a dropped connection itself, but a non-200 reply makes it fail outright: one error
  event, then nothing ever again. Nothing was listening, so after a backend restart or a
  sleep/wake the tab looked connected while no update ever arrived — no live tail, no picker
  refresh. The stream is now reopened with the current credential and every listener
  re-attached, backing off from 1s to 30s, and a credential change reconnects immediately.
- **The turn list can be scrolled**
  ([`9f5fb4a`](https://github.com/PixelPaw-Labs/codex-trace/commit/9f5fb4a)). Scrolling
  jumped straight back to the selected row and stayed there, because the scroll call lived in
  the handler that fires continuously as you scroll. It now runs only when the selection
  actually changes, and rows are built 600px past each edge of the viewport so they are ready
  before they come into view.
- **Bringing a selection into view no longer pushes the toolbars off-screen**
  ([`6086fdd`](https://github.com/PixelPaw-Labs/codex-trace/commit/6086fdd)).
  `scrollIntoView` scrolls every scrollable ancestor, and an `overflow: hidden` element is
  still programmatically scrollable — so any layout giving the app shell a scroll range let
  this move the shell, with no scrollbar to bring it back. Only the nearest real scroll
  container is adjusted now, and nothing moves when there isn't one.
- **A live session's detail view stops flashing "Loading turn…"**
  ([`75fafa4`](https://github.com/PixelPaw-Labs/codex-trace/commit/75fafa4)). The live-update
  handler emptied the loaded-turn map before re-reading the index, so the selected turn had no
  body and fell back to its placeholder until the refetch landed — repeatedly, for as long as
  the session kept appending. Open turns are now re-fetched and swapped only once their
  replacement has arrived, and a turn whose refetch fails keeps what is on screen.
- **Codex Desktop sessions show their replies**
  ([`140a51d`](https://github.com/PixelPaw-Labs/codex-trace/commit/140a51d),
  [`38500c2`](https://github.com/PixelPaw-Labs/codex-trace/commit/38500c2)). Sessions from
  Codex Desktop 0.153 and 0.154 showed an empty Codex bubble for every turn in the list, and a
  detail view with tool calls but no text at all. Those builds stopped emitting the assistant
  event the parser relied on; the reply now exists only as a response item with
  `role=assistant`, which the parser read solely as a fallback for the final answer and never
  added to the turn. Those items are now collected in stream order so they interleave with tool
  calls as before, and the final answer prefers the message the model actually marked as the
  answer rather than whichever assistant message came first. Verified against a real 29MB
  Desktop session: all six turns went from a blank preview to their own text.
- **A truncated string no longer takes a whole line of session data with it**
  ([`f1bdb11`](https://github.com/PixelPaw-Labs/codex-trace/commit/f1bdb11)). JSON has no
  representation for an unpaired UTF-16 surrogate, so a producer that truncates a string mid-pair
  leaves one half behind and the strict parser rejects the entire line. On a real session where
  that landed on the last assistant message, the turn showed an earlier preamble instead of the
  answer and — because the completion line went with it — was still drawn as running, with no
  end time or duration. Unpaired surrogates are now replaced with U+FFFD before parsing, valid
  pairs untouched, and every reader of a session file goes through that one path.
- **Event handlers can be async, and their failures are reported**
  ([`7a3b0a1`](https://github.com/PixelPaw-Labs/codex-trace/commit/7a3b0a1)). Handlers were
  typed as returning nothing, so an async handler's promise was dropped: its work was never
  waited on and a rejection escaped as an unhandled rejection. Three failures had nowhere to go
  — a handler that threw was indistinguishable from a malformed frame and discarded with it,
  attaching a listener could fail silently and leave nothing attached, and opening a URL could
  reject into the void.
- **Docker: the healthcheck works and the config directory survives a restart**
  ([`00f0268`](https://github.com/PixelPaw-Labs/codex-trace/commit/00f0268)). The healthcheck
  opened `/dev/tcp` through `/bin/sh`, which on `debian:trixie-slim` is dash and has no such
  redirect — so every check failed and the container reported unhealthy forever. The config
  directory also only lived in the container's writable layer, so the sessions directory, the API
  signing secret and every issued client credential were thrown away on each recreate, meaning
  every client's credential stopped verifying after a routine restart. Both are fixed, with the
  config path on a named volume. Separately, the repo had no `.dockerignore`, so every build
  transferred the whole working tree — `src-tauri/target`, `node_modules` and all, 8.8GB and
  still climbing — to the daemon before the first instruction ran; the context is now a few
  kilobytes. A new test checks the Dockerfile and compose file still describe the same container.
- **Sessions compressed by Codex are still found**
  ([`2a53dfc`](https://github.com/PixelPaw-Labs/codex-trace/commit/2a53dfc)). Codex's
  background worker compresses a rollout file that has been idle for a week into a `.jsonl.zst`
  sibling and deletes the original. The directory scan only matched a bare `.jsonl`, so those
  sessions vanished from the picker entirely with no indication. Both the scan and the file
  watchers now recognise the compressed form, including the case of a session going cold while
  you are looking at it.
- **Secrets in exec commands are redacted**
  ([`1a34f02`](https://github.com/PixelPaw-Labs/codex-trace/commit/1a34f02)). Codex v0.147.0
  redacts bearer tokens and similar secrets, but only when its app-server layer builds items for
  IDE and app clients — the rollout writer never calls it, so the raw command is what actually
  lands in the session file codex-trace reads. Commands from exec events and function-call
  arguments are now redacted here too, so codex-trace does not display what Codex's own clients
  hide.
- **A call blocked by the sandbox says so**
  ([`48c6740`](https://github.com/PixelPaw-Labs/codex-trace/commit/48c6740)). Codex v0.148.0
  made several previously-silent sandbox checks fail closed. A denied MCP call was the worse case:
  only the success branch of the result was read, so a rejected call appeared as a completed one
  with no output at all. Failed MCP calls now read the error and are marked failed, and a denial
  is labelled "Blocked by sandbox" rather than being indistinguishable from any other failure.
- **Agent Plugins calls are recognised instead of shown as unknown**
  ([`a37fd64`](https://github.com/PixelPaw-Labs/codex-trace/commit/a37fd64)). Catalog search and
  plugin install are real built-in function tools, but they were classified generically with no
  summary, so the trace never said which plugin was searched for or requested. They now have their
  own kind and summary. The plugin attribution Codex v0.146.0 added to exec events was also
  hardcoded to empty — the plugin script that ran a command is now read and displayed, which also
  fixes plugin IDs never showing on MCP tool calls.
- **Per-turn warnings are shown**
  ([`3a743df`](https://github.com/PixelPaw-Labs/codex-trace/commit/3a743df)). Codex v0.146.0's
  skills subsystem reports catalog budget and truncation notices as a plain warning event, and
  codex-trace had no handling for that event type at all, so those notices — and any other
  warning, such as a model reroute — were silently dropped. Turns now carry their warnings and the
  detail view has a section for them.
- **An imported conversation no longer shows a fake assistant reply**
  ([`70411ab`](https://github.com/PixelPaw-Labs/codex-trace/commit/70411ab)). Codex marks an
  imported Claude or Cursor thread with a synthetic `<EXTERNAL SESSION IMPORTED>` message. Since
  v0.147.0 can sync further edits into an already-imported thread, that sentinel can now sit
  mid-transcript rather than only at the end — and it was being rendered as a real message. It is
  skipped now.
- **A session that ran out of budget is no longer listed as still running**
  ([`ac1565f`](https://github.com/PixelPaw-Labs/codex-trace/commit/ac1565f)). The discovery scan
  treated only task completion and explicit aborts as turn-terminating, so a session that ended
  through token-budget exhaustion or a cancelled inference stream showed as ongoing in the list
  until a 60-second fallback elapsed. Both events now end the turn and set the session's end time.
- **Multi-agent messages are no longer dropped**
  ([`dd368ba`](https://github.com/PixelPaw-Labs/codex-trace/commit/dd368ba)). Codex v0.142.0
  changed inter-agent messages from plain strings to typed envelopes. The parser read the field as
  a string, which quietly yielded nothing for an object, so those messages disappeared from the
  trace entirely.
- **Response items land on the right turn**
  ([`5c1a4ae`](https://github.com/PixelPaw-Labs/codex-trace/commit/5c1a4ae)). Codex v0.142.2
  stamps every response item with the turn it belongs to. That is read first now, instead of
  guessing from position — which went wrong whenever items for a finished turn arrived while a
  later one was already active.
- **The MCP tool inventory is complete again**
  ([`a6e13a8`](https://github.com/PixelPaw-Labs/codex-trace/commit/a6e13a8)). Codex v0.142.2
  discovers MCP tools through tool-search calls mid-turn rather than listing them all up front, so
  reading only the turn-start list produced a partial inventory and misclassified the calls that
  followed. Tool definitions are now collected from tool-search results as the turn proceeds.
- **Budget, voice and MCP-auth events parse cleanly**
  ([`e6f226f`](https://github.com/PixelPaw-Labs/codex-trace/commit/e6f226f),
  [`96269bf`](https://github.com/PixelPaw-Labs/codex-trace/commit/96269bf),
  [`43bd1fd`](https://github.com/PixelPaw-Labs/codex-trace/commit/43bd1fd),
  [`bc13ae7`](https://github.com/PixelPaw-Labs/codex-trace/commit/bc13ae7),
  [`8f46f65`](https://github.com/PixelPaw-Labs/codex-trace/commit/8f46f65)). A run of event types
  Codex added between v0.142.0 and v0.148.0 now have explicit handling rather than falling through
  the catch-all: a token-budget abort ends the turn with a reason instead of leaving it open;
  v0.145.0's streaming voice transcripts are captured on the turn rather than discarded; MCP
  interactive auth events, which became default behaviour in v0.144.0, no longer log as
  unclassified; compaction lineage is recorded; and the removed `/realtime` voice items are marked
  archive-only so their status is explicit.

[0.5.0]: https://github.com/PixelPaw-Labs/codex-trace/releases/tag/v0.5.0

## [0.4.0] — 2026-06-28

A fresh app icon in codex green, a quieter macOS install, and a much lighter startup.
codex-trace no longer balloons memory while it scans your session history, and the macOS
bundle identifier no longer trips a system warning on launch.

### Added

- **Codex-green app icon**
  ([`1abd896`](https://github.com/PixelPaw-Labs/codex-trace/commit/1abd896)). The app
  icon's iris is recolored from orange to codex green (`#10a37f`) across every asset —
  the macOS `.icns`, the Windows `.ico` and Store tiles, and all PNG sizes — so the
  installed app, dock, and taskbar all show the new mark. The README header now carries
  the icon too.

### Fixed

- **Startup no longer spikes memory on large session histories**
  ([`20d85f4`](https://github.com/PixelPaw-Labs/codex-trace/commit/20d85f4)). The
  discovery scan used to load each session file fully into memory, so peak usage jumped
  to the size of your largest rollout file (often hundreds of MB) before settling. The
  scan now streams each file line by line — decompressing zstd on the fly — so memory
  during discovery is bounded to a single line regardless of session size.
- **macOS install no longer warns about the bundle identifier**
  ([`2b49ff9`](https://github.com/PixelPaw-Labs/codex-trace/commit/2b49ff9)). The bundle
  identifier ended in `.app`, which macOS flags as conflicting with the application
  bundle extension. It is now `com.codextrace.desktop`, so installing and launching the
  app is clean.

[0.4.0]: https://github.com/PixelPaw-Labs/codex-trace/releases/tag/v0.4.0

## [0.3.0] — 2026-06-28

Patch tool calls now read like a real code review, and the parser keeps pace with the
newest Codex CLI releases (v0.140.0 and v0.141.0). If you saw raw `*** Begin Patch`
text instead of a diff, or sessions from the latest Codex builds showed missing context
tools, unrecognized MCP tool calls, or spurious turns around `/import`, this release
addresses those.

### Added

- **`apply_patch` renders as a red/green diff**
  ([`426ea62`](https://github.com/PixelPaw-Labs/codex-trace/commit/426ea62)). An
  `apply_patch` tool call now shows a per-file, per-hunk diff with `+`/`-` markers,
  red/green line tinting, and word-level highlighting on the spans that actually
  changed — instead of the raw patch body. It falls back to the previous
  `patch_changes` / raw views when the input isn't a recognizable patch.

### Fixed

- **Tool calls from the latest Codex builds are classified correctly**
  ([`83cc23b`](https://github.com/PixelPaw-Labs/codex-trace/commit/83cc23b)). Codex
  v0.141.0 emits dynamic tool namespaces (MCP, connector, plugin) in `ThreadStart` /
  `task_started` events, so calls now arrive as qualified `mcp:server/tool_name` names
  or need a registry lookup. codex-trace reads the `dynamic_tools` registry and parses
  the qualified format, so these tools are recognized as MCP calls rather than mislabeled.
- **Context-budget tools are recognized**
  ([`c212d71`](https://github.com/PixelPaw-Labs/codex-trace/commit/c212d71)). Codex
  v0.140.0's `token_budget_context`, `context_remaining`, and `context_window` calls
  are now classified as context queries instead of falling through as unknown tools.
- **`/import` sessions parse cleanly**
  ([`3f73060`](https://github.com/PixelPaw-Labs/codex-trace/commit/3f73060)). Codex
  v0.140.0's `/import` command writes new lifecycle entries (e.g.
  `external_agent_imported`) before the first `task_started`, and v0.141.0 adds an
  `external_agent_import_result` response item. These are now handled explicitly, so
  imported-agent context no longer produces spurious synthetic turns or corrupts the
  turn it sits in.
- **IPC commands are granted explicitly in the ACL**
  ([`7e330bd`](https://github.com/PixelPaw-Labs/codex-trace/commit/7e330bd)). The app
  previously relied on Tauri implicitly permitting its own commands. Each command is now
  granted through an explicit permission set, with a regression test that cross-checks
  the handlers against the ACL in both directions — closing a path where a wired-up
  command could fail at runtime with "Command not allowed by ACL".

## [0.2.0] — 2026-06-16

A readability upgrade for the turn view plus a sweep of parser compatibility with the
latest Codex CLI releases (v0.132.0 through v0.139.0). If your sessions had blank final
answers, missing memory notes, or tool calls that looked corrupted on newer Codex
builds, this release fixes those — and the assistant's commentary now reads inline,
in order, alongside the tool calls it interleaves with.

### Added

- **Assistant commentary renders inline**
  ([`20d48f4`](https://github.com/PixelPaw-Labs/codex-trace/commit/20d48f4)). The
  assistant's prose is now a first-class timeline item ("Complementary") shown expanded
  by default, so a turn reads commentary → tool call → commentary → … → final answer top
  to bottom instead of leaving the text as loose lines above a tool box.
- **Image file paths from generated images**
  ([`1deb184`](https://github.com/PixelPaw-Labs/codex-trace/commit/1deb184)). Codex
  v0.138.0 attaches a `file_path` to image-generation results; codex-trace now surfaces
  it so you can see where a generated image landed on disk.
- **Archived-session awareness**
  ([`fcc8bc4`](https://github.com/PixelPaw-Labs/codex-trace/commit/fcc8bc4)). Sessions
  archived or unarchived via Codex v0.136.0's `codex archive` / `/archive` are now
  tracked, so archived runs are recognized rather than shown as ordinary sessions.

### Fixed

- **Raw command output no longer corrupts tool-call details**
  ([`6850c30`](https://github.com/PixelPaw-Labs/codex-trace/commit/6850c30)). On Codex
  v0.133.0, exec output is kept verbatim; phrases like "exit code: 1" inside real output
  were being mistaken for metadata. Exec metadata is now read only from the structured
  `Output:` marker, so a compiler or test log can no longer fake an exit code or
  duration.
- **Tool calls with structured arguments are no longer dropped**
  ([`f081fd0`](https://github.com/PixelPaw-Labs/codex-trace/commit/f081fd0)). Codex
  v0.139.0 can emit `function_call` arguments as a JSON object rather than a string;
  those calls now parse and display instead of showing up empty.
- **Final answers from `--output-schema` runs now display**
  ([`26f1874`](https://github.com/PixelPaw-Labs/codex-trace/commit/26f1874)). Codex
  v0.132.0 `structured_output` / `message` response items were silently skipped, leaving
  the final answer blank; they're now shown.
- **Versioned memory summaries are parsed again**
  ([`947f248`](https://github.com/PixelPaw-Labs/codex-trace/commit/947f248)). Codex
  v0.132.0 made `turn_context` memories versioned objects instead of plain strings,
  which dropped them from the view; both forms are now handled.
- **Agent-interrupt events are recognized under their new name**
  ([`b9f9bd1`](https://github.com/PixelPaw-Labs/codex-trace/commit/b9f9bd1)). Codex
  v0.139.0 renamed `close_agent` to `interrupt_agent`; both names are now classified
  correctly, so multi-agent runs keep displaying these events.

### Changed

- **Fonts aligned with claude-code-trace**
  ([`20d48f4`](https://github.com/PixelPaw-Labs/codex-trace/commit/20d48f4)). Detail-view
  text now uses fixed `px` sizing (13px prose) instead of `rem`, so type no longer
  rescales with browser/OS root font settings.

[0.3.0]: https://github.com/PixelPaw-Labs/codex-trace/releases/tag/v0.3.0
[0.2.0]: https://github.com/PixelPaw-Labs/codex-trace/releases/tag/v0.2.0

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
