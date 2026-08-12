#[cfg(not(target_os = "espidf"))]
fn main() {
    println!("Touch349 product firmware targets ESP32-S3");
}

#[cfg(target_os = "espidf")]
mod firmware {
    use std::{
        slice, thread,
        time::{Duration, Instant},
    };

    use anyhow::ensure;
    use esp_idf_sys::hmi_touch349::{
        hmi_touch349_backlight_set, hmi_touch349_flush_full, hmi_touch349_flush_stats_t,
        hmi_touch349_framebuffer, hmi_touch349_init, hmi_touch349_power_button_pressed,
        hmi_touch349_power_off, hmi_touch349_sd_mount, hmi_touch349_sd_stats_t,
        hmi_touch349_touch_read,
    };
    use hmi_core::{
        decode_touch349_packet, render_touch349_dashboard, BatteryTelemetry, DashboardState,
        Health, StorageTelemetry, Touch349FrameBuffer,
    };

    const PIXELS: usize = hmi_core::TOUCH349_PIXELS;
    const POWER_HOLD: Duration = Duration::from_millis(1_200);
    const TOUCH_RELEASE_DEBOUNCE: Duration = Duration::from_millis(55);
    const TOUCH_POLL: Duration = Duration::from_millis(20);

    fn flush() -> anyhow::Result<hmi_touch349_flush_stats_t> {
        let mut stats = hmi_touch349_flush_stats_t {
            flush_us: 0,
            dma_wait_us: 0,
            bands: 0,
        };
        let result = unsafe { hmi_touch349_flush_full(&mut stats) };
        ensure!(result == 0, "frame flush failed: {result}");
        Ok(stats)
    }

    fn render(display: &mut Touch349FrameBuffer<'_>, state: &DashboardState) -> anyhow::Result<()> {
        render_touch349_dashboard(display, state)
            .expect("Touch349 framebuffer drawing is infallible");
        flush()?;
        Ok(())
    }

    fn initial_state(sd: &hmi_touch349_sd_stats_t) -> DashboardState {
        let mut state = DashboardState::default();
        state.display_brightness = 80;
        state.display_fps = 10;
        state.battery = BatteryTelemetry {
            health: Health::Unknown,
            millivolts: 0,
            percent: 78,
            raw: 0,
        };
        state.storage = StorageTelemetry {
            health: if sd.mounted == 1 {
                Health::Ok
            } else {
                Health::Error
            },
            mounted: sd.mounted == 1,
            total_kib: sd.capacity_bytes / 1024,
            free_kib: 0,
            event_bytes_written: 0,
            pending_events: 0,
            write_errors: 0,
        };
        state.audio.health = Health::Unknown;
        state.wifi.health = Health::Unknown;
        state.clock.health = Health::Unknown;
        state.runtime.loop_hz = 50;
        state.runtime.free_psram = 0;
        state.record(0, "BOOT", "Touch349 product UI started");
        state
    }

    pub fn run() -> anyhow::Result<()> {
        esp_idf_sys::link_patches();
        println!("PRODUCT BOOT Touch349 V2");
        let init_result = unsafe { hmi_touch349_init() };
        ensure!(
            init_result == 0,
            "display and touch init failed: {init_result}"
        );

        let mut pixel_count = 0usize;
        let pointer = unsafe { hmi_touch349_framebuffer(&mut pixel_count) };
        ensure!(!pointer.is_null(), "framebuffer pointer is null");
        ensure!(pixel_count == PIXELS, "framebuffer length is {pixel_count}");
        let pixels = unsafe { slice::from_raw_parts_mut(pointer, pixel_count) };
        let mut display = Touch349FrameBuffer::new(pixels).expect("verified framebuffer size");

        let mut sd = hmi_touch349_sd_stats_t {
            capacity_bytes: 0,
            sector_size: 0,
            mounted: 0,
        };
        let sd_result = unsafe { hmi_touch349_sd_mount(&mut sd) };
        let mut state = initial_state(&sd);
        render(&mut display, &state)?;
        let backlight_result = unsafe { hmi_touch349_backlight_set(0, true) };
        ensure!(
            backlight_result == 0,
            "backlight enable failed: {backlight_result}"
        );
        println!(
            "PRODUCT READY sd_mounted={} sd_error={sd_result} capacity_bytes={}",
            sd.mounted, sd.capacity_bytes
        );

        let boot_time = Instant::now();
        let mut power_pressed_since: Option<Instant> = None;
        let mut last_touch = None;
        let mut last_touch_seen = Instant::now();
        let mut redraw = false;
        let mut heartbeat = Instant::now();

        loop {
            let now_ms = boot_time.elapsed().as_millis() as u64;
            let power_pressed = unsafe { hmi_touch349_power_button_pressed() };
            if power_pressed {
                let started = *power_pressed_since.get_or_insert_with(Instant::now);
                let held = started.elapsed();
                if held >= POWER_HOLD {
                    println!("POWER OFF threshold reached; releasing SYS_EN");
                    thread::sleep(Duration::from_millis(250));
                    unsafe { hmi_touch349_power_off() };
                    unreachable!("deep sleep returned");
                }
            } else if power_pressed_since.take().is_some() {
                println!("POWER HOLD cancelled");
            }

            let mut response = [0u8; 32];
            let touch_result = unsafe { hmi_touch349_touch_read(response.as_mut_ptr()) };
            if touch_result == 0 {
                if let Some(point) = decode_touch349_packet(&response) {
                    last_touch = Some(point);
                    last_touch_seen = Instant::now();
                } else if let Some(point) = last_touch {
                    if last_touch_seen.elapsed() >= TOUCH_RELEASE_DEBOUNCE {
                        let brightness_changed = state.apply_touch349(point.x, point.y, now_ms);
                        println!(
                            "TOUCH x={} y={} view={}",
                            point.x,
                            point.y,
                            state.view.title()
                        );
                        if brightness_changed {
                            let duty = 255u16
                                .saturating_sub(u16::from(state.display_brightness) * 255 / 100)
                                as u8;
                            unsafe { hmi_touch349_backlight_set(duty, true) };
                        }
                        last_touch = None;
                        redraw = true;
                    }
                }
            }

            state.runtime.uptime_ms = now_ms;
            if redraw {
                render(&mut display, &state)?;
                redraw = false;
            }
            if heartbeat.elapsed() >= Duration::from_secs(2) {
                println!(
                    "PRODUCT HEARTBEAT view={} sd_mounted={} touch_result={touch_result}",
                    state.view.title(),
                    sd.mounted
                );
                heartbeat = Instant::now();
            }
            thread::sleep(TOUCH_POLL);
        }
    }
}

#[cfg(target_os = "espidf")]
fn main() -> anyhow::Result<()> {
    firmware::run()
}
