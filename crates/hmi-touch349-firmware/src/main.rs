#[cfg(not(target_os = "espidf"))]
fn main() {
    println!("Touch349 product firmware targets ESP32-S3");
}

#[cfg(target_os = "espidf")]
mod firmware {
    use std::{
        env,
        ffi::CString,
        format, fs, slice, thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use anyhow::ensure;
    use esp_idf_sys::hmi_touch349::{
        hmi_touch349_backlight_set, hmi_touch349_battery_read, hmi_touch349_flush_full,
        hmi_touch349_flush_stats_t, hmi_touch349_framebuffer, hmi_touch349_free_heap,
        hmi_touch349_free_psram, hmi_touch349_init, hmi_touch349_network_start,
        hmi_touch349_network_stats, hmi_touch349_network_stats_t,
        hmi_touch349_power_button_pressed, hmi_touch349_power_off, hmi_touch349_sd_mount,
        hmi_touch349_sd_stats_t, hmi_touch349_touch_read,
    };
    use hmi_core::{
        decode_touch349_packet, render_touch349_dashboard, BatteryTelemetry, DashboardState,
        FileEntry, FileKind, Health, StorageTelemetry, Touch349FrameBuffer,
    };

    const PIXELS: usize = hmi_core::TOUCH349_PIXELS;
    const POWER_HOLD: Duration = Duration::from_millis(1_200);
    const MIN_BRIGHTNESS_PERCENT: u8 = 20;
    const TOUCH_RELEASE_DEBOUNCE: Duration = Duration::from_millis(55);
    const TOUCH_POLL: Duration = Duration::from_millis(20);
    const WIFI_SSID: &str = env!("HMI_WIFI_SSID");
    const WIFI_PASSWORD: &str = env!("HMI_WIFI_PASSWORD");

    #[derive(Clone, Copy)]
    struct RenderStats {
        frame_us: u32,
        flush_us: u32,
    }

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

    fn render(
        display: &mut Touch349FrameBuffer<'_>,
        state: &mut DashboardState,
    ) -> anyhow::Result<RenderStats> {
        let started = Instant::now();
        render_touch349_dashboard(display, state)
            .expect("Touch349 framebuffer drawing is infallible");
        let stats = flush()?;
        let frame_us = started.elapsed().as_micros().min(u128::from(u32::MAX)) as u32;
        state.runtime.display_flush_ms = (stats.flush_us / 1_000).min(u32::from(u16::MAX)) as u16;
        state.display_fps = if frame_us == 0 {
            0
        } else {
            (1_000_000 / frame_us).min(u32::from(u8::MAX)) as u8
        };
        Ok(RenderStats {
            frame_us,
            flush_us: stats.flush_us,
        })
    }

    fn initial_state(sd: &hmi_touch349_sd_stats_t) -> DashboardState {
        let mut state = DashboardState::default();
        state.display_brightness = 80;
        state.display_fps = 0;
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
            free_kib: sd.free_bytes / 1024,
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

    fn scan_sd_files() -> Vec<FileEntry> {
        let Ok(entries) = fs::read_dir("/sdcard") else {
            return Vec::new();
        };
        let mut files = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let metadata = entry.metadata().ok()?;
                if !metadata.is_file() {
                    return None;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                let extension = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
                let kind = match extension.as_str() {
                    "wav" => FileKind::Wav,
                    "txt" | "log" | "csv" => FileKind::Text,
                    _ => FileKind::Other,
                };
                Some(FileEntry {
                    name,
                    size: metadata.len(),
                    kind,
                })
            })
            .take(64)
            .collect::<Vec<_>>();
        files.sort_by(|left, right| left.name.cmp(&right.name));
        files
    }

    fn battery_percent(millivolts: u32) -> u8 {
        if millivolts <= 3_300 {
            0
        } else if millivolts >= 4_200 {
            100
        } else {
            ((millivolts - 3_300) * 100 / 900) as u8
        }
    }

    fn civil_date(days_since_epoch: i64) -> (u16, u8, u8) {
        let z = days_since_epoch + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let day_of_era = z - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let mut year = year_of_era + era * 400;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let month_prime = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
        let month = month_prime + if month_prime < 10 { 3 } else { -9 };
        year += i64::from(month <= 2);
        (year as u16, month as u8, day as u8)
    }

    fn update_live_data(state: &mut DashboardState, network: &hmi_touch349_network_stats_t) {
        state.runtime.free_heap = unsafe { hmi_touch349_free_heap() };
        state.runtime.free_psram = unsafe { hmi_touch349_free_psram() };
        state.wifi.health = if network.connected == 1 {
            Health::Ok
        } else {
            Health::Stale
        };
        state.wifi.ssid = WIFI_SSID.into();
        state.wifi.rssi_dbm = i16::from(network.rssi_dbm);
        state.wifi.ipv4 = if network.connected == 1 {
            let octets = network.ipv4.to_ne_bytes();
            format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3])
        } else {
            "CONNECTING".into()
        };

