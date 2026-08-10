# Build intent and upstream comparison

## Product interpretation

The requested repositories do not describe one existing product. Together,
they describe the ingredients for a new one:

```text
Mac-local data sources and adapters
              |
              | versioned data/events, not remote pixels
              v
ESP32-S3 service shell (Wi-Fi, HTTP/WS, USB, storage, OTA)
              |
              v
shared Rust state + Ratatui UI + embedded-graphics
        |                         |
        v                         v
host simulator              ST7305 driver
                                  |
                                  v
                       300x400 reflective LCD
```

The device owns interaction, state presentation, and rendering. The Mac owns
provider credentials, expensive integrations, durable history, and test data.
The same pure Rust UI/state code should render in a headless host simulator
before it is used by firmware.

This differs from a remote framebuffer: the network sends typed state and
events, while the ESP32 lays out and draws the interface locally. It also
differs from a standalone cloud client: provider keys should not normally live
on the device.

## Exact hardware contract

The platform is the Waveshare ESP32-S3-RLCD-4.2, not a generic e-paper board:

| Property | Contract |
|---|---|
| MCU/module | ESP32-S3-WROOM-1-N16R8, dual-core Xtensa LX7 at 240 MHz |
| Memory | 16 MB flash, 8 MB octal PSRAM |
| Display | ST7305, 4.2-inch reflective monochrome LCD, 300x400 native portrait |
| Frame storage | 1 bpp, 120,000 pixels, 15,000 packed bytes |
| Display SPI | SCLK 11, MOSI 12, DC 5, CS 40, reset 41, TE 6 |
| Buttons | BOOT 0, KEY 18 |
| Shared I2C | SDA 13, SCL 14 |
| Audio | ES8311 output, ES7210 microphone ADC; I2S pins 16/9/45/8/10, amp 46 |
| SDMMC | CLK 38, CMD 21, D0 39 in the official SD example |
| Native USB | USB Serial/JTAG on GPIO19/20; no separate UART bridge |

Use the official Waveshare repository as the authority for board wiring. Use
the independent projects as empirical evidence for performance and failure
modes, not as a reason to silently rename the controller to ST7306.

## What each reference contributes

| Reference | What it is | Reuse or learn | Boundary / risk |
|---|---|---|---|
| `waveshareteam/ESP32-S3-RLCD-4.2` | Official Arduino, ESP-IDF, ESPHome, XiaoZhi examples and binaries | Pin map, init sequence, peripherals, U8g2/LVGL bring-up | Example collection, not a product architecture; very large and contains binaries |
| `URL42/doom-esp32s3-rlcd` | Hardware-verified ESP-IDF Doom port | Best profiling evidence; packed 15 KB framebuffer, ordered dithering, SPI/transaction bottlenecks, calibration and de-ghost experiments | GPLv2; its row-by-row flush is tied to its buffer layout and measured panel behavior |
| `tigerxu255-lgtm/esp32-s3-rlcd-gb-emulator` | Local GB/GBC emulator with SD, audio, AP-mode phone controller | Component boundaries, local rendering, binary WebSocket input, rotation, partial-area packing experiment | GPLv2; its LVGL partial flush is explicitly marked uncalibrated and falls back to full flush |
| `art-jin/esp32-rlcd-apps` | ESP-IDF multi-app firmware | App lifecycle, cooperative shutdown, USB Serial/JTAG data ingress, WebSocket services, captive provisioning | Useful product patterns, but its docs/code use ST7306 naming and its C/LVGL app shell is not the target Rust core |
| `Gskdl78/claude-rlcd` | Node bridge plus LAN-connected local-rendering dashboard | NDJSON WebSocket protocol, reconnect/full-state replay, mDNS, state-store separation, simulator tests | Single-client and Claude-specific; hook installers mutate user configuration and should remain optional |
| `Winter-And-You-Gone/token-monitor-RLCD` | FastAPI usage bridge plus API-fed local LVGL dashboard | Mock endpoint, cache-before-poll, stale-data behavior, web preview, provider adapter separation | Provider-specific and broad; README declares MIT but the pinned root lacks a license file |
| `bitbank2/OneBitDisplay` | Generic C/C++ monochrome display library | Exact-board `LCD_ST7305B` init, 40 MHz speed-test path, packing/conversion techniques | Arduino/C++ API is not the Rust driver; 40 MHz must be confirmed on our board rather than assumed |
| `SixSigmaEngineer/waveshareesp32clock` | Patch overlay on a pinned XiaoZhi tree | Excellent reproducible-overlay pattern; RTC/NTP/SD configuration and hard-won PSRAM/TLS/audio stack lessons | Direct cloud keys on SD conflict with local-first credentials; custom code says MIT but lacks a root license file |
| `ratatui/mousefood` | no_std Ratatui backend for embedded-graphics | Chosen UI adapter; lets one Ratatui UI target an embedded `DrawTarget` | Hardware-agnostic and does not supply an ST7305 driver; default fonts/framebuffer affect flash/RAM |
| `embedded-graphics/simulator` | Host display simulator | Chosen deterministic render target, PNG snapshots, headless CI without SDL | Models pixels, not SPI packing, ghosting, panel timing, or electrical behavior |
| `esp-rs/esp-idf-hal` | Rust wrappers for ESP-IDF drivers | Chosen first hardware layer for SPI, GPIO, I2C, I2S, UART/USB-adjacent work | Community wrappers can lag ESP-IDF; pin a tested toolchain and crate set |
| `esp-rs/esp-idf-svc` | Rust wrappers for ESP-IDF services | Chosen first service layer for Wi-Fi, HTTP, WebSocket, NVS, OTA | Same community/toolchain caveat; service behavior still needs failure-injection tests |
| `esp-rs/esp-hal` | Officially supported bare-metal no_std HAL | Keep for isolated experiments and a possible later minimal firmware profile | Alternative to ESP-IDF, not an add-on; adopting it means rebuilding more networking/service integration and using async |

