# ESP32-S3-Touch-LCD-3.49 V2 extension plan

## Decision

Add the Waveshare ESP32-S3-Touch-LCD-3.49 V2 as an exact-board target. Preserve the existing ESP32-S3-RLCD-4.2 firmware and `st7305` driver as an independent target.

Do not create a universal display driver or runtime board detector. Share application state, reducers, telemetry semantics, media logic, and formatting helpers. Keep display transport, pin maps, layout geometry, touch decoding, reset, backlight, SD, and peripheral initialization board-specific.

The first deliverable is a V2 display-and-touch bring-up image that visibly renders a checkerboard and RGB color bars. Production UI and peripheral integration follow only after that physical gate passes.

## Source baseline

| Source | Pinned commit | Use |
|---|---|---|
| Current repository | `3b3757d18da36ded08aa442e696a79072f2626d5` | Product architecture and RLCD regression baseline |
| Official Waveshare V1 tree | `def6edd0b6e1925ed09702eed01a2f181afdf8c1` | Revision comparison only |
| Official Waveshare V2 tree | `1c157e6e8e68b89fd4dc400f46bf1724cb64a57e` | V2 pins, components, initialization, and examples |

The source trees do not prove the stated 2026-06-08 shipment transition. Select V1 or V2 explicitly from physical board identity, not purchase date.

## Hardware contract

| Capability | Touch LCD 3.49 V2 contract | Initial support |
|---|---|---|
| Display | AXS15231B, 172x640, RGB565 | Required in M1 |
| Display bus | SPI3 QSPI, CS9, CLK10, D0-D3 GPIO11-14, mode 3, 40 MHz | Required in M1 |
| Display reset | TCA9554 P5 | Required in M1 |
| Backlight | GPIO42 PWM plus TCA9554 P1 enable | Required in M1 |
| TE | GPIO21 | Observe after M1; do not gate initial flush |
| Touch | AXS15231B touch, I2C SDA17/SCL18, address `0x3b` | Required in M2 |
| System I2C | SDA47/SCL48 | Required for expander; shared later |
| IO expander | TCA9554 at `0x20`; interrupt P0/GPIO8, BL P1, LCD reset P5 | Required in M1 |
| IMU | QMI8658 at `0x6b` | Deferred to M5 |
| RTC | PCF85063 at `0x51` | Deferred to M5 |
| Battery | GPIO4, ADC1 channel 3; nominal official multiplier 3 | M5, calibrated on hardware |
| SD | 1-bit SDMMC CMD39, D0 40, CLK41 | M4 |
| Audio | ES7210 input, ES8311 output; MCLK7/BCLK15/WS46/DIN6/DOUT45 | M4 |
| Buttons | BOOT GPIO0 and SYS_OUT GPIO16 in official factory example | Optional; touch is primary HMI |
| Environment sensor | No SHTC3 contract established for this board | Report unsupported, never error |

V1 must remain a later, separate target because it drives LCD reset directly on GPIO21 and backlight on GPIO8. Those pins are not interchangeable with V2.

## Architecture

```text
shared product logic
  hmi-core
    model + reducer + semantic UiCommand
    telemetry + capability states
    media/viewer/poweroff state
  shared media runtime
    WAV recorder/player + event log

board-specific presentation and hardware
  RLCD-4.2 target
    300x400 BinaryColor renderer
    existing ST7305 + hmi_board implementation
  Touch349-V2 target
    172x640 Rgb565 renderer + hit regions
    AXS15231B/TCA9554 C adapter
    V2 touch/audio/SD/sensor services
```

### Build boundary

Use a separate `hmi-firmware-touch349-v2` package for initial bring-up. This keeps its ESP-IDF component graph and SDK defaults separate from the working RLCD package. Once both targets are stable, shared Rust application-loop code may move to a library used by both binaries.

Do not begin with Cargo features inside the existing firmware package: board selection also controls CMake components, codec profiles, SDK configuration, and exclusive SPI3 ownership.

### UI boundary

