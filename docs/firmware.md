# Local device firmware

This first firmware is a local diagnostic interface for the exact Waveshare
ESP32-S3-RLCD-4.2. It is deliberately useful before any Mac bridge exists: the
device samples its own hardware, retains a bounded RAM event history, and
renders all status locally. Persistent event logging and WAV recording are
disabled until the user explicitly enables them; live microphone levels remain
visible without saving PCM.

## Controls

| Control | Connection | Click after boot | Long press |
|---|---:|---|---|
| KEY | GPIO18 | Back one hierarchy level | Return Home from anywhere |
| PWR | ECJ23001 power latch | Power on when fully off; no firmware event while on | Hardware power off |
| BOOT | GPIO0 | Open Menu, advance a selection, or perform the primary leaf action | Open/activate the selection, or perform the secondary leaf action |

Long presses fire once at 650 ms. Inputs are active-low and use a 35 ms
debounce window. Holding BOOT while resetting still selects the ESP32-S3 ROM
download mode; once firmware has booted, BOOT is available as an application
input.

The on-screen footer follows the physical order when facing the display:
`KEY | PWR | BOOT`.

The official schematic routes PWR into the ECJ23001 latch. Its outputs drive the
power MOSFET path and do not return to an ESP32 GPIO, so a powered firmware image
cannot use a short PWR press as Select. **Prepare power off** finalizes any open
WAV and flushes the event log only when that log was enabled; the user then
holds physical PWR. Firmware never substitutes fake deep sleep for shutdown.

## Navigation state machine

```text
Home --BOOT--> Menu --BOOT hold--> selected app
 ^              ^                     |
 |              |------ KEY ----------|
 |------------------- KEY hold --------|

Files --BOOT hold--> Player or Viewer
  ^                      |
  |-------- KEY ----------|
```

Menu, Files, and Settings use BOOT click to move and BOOT hold to activate. A
leaf screen uses BOOT click for its primary action and BOOT hold for its
secondary action. KEY click is always Back and KEY hold is always Home; leaving
Recorder finalizes an active WAV, while leaving Player stops and rewinds it.
Prepare power off in Settings is recommended immediately before a physical PWR
hold so files and the optional event log can be finalized first.

| View | BOOT click | BOOT hold |
|---|---|---|
| Home | Open Menu | Open Menu |
| Menu | Next app | Open selected app |
| Recorder | Record/stop | Open last recording in Player |
| Files | Next file | Open selected file |
| Player | Play/pause | Stop and rewind |
| Viewer | Next page | Return to file start |
| Live Audio | No destructive action | Open Recorder |
| Diagnostics | More history | Return to newest history |
| Settings | Next setting | Apply selected setting |

## Live data paths

| Path | Hardware/API | UI and event behavior |
|---|---|---|
| Display | ST7305, SPI3 at 24 MHz, GPIO 11/12/5/40/41; TE on GPIO6 | Native 300x400 portrait render, fixed spatial clock halftone, rising-edge interrupt-synchronized 15,000-byte frame |
| Wi-Fi/time | ESP-IDF station mode + SNTP | IPv4, RSSI, reconnect state, and network-synchronized IST date/time |
| Environment | SHTC3 at I2C address 0x70 on GPIO 13/14 | Temperature with Waveshare's -4.00 C board-heat compensation, humidity, CRC failures, recovery events |
| Battery | ADC1 channel 3, 12 dB attenuation, 3x divider | Raw ADC, millivolts, approximate percentage |
| Microphone | ES7210 through `esp_codec_dev`, 24 kHz stereo/16-bit | Dedicated high-priority capture task and bounded queue; live waveform and level telemetry; PCM is persisted only while Recorder is active |
| Speaker | ES8311 + NS4150B PA on GPIO46 | Bounded playback queue, PCM WAV playback, live player waveform/progress, configurable volume, direct 440 Hz Settings test |
| SD | 1-bit SDMMC on GPIO 38/21/39 | Recursive bounded catalog, WAV player, paged text viewer, hex fallback, optional NDJSON log |
| Runtime | ESP-IDF heap APIs | Free/min heap, free PSRAM, loop rate, LCD flush time |

Subsystem startup is independent. A missing SD card or failed sensor remains a
visible `ERR` status and recorded event; it does not prevent the rest of the UI
from running.

Events are retained in a 96-entry RAM ring. The Settings event-log switch is off
by default. Enabling it begins appending only subsequent events to
`/sdcard/events.ndj`; disabling it stops writes and does not retroactively dump
RAM history later.

Recorder creates 24 kHz stereo 16-bit PCM files named `R000001.WAV` and upward.
The 44-byte header is written initially and its RIFF/data lengths are finalized
on Stop or Prepare power off. Files are never loaded wholly into RAM: playback
streams bounded chunks, while Viewer reads paged text or a hexadecimal fallback.

Settings contains display refresh, speaker volume, a direct speaker test tone,
the event-log toggle, SD rescan, and Prepare power off.

## Credentials

The build reads `WIFI_SSID` and `WIFI_PASSWORD` from the ignored root
`.env.local` and embeds them in the firmware image. The real values are never
checked into Git. Use `.env.local.example` as the shape for another network.

## Verify without hardware

```sh
cargo +esp test --workspace --exclude hmi-firmware
cargo +esp run -p hmi-simulator -- artifacts/portrait-home.png home
./scripts/build-firmware.sh
```

The last command produces the ESP32-S3 release ELF at
`target/xtensa-esp32s3-espidf/release/hmi-firmware`. It builds only. Flashing is
a separate, explicitly authorized operation.
