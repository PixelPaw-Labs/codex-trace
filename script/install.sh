#!/usr/bin/env bash
set -euo pipefail

# Install codex-trace.
#
# Platform-specific, because the right artifact differs:
#   - macOS: build a proper `.app` bundle via `tauri build` and install it to
#     /Applications. `cargo install` only produces a bare Mach-O binary with no
#     Info.plist / GUI activation policy, which shows a blank white window on
#     macOS. The backend and frontend are fine; only the bundling is wrong.
#   - Linux/other: a bare binary from `cargo install` works fine (there is no
#     `.app` concept), so keep the original flow.

cd "$(dirname "$0")/.."

echo "==> Installing npm dependencies..."
npm install

OS="$(uname -s)"

if [ "$OS" = "Darwin" ]; then
  # `tauri build` runs the frontend build itself (beforeBuildCommand).
  echo "==> Building macOS .app bundle (tauri build)..."
  npx tauri build --bundles app

  APP_SRC="src-tauri/target/release/bundle/macos/Codex Trace.app"
  APP_DEST="/Applications/Codex Trace.app"

  echo "==> Installing to ${APP_DEST}..."
  rm -rf "$APP_DEST"
  cp -R "$APP_SRC" "$APP_DEST"

  # A stale `cargo install` binary in ~/.cargo/bin shadows everything else on
  # PATH and reintroduces the white-screen bug, so remove it.
  if [ -f "$HOME/.cargo/bin/codex-trace" ]; then
    echo "==> Removing stale cargo-installed binary (~/.cargo/bin/codex-trace)..."
    cargo uninstall codex-trace >/dev/null 2>&1 || rm -f "$HOME/.cargo/bin/codex-trace"
  fi

  # Keep the `codex-trace` CLI (dev/web modes) available on PATH.
  echo "==> Linking codex-trace CLI..."
  npm link

  echo ""
  echo "Installed! Launch the desktop app from Launchpad/Applications, or:"
  echo "  open -a \"Codex Trace\"   # desktop app"
  echo "  codex-trace --web        # web mode (opens browser)"
else
  echo "==> Building frontend..."
  npm run build

  echo "==> Installing binary via cargo..."
  cargo install --path src-tauri

  echo "==> Linking codex-trace CLI..."
  npm link

  echo ""
  echo "Installed! Run:"
  echo "  codex-trace          # desktop app (default)"
  echo "  codex-trace --web    # web mode (opens browser)"
fi