Move the current renderer mechanically to an RLCD-specific module without changing its output. Add a separate Touch349 renderer. Share only geometry-free formatting helpers.

Proposed contracts:

```rust
pub enum UiCommand {
    Back,
    Home,
    Next,
    Activate,
    SelectMenu(u8),
    SelectFile(usize),
    Scroll { delta: i16 },
    Action(UiAction),
}

pub fn render_rlcd42_dashboard<D>(display: &mut D, state: &DashboardState)
    -> Result<(), D::Error>;

pub fn render_touch349_dashboard<D>(display: &mut D, state: &DashboardState)
    -> Result<(), D::Error>;

pub fn touch349_hit_test(view: View, point: TouchPoint) -> Option<UiCommand>;
```

Existing BOOT/KEY gestures and new touch targets must both dispatch `UiCommand` through the same reducer. Raw touch coordinates and physical button names must not enter `DashboardState`.

### Display boundary

Vendor the smallest required official V2 components with license and pinned provenance:

- `espressif/esp_lcd_axs15231b`
- `espressif/esp_io_expander_tca9554`

Wrap them in a product-owned V2 C adapter. Do not copy the Waveshare LVGL application and do not add AXS commands to `st7305`.

The adapter owns QSPI, panel handles, TCA9554, reset/backlight order, one internal DMA buffer, completion synchronization, touch I2C, and byte-order conversion. Rust owns the RGB565 frame producer and safe wrapper.

```c
int hmi_touch349_v2_display_init(void);
uint16_t *hmi_touch349_v2_framebuffer(size_t *pixel_count);
int hmi_touch349_v2_flush_full(void);
int hmi_touch349_v2_touch_read(uint16_t *x, uint16_t *y, bool *pressed);
int hmi_touch349_v2_backlight_set(uint8_t duty, bool enabled);
```

## Rendering rate and memory budget

| Quantity | Value |
|---|---:|
| Pixels | 110,080 |
| Full RGB565 frame | 220,160 bytes |
| Official 64-row DMA band | 22,016 bytes |
| Bands per full frame | 10 |
| Raw QSPI payload rate | 20 MB/s |
| Raw full-frame wire time | about 11.0 ms |
| Raw theoretical ceiling | about 90 FPS |
| Realistic initial full-frame target | 12-25 ms, about 40-80 FPS transport-only |
| Recommended product cadence initially | 4-10 FPS |
| Later interactive target after measurement | 30 FPS |

The official firmware does not promise a frame rate. The 90 FPS value is only a bus-bandwidth ceiling and excludes DMA setup, PSRAM copies, commands, callbacks, rendering, scheduling, audio/SD contention, and panel behavior. Do not claim 60 FPS until it is measured on the device. Initial acceptance is p95 full flush below 25 ms, p99 below 33 ms, zero transfer failures for ten minutes, and no visible corruption.

Memory budget:

| Allocation | Bytes | Required memory |
|---|---:|---|
| One framebuffer | 220,160 | PSRAM |
| One DMA band | 22,016 | Internal DMA-capable heap |
| Total baseline | 242,176 | Split as above |
| Optional second frame | +220,160 | PSRAM, deferred |

Never place the full frame on a task stack. The current SDK policy does not guarantee that a 22,016-byte allocation is internal, so request DMA-capable internal memory explicitly.

## Milestones

### M0 - Preserve the RLCD baseline

- Capture deterministic simulator images for all existing views.
- Move the current renderer without visual changes.
- Build and test the existing RLCD firmware.
- Confirm `st7305`, RLCD pins, and RLCD C component are unchanged.

Gate: RLCD tests and screenshots are unchanged. A physical RLCD smoke test remains required before release.

### M1 - V2 display-only bring-up

- Add the separate V2 firmware package and SDK defaults.
- Vendor pinned AXS15231B and TCA9554 components.
- Implement system I2C, expander, GPIO42 PWM, reset order, QSPI, PSRAM frame, and ten-band blocking flush.
- Render the dashboard as the first frame while the backlight remains disabled.
- Log frame size, band count, flush duration, DMA wait, free internal heap, and free PSRAM.

