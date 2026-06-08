# Phase 3 — Bump version files

Goal: bump every version-bearing file to `$NEXT_VERSION` and regenerate the two
lockfiles.

Refer to `${CLAUDE_SKILL_DIR}/references/project-shape.md` for the rules on which files
move in lockstep.

## Step 3.1 — Bump root + Cargo + Tauri config (lockstep)

Use the Edit tool with precise `old_string`/`new_string` (not sed):

- `package.json` — top-level `"version"` field
- `src-tauri/Cargo.toml` — `[package].version` line
- `src-tauri/tauri.conf.json` — top-level `"version"` field

All three must end up at `$NEXT_VERSION`. `tauri.conf.json` is the one `tauri-action`
reads when stamping artifact filenames at build time (`Codex.Trace_<version>_*.dmg`, etc.,
from the `productName` "Codex Trace"). Skipping it produces a release whose artifacts are
stamped with the previous version.

## Step 3.2 — Other sub-packages (none currently)

codex-trace has no separate sub-package (TUI or otherwise) carrying its own version
string. Nothing else to bump here.

If a versioned manifest (e.g. a nested `package.json` or a `pyproject.toml`) is ever
added, bump it in lockstep with the root package and update this step.

## Step 3.3 — Regenerate lockfiles

```bash
npm install --package-lock-only
( cd src-tauri && cargo check --offline )
```

These commands write the new local-workspace version into the lockfiles. Then verify the
diff is small and only touches version strings:

```bash
git diff --stat -- package.json src-tauri/Cargo.toml src-tauri/tauri.conf.json package-lock.json src-tauri/Cargo.lock
```

Expect roughly 6 files changed and around 6 insertions / 6 deletions. A large diff
suggests the lockfile was stale or has unrelated dep changes — investigate before
continuing.

Proceed to Phase 4.
