#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

if [ ! -f .env.local ]; then
  echo "missing .env.local; copy .env.local.example and fill Wi-Fi credentials" >&2
  exit 1
fi

exec cargo +esp build \
  -p hmi-firmware \
  --release \
  --target xtensa-esp32s3-espidf \
  -Zbuild-std=std,panic_abort
