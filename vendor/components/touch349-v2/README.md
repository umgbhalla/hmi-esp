# Touch349 V2 ESP-IDF components

These Apache-2.0 components are copied unchanged from the managed component
set in the official Waveshare `ESP32-S3-Touch-LCD-3.49-V2` repository at
commit `1c157e6e8e68b89fd4dc400f46bf1724cb64a57e`. The TCA9554 manifest's remote
dependency is replaced by an explicit CMake dependency on the adjacent pinned
`esp_io_expander`, keeping the firmware build local and reproducible.

| Component | Version | Upstream commit |
|---|---:|---|
| `esp_lcd_axs15231b` | `1.0.1~1` | `59a708e5356f7922cda1ed026676477afafd9676` |
| `esp_io_expander_tca9554` | `2.0.3` | `53f6127ba3a1dd80fbdf9a76b759ccd1a8dc0101` |
| `esp_io_expander` | `1.2.1` | `eb76dc6ecf21ccc4ee7ee58bfea3d3d31fa090cf` |
| `esp_lcd_touch` | `1.2.1` | See its bundled manifest |
| `cmake_utilities` | `0.5.3` | See its bundled manifest |

Every component retains its manifest, checksums, SPDX header, and license
file. Product-specific GPIO, reset, backlight, DMA, and touch behavior lives in
`crates/hmi-touch349-firmware/components/hmi_touch349`.
