# Phase 7 — Publish the GitHub release

Goal: confirm the release is published on the repo's releases page with the CHANGELOG
section as its body.

> **codex-trace has no release pipeline yet.** There is no
> `.github/workflows/release.yml`, so the tag push from Phase 6 did **not** build artifacts
> or create a release. This phase therefore publishes the release **manually** from the
> CHANGELOG slice. The desktop binaries (macOS / Linux / Windows) are **not** built
> automatically — note that in the final report (Phase 9). When a `release.yml` is added,
> replace this phase with the pipeline-watch flow documented at the bottom.

## Step 7.1 — Slice the CHANGELOG for this version

Extract just this version's section to use as the release body:

```bash
awk -v ver="$NEXT_VERSION" '
  $0 ~ "^## \\[" ver "\\]" { inside=1; print; next }
  inside && /^## \[/ { exit }
  inside { print }
' CHANGELOG.md > /tmp/release-notes.md

cat /tmp/release-notes.md
```

If the file is empty, the CHANGELOG heading isn't in the exact `## [X.Y.Z] — YYYY-MM-DD`
format — fix the heading (Phase 4) and re-slice before continuing.

## Step 7.2 — Create the release (foreground, blocking)

Run `gh release create` **in the foreground**. It does not exist yet (the tag was pushed
but no release object was created), so create it pointing at the tag:

```bash
gh release create "v$NEXT_VERSION" \
  --title "v$NEXT_VERSION" \
  --notes-file /tmp/release-notes.md \
  --latest
```

If a draft release somehow already exists for the tag, publish it instead of creating a
duplicate:

```bash
gh release edit "v$NEXT_VERSION" --notes-file /tmp/release-notes.md --draft=false --latest
```

There are no artifacts to attach (nothing was built). If you have locally built bundles
you want to ship, attach them explicitly with `gh release upload "v$NEXT_VERSION" <files>`
— but the skill does not build them itself.

## Step 7.3 — Confirm the release is public

```bash
gh release view "v$NEXT_VERSION" --json isDraft,isPrerelease,url,publishedAt,assets \
  --jq '{isDraft,isPrerelease,url,publishedAt,assets:[.assets[]|.name]}'
```

Expect `"isDraft": false`, `"isPrerelease": false`, and a published timestamp. The
`assets` list will be empty unless you manually uploaded bundles — that's expected while
there's no build pipeline.

## Step 7.4 — Verify the body matches the CHANGELOG

```bash
gh release view "v$NEXT_VERSION" --json body --jq '.body' | head -20
```

The first line should be `## [$NEXT_VERSION] — YYYY-MM-DD`. If it's wrong, re-slice (Step
7.1) and update in place:

```bash
gh release edit "v$NEXT_VERSION" --notes-file /tmp/release-notes.md
```

## Manual fallback — if `gh` is unavailable

If the `gh` CLI isn't available in the session, surface that to the user with the tag
name, the release URL template (`<repo-url>/releases/tag/v$NEXT_VERSION`), and the
`/tmp/release-notes.md` contents so they can create the release from the GitHub web UI.
Do not force-push or re-tag to work around it.

Proceed to Phase 8.

---

## Future state — when a `release.yml` exists

Once a GitHub Actions release pipeline is added, this phase becomes a watch-and-verify
step instead of a manual publish:

1. The tag push triggers a `notes` job that slices `CHANGELOG.md` and exposes it as a
   workflow output.
2. A `guard` job fails the workflow if a published (non-draft) release already exists for
   the tag.
3. Three build jobs (macOS / Linux / Windows) pass `releaseBody:
${{ needs.notes.outputs.body }}` to `tauri-apps/tauri-action`, creating a draft release
   with the CHANGELOG body and built artifacts attached.
4. A `publish` job flips the draft public and marks it latest.

Watch it in the foreground and verify, e.g.:

```bash
RUN_ID=$(gh run list --workflow=release.yml --limit=1 --json databaseId --jq '.[0].databaseId')
gh run watch --exit-status "$RUN_ID"     # set Bash timeout to ~1800000 ms (30 min)
```

Then re-check `gh release view` for `isDraft: false` and the expected platform artifacts
stamped with `$NEXT_VERSION`. If a job conclusion isn't `success`, read
`gh run view "$RUN_ID" --log-failed` and treat the failure as a real CI bug; if artifacts
didn't build, fix the cause and cut a new tag (`vX.Y.Z+1`) rather than force-pushing.