## Key decisions

### 1. Start with ESP-IDF Rust

The first complete device needs Wi-Fi provisioning, HTTP/WebSocket, NVS,
native USB logging/transport, SD, and eventually OTA. `esp-idf-hal` plus
`esp-idf-svc` minimizes integration risk while keeping application and UI code
in Rust. Bare-metal `esp-hal` stays in the vendor patch for evaluation but is
not part of the first firmware runtime.

### 2. Share state and rendering, not hardware code

The shared crate should contain:

- protocol types and validation;
- reducer/state machine;
- navigation and app model;
- Ratatui view functions;
- deterministic clocks and injected data sources for tests.

The firmware crate owns ESP-IDF handles, tasks, networking, storage, and the
ST7305 transport. The simulator crate owns window/headless output and fault
fixtures. Neither should leak its platform types into the shared core.

### 3. Build a dedicated ST7305 Rust driver

The driver should expose an embedded-graphics `DrawTarget<BinaryColor>` plus a
separate flush interface. Its first proven mode should use the official 24 MHz
SPI setting and a correctness-first full-frame path. Later hardware experiments
can compare 40 MHz, queued transactions, dirty rows, and aligned partial areas.

There is conflicting upstream evidence that must be resolved on the real panel:

- Doom reports that one flat 15 KB transfer produces vertical striping and
  requires per-row addressing for its packing.
- The GB project sends a flat 15 KB full frame with a different physical
  layout, while its partial-area path is not yet trusted by its own LVGL glue.
- OneBitDisplay uses controller-positioned strips and a conversion buffer.

Therefore, "full frame" and "single flat transfer" are not synonyms. The
packing contract, address-window units, CS lifetime, and transaction order must
be covered by golden byte-stream tests and then checked visually on hardware.

### 4. Make the protocol product-level

Use a versioned envelope over WebSocket on LAN, with USB Serial/JTAG as an
operator/recovery transport. Messages should include sequence/revision,
timestamp, source, payload type, and explicit snapshot versus delta semantics.
The device must reconnect, request or accept a full snapshot, reject malformed
or oversized messages, retain a last-known-good view, and visibly mark stale
data.

NDJSON is appropriate for diagnostics and low-rate state. High-rate controls
may use a compact binary message, as the GB controller does, but should still
map into the same typed input events.

### 5. Treat the simulator as a release gate, not hardware proof

Host tests should cover 300x400 and 400x300 layouts, deterministic snapshot
images, reconnect/replay, malformed and out-of-order protocol data, stale state,
slow source, network loss, and renderer panic isolation. A separate packer test
must verify the exact 15,000-byte panel representation.

Passing those tests proves shared behavior and pixel intent. It does not prove
SPI signaling, ghosting, current draw, audio, Wi-Fi coexistence, boot behavior,
or OTA recovery. Those require an explicitly authorized flash and physical
acceptance pass.

## Initial workspace shape

```text
crates/
  hmi-core/          protocol, reducer, navigation, Ratatui views (no_std + alloc)
  hmi-simulator/     host runner, fixtures, snapshot and fault tests
  st7305/            packing, address windows, DrawTarget, transport traits
  hmi-firmware/      ESP-IDF tasks, services, board wiring and persistence
  hmi-bridge/        Mac-local adapters, cache and device transport
vendor/
  upstream/          ignored, pinned source checkouts
  patches/           only unavoidable upstream diffs
```

That structure is a design direction, not yet generated code. The next safe
implementation milestone is the shared binary UI core plus headless simulator,
followed by byte-exact ST7305 packing tests, before firmware or flashing.

