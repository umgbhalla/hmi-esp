#[cfg(not(target_os = "espidf"))]
fn main() {
    println!("Touch349 firmware targets ESP32-S3; use hmi-simulator for host previews");
}

#[cfg(target_os = "espidf")]
fn main() -> anyhow::Result<()> {
    firmware::run()
}

#[cfg(target_os = "espidf")]
mod firmware {
    use std::{
        slice, thread,
        time::{Duration, Instant},
    };

    use anyhow::{anyhow, Context};
    use embedded_svc::wifi::{ClientConfiguration, Configuration};
    use esp_idf_hal::peripherals::Peripherals;
    use esp_idf_svc::{
        eventloop::EspSystemEventLoop,
        log::EspLogger,
        nvs::EspDefaultNvsPartition,
        wifi::{BlockingWifi, EspWifi},
    };
    use hmi_core::{
        render_touch349_dashboard, BatteryTelemetry, ClockTelemetry, DashboardState, Health,
        Touch349FrameBuffer, UiAction, TOUCH349_PIXELS,
    };
    use log::{info, warn};

    use esp_idf_sys::hmi_touch349::{
        hmi_touch349_backlight_set, hmi_touch349_flush_full, hmi_touch349_flush_stats_t,
        hmi_touch349_framebuffer, hmi_touch349_init, hmi_touch349_time_init,
        hmi_touch349_time_read, hmi_touch349_touch_read,
    };

    const WIFI_SSID: &str = env!("HMI_WIFI_SSID", "set WIFI_SSID in .env.local");
    const WIFI_PASSWORD: &str = env!("HMI_WIFI_PASSWORD", "set WIFI_PASSWORD in .env.local");
    const TIMEZONE: &[u8] = b"IST-5:30\0";