Gate: visible full-screen pattern on the physical V2 panel with correct colors/orientation and no missing bands. A serial `flush complete` message alone does not pass.

### M2 - Touch

- Poll the official 11-byte command/32-byte response path.
- Decode, clamp, transform, and expose press/move/release.
- Start with the official mapping `x = raw_y`, `y = 640 - raw_x`, then calibrate physically.
- Add hit testing that emits semantic `UiCommand` values.
- Keep GPIO18 out of the RLCD KEY path in this target.

Gate: four corners, center, representative targets, drag, and release all work without ghost input during display flush.

### M3 - Touch349 UI and simulator

- Add an independent 172x640 RGB565 layout for all nine views.
- Use 48-pixel minimum rows and a 56-64-pixel navigation area.
- Add explicit simulator board selection and tracked snapshots.
- Cover empty/long-file states, recording/playback, stale/error/unavailable telemetry, checkerboard, and color bars.

Gate: all views render in bounds; hit regions align with visual targets; RLCD snapshots remain unchanged.

### M4 - Shared media, audio, and storage

- Reuse `MediaRuntime`, WAV recorder/player, event recorder, and normalized board ABI.
- Select official `S3_LCD_3_49` codec profile.
- Add V2 SDMMC mapping.
- Prove microphone capture, speaker tone, record, playback, SD write/sync/readback, and simultaneous display activity.

Gate: audible/physical proof and zero queue overruns/underruns in a ten-minute integrated run.

### M5 - Capabilities, IMU, RTC, and battery

- Add capability state separate from health: unsupported, supported/no sample, healthy, stale, error.
- Add QMI8658, then PCF85063, then battery sampling independently.
- Mark environment sensing unsupported unless actual hardware establishes a sensor.
- Calibrate battery conversion against a meter.

Gate: chip identity and motion response, RTC set/read/retention, plausible calibrated battery voltage, and honest unavailable UI for absent hardware.

### M6 - Performance tuning and production integration

- Measure rendering time separately from transfer time.
- Measure flush p50/p95/p99, missed frames, heap minima, task stacks, audio queues, and I2C errors under integrated load.
- Add a display task, snapshot handoff, double buffering, TE pacing, or partial refresh only when measurements justify them.
- Unify shared application-loop code without merging board components.

Gate: measured 30 FPS only if required, no visible tearing, no media regressions, and physical smoke tests on both boards.

## Test matrix

| Layer | Required evidence |
|---|---|
| Domain | Existing button semantics preserved through `UiCommand`; action and poweroff tests pass |
| RGB565 frame | Exact length, bounds, stride, primary-color byte order, ten bands |
| Touch | Packet decode, transform, clamp, boundaries, release, hit-test mapping |
| Build | RLCD and V2 packages build independently; forbidden component/pin cross-links absent |
| Simulator | Deterministic snapshots for both panels and all important states |
| Display hardware | Directly observed pattern, orientation, color, border, and stability |
| Integrated hardware | Touch, audio, SD, IMU, RTC, battery, Wi-Fi, and display exercised together |

## Explicit deferrals

- Touch349 V1 support.
- Runtime revision detection.
- Generic display HAL or generic theme system.
- Dirty rectangles and arbitrary partial windows.
- TE-gated scheduling before GPIO21 timing is measured.
- Double buffering before a single-frame path is stable.
- Gesture framework and continuous scrolling.
- Claiming SHTC3/environment support on the 3.49.
- Claiming an FPS number not measured on the physical device.

## First implementation patch

The first patch should contain only:

1. Pinned V2 component provenance and licenses.
2. A separate V2 display-only firmware package.
3. TCA9554 reset/backlight and exact 40 MHz QSPI initialization.
4. One PSRAM RGB565 frame and one internal 64-row DMA band.
5. Blocking ten-band full-frame flush with timing telemetry.
6. Checkerboard, borders, and RGB color bars.
7. Host tests for dimensions, strip planning, and byte order.

It is complete only when the pattern is visibly correct on the physical V2 display.
