#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
page="$repo_root/emulator/touch349/index.html"

case "$(uname -s)" in
  Darwin) open "$page" ;;
  Linux) xdg-open "$page" ;;
  *) printf 'Open %s in a browser.\n' "$page" ;;
esac
