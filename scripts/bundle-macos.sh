#!/usr/bin/env bash
# Build a macOS .app bundle for the Slint GUI so it launches like a real app (no terminal window).
# Usage: scripts/bundle-macos.sh [debug|release]
# Uses the committed icon (packaging/AppIcon.icns) and Info.plist (packaging/Info.plist).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PROFILE="${1:-debug}"
BIN="target/$PROFILE/aiclean-gui"
if [ ! -x "$BIN" ]; then
  echo "error: $BIN not found. Build it first:"
  echo "  cargo build -p cleanup-gui $([ "$PROFILE" = release ] && echo --release)"
  exit 1
fi

APP="dist/llm-cleanup.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/aiclean-gui"
cp packaging/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
cp packaging/Info.plist "$APP/Contents/Info.plist"
touch "$APP"
echo "Built $APP"
