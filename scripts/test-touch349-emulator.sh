#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
page="file://$repo_root/emulator/touch349/index.html?selftest=1"

headless_shell="$HOME/Library/Caches/ms-playwright/chromium_headless_shell-1223/chrome-headless-shell-mac-arm64/chrome-headless-shell"

if [ -n "${CHROME_BIN:-}" ] && [ -x "$CHROME_BIN" ]; then
  chrome=$CHROME_BIN
elif [ -x "$headless_shell" ]; then
  chrome=$headless_shell
elif [ -x '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' ]; then
  chrome='/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
elif command -v chromium >/dev/null 2>&1; then
  chrome=$(command -v chromium)
elif command -v google-chrome >/dev/null 2>&1; then
  chrome=$(command -v google-chrome)
else
  printf '%s\n' 'Chrome or Chromium is required for the emulator browser test.' >&2
  exit 1
fi

profile=$(mktemp -d)
dom=$(mktemp)
cleanup() {
  rm -rf "$profile"
  rm -f "$dom"
}
trap cleanup EXIT HUP INT TERM

"$chrome" \
  --headless=new \
  --disable-gpu \
  --disable-background-networking \
  --disable-component-update \
  --disable-features=OptimizationHints,MediaRouter \
  --no-first-run \
  --no-default-browser-check \
  --no-process-singleton-dialog \
  --user-data-dir="$profile" \
  --virtual-time-budget=7000 \
  --dump-dom "$page" >"$dom" 2>/dev/null

if ! grep -q 'data-selftest="pass"' "$dom"; then
  grep -o 'data-selftest-error="[^"]*"' "$dom" >&2 || true
  exit 1
fi

grep -o 'data-selftest-checks="[^"]*"' "$dom"