    pub fn run() -> anyhow::Result<()> {
        esp_idf_sys::link_patches();
        EspLogger::initialize_default();
        info!("Touch349 V2 bring-up firmware booting");

        let peripherals = Peripherals::take().context("take ESP-IDF peripherals")?;
        if unsafe { hmi_touch349_init() } != 0 {
            return Err(anyhow!("Touch349 display initialization failed"));
        }
        let mut pixel_count = 0usize;
        let frame_pointer = unsafe { hmi_touch349_framebuffer(&mut pixel_count) };
        if frame_pointer.is_null() || pixel_count != TOUCH349_PIXELS {
            return Err(anyhow!("Touch349 framebuffer contract mismatch"));
        }
        let pixels = unsafe { slice::from_raw_parts_mut(frame_pointer, pixel_count) };
        let mut framebuffer = Touch349FrameBuffer::new(pixels)
            .ok_or_else(|| anyhow!("Touch349 framebuffer length rejected"))?;

        let mut state = DashboardState::default();
        state.battery = BatteryTelemetry {
            health: Health::Unknown,
            millivolts: 0,
            percent: 0,
            raw: 0,
        };
        state.wifi.health = Health::Stale;
        state.wifi.ssid = WIFI_SSID.into();
        state.record(0, "BOOT", "Touch349 V2 display initialized");

        // The first visible frame is useful UI; keep the backlight off until it is complete.
        render_touch349_dashboard(&mut framebuffer, &state)
            .expect("Touch349 framebuffer is infallible");
        flush(&mut state)?;
        if set_backlight(state.display_brightness) != 0 {
            return Err(anyhow!("Touch349 backlight enable failed"));
        }

        let system_loop = EspSystemEventLoop::take().context("system event loop")?;
        let nvs = EspDefaultNvsPartition::take().context("default NVS")?;
        let mut wifi = BlockingWifi::wrap(
            EspWifi::new(peripherals.modem, system_loop.clone(), Some(nvs))?,
            system_loop,
        )?;
        wifi.set_configuration(&Configuration::Client(ClientConfiguration {
            ssid: WIFI_SSID
                .try_into()
                .map_err(|_| anyhow!("Wi-Fi SSID too long"))?,
            password: WIFI_PASSWORD
                .try_into()
                .map_err(|_| anyhow!("Wi-Fi password too long"))?,
            ..Default::default()
        }))?;
        wifi.start()?;
        if unsafe { hmi_touch349_time_init(TIMEZONE.as_ptr().cast()) } != 0 {
            warn!("SNTP initialization failed");
        }
        if let Err(error) = wifi.connect().and_then(|_| wifi.wait_netif_up()) {
            warn!("initial Wi-Fi connection failed: {error}");
            state.wifi.health = Health::Error;
        } else {
            update_wifi(&wifi, &mut state);
            state.record(0, "WIFI", "station connected");
        }

        render_touch349_dashboard(&mut framebuffer, &state)
            .expect("Touch349 framebuffer is infallible");
        flush(&mut state)?;

        let boot = Instant::now();
        let mut last_render = 0u64;
        let mut last_clock = 0u64;
        let mut last_wifi_retry = 0u64;
        let mut last_pressed = false;
        loop {
            let now_ms = boot.elapsed().as_millis() as u64;
            state.runtime.uptime_ms = now_ms;
            state.runtime.free_heap = unsafe { esp_idf_sys::esp_get_free_heap_size() };
            state.runtime.free_psram =
                unsafe { esp_idf_sys::heap_caps_get_free_size(esp_idf_sys::MALLOC_CAP_SPIRAM) }
                    as u32;
            update_wifi(&wifi, &mut state);
            if state.wifi.health != Health::Ok
                && (last_wifi_retry == 0 || now_ms.saturating_sub(last_wifi_retry) >= 10_000)
            {
                last_wifi_retry = now_ms;
                if let Err(error) = wifi.connect() {
                    warn!("Wi-Fi reconnect failed: {error}");
                    state.wifi.health = Health::Error;
                }
            }
            if last_clock == 0 || now_ms.saturating_sub(last_clock) >= 1_000 {
                sample_clock(&mut state, now_ms);
                last_clock = now_ms;
            }

            let mut x = 0u16;
            let mut y = 0u16;
            let mut pressed = false;
            if unsafe { hmi_touch349_touch_read(&mut x, &mut y, &mut pressed) } == 0 {
                if pressed && !last_pressed {
                    if state.apply_touch349(x, y, now_ms) {
                        let result = set_backlight(state.display_brightness);
                        if result != 0 {
                            warn!("backlight update failed: {result}");
                        }
                    }
                    last_render = 0;
                }
                last_pressed = pressed;
            }

            if let Some(action) = state.take_action() {
                apply_action(action, &mut state, now_ms);
                last_render = 0;
            }

            let frame_period_ms = 1_000 / u64::from(state.display_fps.max(1));
            if last_render == 0 || now_ms.saturating_sub(last_render) >= frame_period_ms {
                render_touch349_dashboard(&mut framebuffer, &state)
                    .expect("Touch349 framebuffer is infallible");
                flush(&mut state)?;
                last_render = now_ms;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn set_backlight(percent: u8) -> i32 {
        let duty = (u16::from(percent.min(100)) * 255 / 100) as u8;
        unsafe { hmi_touch349_backlight_set(duty, true) }
    }

    fn apply_action(action: UiAction, state: &mut DashboardState, now_ms: u64) {
        match action {
            UiAction::ToggleRecording => {
                state.recording = !state.recording;
                state.record(
                    now_ms,
                    "REC",
                    if state.recording {
                        "started"
                    } else {
                        "stopped"
                    },
                );
            }
            UiAction::TogglePlayback => state.playing = !state.playing,
            UiAction::StopPlayback => state.playing = false,
            UiAction::ViewerTop => state.viewer_offset = 0,
            UiAction::ViewerNext => state.viewer_offset = state.viewer_next_offset,
            UiAction::ToggleEventLog => state.event_logging_enabled = !state.event_logging_enabled,
            UiAction::CycleVolume => {
                state.speaker_volume = (state.speaker_volume.saturating_add(15)).min(100)
            }
            UiAction::RefreshDisplay => state.refresh_requested = true,
            UiAction::RefreshFiles
            | UiAction::OpenLastRecording
            | UiAction::OpenSelectedFile
            | UiAction::SpeakerTest
            | UiAction::PreparePoweroff => {}
        }
    }

    fn sample_clock(state: &mut DashboardState, now_ms: u64) {
        let mut year = 0i32;
        let mut month = 0u8;
        let mut day = 0u8;
        let mut weekday = 0u8;
        let mut hour = 0u8;
        let mut minute = 0u8;
        let mut second = 0u8;
        let result = unsafe {
            hmi_touch349_time_read(
                &mut year,
                &mut month,
                &mut day,
                &mut weekday,
                &mut hour,
                &mut minute,
                &mut second,
            )
        };
        if result == 0 {
            let first_sync = state.clock.health != Health::Ok;
            state.clock = ClockTelemetry {
                health: Health::Ok,
                zone: "IST",
                year: year.clamp(0, u16::MAX as i32) as u16,
                month,
                day,
                weekday,
                hour,
                minute,
                second,
            };
            if first_sync {
                state.record(now_ms, "TIME", "network time synchronized");
            }
        } else if state.clock.health == Health::Unknown {
            state.clock.health = Health::Stale;
        }
    }

    fn flush(state: &mut DashboardState) -> anyhow::Result<()> {
        let mut stats = hmi_touch349_flush_stats_t {
            flush_us: 0,
            dma_wait_us: 0,
            bands: 0,
            failures: 0,
        };
        let result = unsafe { hmi_touch349_flush_full(&mut stats) };
        if result != 0 {
            return Err(anyhow!("Touch349 flush failed: {result}"));
        }
        state.runtime.display_flush_ms = (stats.flush_us / 1000).min(u16::MAX as u32) as u16;
        log::debug!(
            "Touch349 flush: {} us, DMA wait {} us, {} bands, {} failures",
            stats.flush_us,
            stats.dma_wait_us,
            stats.bands,
            stats.failures
        );
        Ok(())
    }

    fn update_wifi(wifi: &BlockingWifi<EspWifi<'_>>, state: &mut DashboardState) {
        if wifi.is_connected().unwrap_or(false) {
            state.wifi.health = Health::Ok;
            state.wifi.ssid = WIFI_SSID.into();
            if let Ok(info) = wifi.wifi().sta_netif().get_ip_info() {
                state.wifi.ipv4 = info.ip.to_string();
            }
            if let Ok(rssi) = wifi.wifi().get_rssi() {
                state.wifi.rssi_dbm = rssi.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            }
        } else {
            state.wifi.health = Health::Stale;
        }
    }
}
