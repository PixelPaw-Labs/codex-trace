# Project Shape — `codex-trace`

The skill-specific facts the phase files rely on. For everything that's already in the
codebase, this file points at the source of truth rather than repeating it.

## Read these first if you don't know the codebase

- `AGENTS.md` (with `CLAUDE.md` symlinked to it) — toolchain commands (`npm run check`,
  `oxfmt`, `oxlint`, etc.) and the "format + lint + test before committing" rule.
- `.claude/hooks/pre-commit.sh` — header comments describe exactly what the hook checks
  (format, lint, tsc, Rust fmt/clippy) and how the per-session test-reflection flag
  bypass works.
- `.claude/settings.json` — the `Bash(*git commit*)` matcher that wires the hook into
  `git commit` calls.

## Version-bearing files (skill-specific rule)

Three files must agree on the next-version string. Nothing in the codebase enforces this
sync — the skill does.

| File                        | Owns                                 | Bumps with                     |
| --------------------------- | ------------------------------------ | ------------------------------ |
| `package.json` (root)       | Node/TS workspace + binary entry     | the Rust crate (lockstep)      |
| `src-tauri/Cargo.toml`      | Rust crate version                   | root `package.json` (lockstep) |
| `src-tauri/tauri.conf.json` | Tauri bundle filenames + app version | the Rust crate (lockstep)      |

Root + Cargo + `tauri.conf.json` move together because the desktop binary the user installs
is built from all three — `tauri.conf.json`'s `version` field is what `tauri-action`
templates into the released artifact filenames (`Codex.Trace_<version>_*.dmg`, etc., from
the `productName` "Codex Trace"). Missing this file silently ships a release whose
artifacts are stamped with the previous version.

There is currently no separate sub-package (TUI or otherwise) with its own version
manifest. If a versioned manifest is ever introduced (e.g. a `pyproject.toml` or a nested
`package.json`), add it to the lockstep set and update the skill's Phase 3 step.

## Lockfile regen after a version bump

The lockfiles embed the local workspace's version, so they have to be regenerated after
editing version files — `npm run check` won't fix this on its own. Run:

```bash
npm install --package-lock-only           # → package-lock.json
( cd src-tauri && cargo check --offline ) # → src-tauri/Cargo.lock
```

`--package-lock-only` skips the full reinstall (nothing in `node_modules` needs to
change) and `--offline` skips the registry round-trip — only the local crate's version
moved.

## Release pipeline (NOT yet wired up)

> **Heads-up:** codex-trace does **not** currently have a `.github/workflows/release.yml`.
> Only `.github/workflows/ci.yml` exists, and it does not build or publish release
> artifacts. Pushing a `v*` tag therefore does **not** trigger any release automation.
>
> Until a release workflow is added, Phase 6 still pushes the tag (so the version is
> recorded on `origin`) but Phase 7 cannot "watch the pipeline" — instead it publishes the
> GitHub release manually from the CHANGELOG slice (see that phase for the `gh release
create` fallback). The desktop artifacts are not built automatically.

If/when a `release.yml` is added, it should follow this shape so the phase files line up
again:

1. `guard` — refuse to run if a non-draft GitHub release for the tag already exists
   (defence-in-depth complement to the skill's Phase 1 preflight).
2. `notes` — slice `CHANGELOG.md` for the version's section and expose it as a workflow
   output (expects the exact `## [X.Y.Z] — YYYY-MM-DD` heading format).
3. `build-macos` / `build-linux` / `build-windows` — three parallel `tauri-action` runs,
   each creating / updating a draft release with platform artifacts.
4. `publish` — flip the draft to public and mark it latest.

When you add the workflow, update Phase 6 and Phase 7 to re-enable the `gh run watch`
flow and remove the manual-publish fallback.

## GitHub repo identity

Read once with `git remote get-url origin`; the URL template for commits and releases
follows from there:

- Commit: `<repo-url>/commit/<sha>`
- Release: `<repo-url>/releases/tag/v<X.Y.Z>`

The CHANGELOG template (`changelog-template.md`) hardcodes `PixelPaw-Labs/codex-trace`
in the link format. If the repo moves, update that file once.