        let mut raw = 0u16;
        let mut millivolts = 0u32;
        if unsafe { hmi_touch349_battery_read(&mut raw, &mut millivolts) } == 0 {
            state.battery = BatteryTelemetry {
                health: Health::Ok,
                millivolts,
                percent: battery_percent(millivolts),
                raw,
            };
        } else {
            state.battery.health = Health::Error;
        }

        if network.time_synced == 1 {
            if let Ok(elapsed) = SystemTime::now().duration_since(UNIX_EPOCH) {
                let local = elapsed.as_secs() + 19_800;
                let seconds = local % 86_400;
                let days = (local / 86_400) as i64;
                let (year, month, day) = civil_date(days);
                state.clock.health = Health::Ok;
                state.clock.zone = "IST";
                state.clock.year = year;
                state.clock.month = month;
                state.clock.day = day;
                state.clock.weekday = ((days + 4).rem_euclid(7)) as u8;
                state.clock.hour = (seconds / 3_600) as u8;
                state.clock.minute = ((seconds % 3_600) / 60) as u8;
                state.clock.second = (seconds % 60) as u8;
            }
        } else {
            state.clock.health = Health::Stale;
        }
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
            free_bytes: 0,
            sector_size: 0,
            mounted: 0,
        };
        let sd_result = unsafe { hmi_touch349_sd_mount(&mut sd) };
        let mut state = initial_state(&sd);
        state.files = scan_sd_files();
        let mut render_stats = render(&mut display, &mut state)?;
        let initial_duty =
            255u16.saturating_sub(u16::from(state.display_brightness) * 255 / 100) as u8;
        let backlight_result = unsafe { hmi_touch349_backlight_set(initial_duty, true) };
        ensure!(
            backlight_result == 0,
            "backlight enable failed: {backlight_result}"
        );
        let wifi_ssid = CString::new(WIFI_SSID).expect("Wi-Fi SSID contains NUL");
        let wifi_password = CString::new(WIFI_PASSWORD).expect("Wi-Fi password contains NUL");
        let network_start =
            unsafe { hmi_touch349_network_start(wifi_ssid.as_ptr(), wifi_password.as_ptr()) };
        println!("NETWORK START result={network_start} ssid={WIFI_SSID}");
        println!(
            "PRODUCT READY sd_mounted={} sd_error={sd_result} capacity_bytes={} free_bytes={} files={}",
            sd.mounted,
            sd.capacity_bytes,
            sd.free_bytes,
            state.files.len()
        );

        let boot_time = Instant::now();
        let mut power_pressed_since: Option<Instant> = None;
        let mut last_touch = None;
        let mut last_touch_seen = Instant::now();
        let mut redraw = false;
        let mut heartbeat = Instant::now();
        let mut live_data_refresh = Instant::now() - Duration::from_secs(2);
        let mut network = hmi_touch349_network_stats_t {
            ipv4: 0,
            rssi_dbm: 0,
            connected: 0,
            time_synced: 0,
        };

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
                            state.display_brightness =
                                state.display_brightness.max(MIN_BRIGHTNESS_PERCENT);
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
            if live_data_refresh.elapsed() >= Duration::from_secs(1) {
                unsafe { hmi_touch349_network_stats(&mut network) };
                update_live_data(&mut state, &network);
                redraw = true;
                live_data_refresh = Instant::now();
            }
            if redraw {
                render_stats = render(&mut display, &mut state)?;
                redraw = false;
            }
            if heartbeat.elapsed() >= Duration::from_secs(2) {
                println!(
                    "PRODUCT HEARTBEAT view={} touch={} sd={} wifi={} time={} battery={}mV/{}% heap={}K psram={}K frame={}us flush={}us fps={}",
                    state.view.title(),
                    touch_result,
                    sd.mounted,
                    network.connected,
                    network.time_synced,
                    state.battery.millivolts,
                    state.battery.percent,
                    state.runtime.free_heap / 1024,
                    state.runtime.free_psram / 1024,
                    render_stats.frame_us,
                    render_stats.flush_us,
                    state.display_fps,
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
