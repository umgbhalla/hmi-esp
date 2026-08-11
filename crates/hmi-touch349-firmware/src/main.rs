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
        render_touch349_dashboard, render_touch349_test_pattern, BatteryTelemetry, DashboardState,
        Health, Touch349FrameBuffer, View, TOUCH349_PIXELS,
    };
    use log::{info, warn};

    use esp_idf_sys::hmi_touch349::{
        hmi_touch349_backlight_set, hmi_touch349_flush_full, hmi_touch349_flush_stats_t,
        hmi_touch349_framebuffer, hmi_touch349_init, hmi_touch349_touch_read,
    };

    const WIFI_SSID: &str = env!("HMI_WIFI_SSID", "set WIFI_SSID in .env.local");
    const WIFI_PASSWORD: &str = env!("HMI_WIFI_PASSWORD", "set WIFI_PASSWORD in .env.local");

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
        state.record(0, "BOOT", "Touch349 V2 display initialized");

        // Prove the panel path before any network operation can block startup.
        render_touch349_test_pattern(&mut framebuffer).expect("Touch349 framebuffer is infallible");
        flush(&mut state)?;
        if unsafe { hmi_touch349_backlight_set(64, true) } != 0 {
            return Err(anyhow!("Touch349 backlight enable failed"));
        }
        thread::sleep(Duration::from_secs(2));

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
        let mut last_pressed = false;
        loop {
            let now_ms = boot.elapsed().as_millis() as u64;
            state.runtime.uptime_ms = now_ms;
            state.runtime.free_heap = unsafe { esp_idf_sys::esp_get_free_heap_size() };
            state.runtime.free_psram =
                unsafe { esp_idf_sys::heap_caps_get_free_size(esp_idf_sys::MALLOC_CAP_SPIRAM) }
                    as u32;
            update_wifi(&wifi, &mut state);

            let mut x = 0u16;
            let mut y = 0u16;
            let mut pressed = false;
            if unsafe { hmi_touch349_touch_read(&mut x, &mut y, &mut pressed) } == 0 {
                if pressed && !last_pressed {
                    state.view = if y >= 568 {
                        if x < 57 {
                            View::Home
                        } else if x < 114 {
                            View::Menu
                        } else {
                            View::Home
                        }
                    } else if state.view == View::Home {
                        View::Menu
                    } else {
                        state.view
                    };
                    state.record(now_ms, "TOUCH", format!("x={x} y={y}"));
                    last_render = 0;
                }
                last_pressed = pressed;
            }

            if last_render == 0 || now_ms.saturating_sub(last_render) >= 250 {
                render_touch349_dashboard(&mut framebuffer, &state)
                    .expect("Touch349 framebuffer is infallible");
                flush(&mut state)?;
                last_render = now_ms;
            }
            thread::sleep(Duration::from_millis(10));
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
        info!(
            "Touch349 flush: {} us, DMA wait {} us, {} bands, {} failures",
            stats.flush_us, stats.dma_wait_us, stats.bands, stats.failures
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
