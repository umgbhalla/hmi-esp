#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

target=xtensa-esp32s3-espidf
export ESP_IDF_SYS_ROOT_CRATE=hmi-touch349-firmware
if [ -f .env.local ]; then
    set -a
    . ./.env.local
    set +a
fi
HMI_WIFI_SSID=${HMI_WIFI_SSID:-${WIFI_SSID:-}}
HMI_WIFI_PASSWORD=${HMI_WIFI_PASSWORD:-${WIFI_PASSWORD:-}}
export HMI_WIFI_SSID HMI_WIFI_PASSWORD
: "${HMI_WIFI_SSID:?Set HMI_WIFI_SSID for the Touch349 firmware build}"
: "${HMI_WIFI_PASSWORD:?Set HMI_WIFI_PASSWORD for the Touch349 firmware build}"

# Extra ESP-IDF C components are compiled inside esp-idf-sys. Clean that one
# package so a display-driver edit can never reuse a stale native object.
cargo +esp clean -p esp-idf-sys --release --target "$target"
cargo +esp build \
  --locked \
  -p hmi-touch349-firmware \
  --release \
  --target "$target" \
  -Zbuild-std=std,panic_abort
