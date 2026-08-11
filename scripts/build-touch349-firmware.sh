#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

target=xtensa-esp32s3-espidf
export ESP_IDF_SYS_ROOT_CRATE=hmi-touch349-firmware

# Extra ESP-IDF C components are compiled inside esp-idf-sys. Clean that one
# package so a display-driver edit can never reuse a stale native object.
cargo +esp clean -p esp-idf-sys --release --target "$target"
cargo +esp build \
  --locked \
  -p hmi-touch349-firmware \
  --release \
  --target "$target" \
  -Zbuild-std=std,panic_abort
