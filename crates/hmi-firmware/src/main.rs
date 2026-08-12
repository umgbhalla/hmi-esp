#[cfg(not(target_os = "espidf"))]
fn main() {
    println!(
        "hmi-firmware is an ESP32-S3 target; run `cargo run -p hmi-simulator` for host UI output"
    );
}

#[cfg(target_os = "espidf")]
fn main() -> anyhow::Result<()> {
    firmware::run()
}

#[cfg(target_os = "espidf")]
mod firmware {
    use std::{
        ffi::CString,
        fmt::Write as _,
        fs::{self, File, OpenOptions},
        io::{Read, Seek, SeekFrom, Write},
        path::{Path, PathBuf},
        string::String,
        thread,
        time::{Duration, Instant},
        vec::Vec,
    };

    use anyhow::{anyhow, Context};
    use embedded_svc::wifi::{ClientConfiguration, Configuration, Wifi};
    use esp_idf_hal::{
        delay::Ets,
        gpio::{PinDriver, Pull},
        peripherals::Peripherals,
        spi::{config::Config as SpiConfig, Dma, SpiDeviceDriver, SpiDriverConfig},
        units::FromValueType,
    };
    use esp_idf_svc::{
        eventloop::EspSystemEventLoop,
        log::EspLogger,
        nvs::EspDefaultNvsPartition,
        wifi::{BlockingWifi, EspWifi},
    };
    use hmi_core::{
        render_dashboard, BatteryTelemetry, Button, ButtonEngine, ClockTelemetry, DashboardState,
        EnvironmentTelemetry, FileEntry, FileKind, Health, UiAction, View,
    };
    use log::{error, info, warn};
    use st7305::{FrameBuffer, St7305};

    use esp_idf_sys::hmi_board::{
        hmi_board_audio_read, hmi_board_audio_set_volume, hmi_board_audio_write,
        hmi_board_battery_read, hmi_board_env_read, hmi_board_init, hmi_board_sd_stats,
        hmi_board_te_init, hmi_board_te_take_rising_edge, HMI_BOARD_AUDIO_READY,
        HMI_BOARD_BATTERY_READY, HMI_BOARD_ENV_READY, HMI_BOARD_SD_READY,
    };

    unsafe extern "C" {
        fn hmi_board_sd_append(data: *const u8, data_len: usize) -> i32;
        fn hmi_board_time_init(timezone: *const core::ffi::c_char) -> i32;
        fn hmi_board_time_read(
            year: *mut i32,
            month: *mut u8,
            day: *mut u8,
            weekday: *mut u8,
            hour: *mut u8,
            minute: *mut u8,
            second: *mut u8,
        ) -> i32;
    }

    const WIFI_SSID: &str = env!("HMI_WIFI_SSID", "set WIFI_SSID in the ignored .env.local");
    const WIFI_PASSWORD: &str = env!(
        "HMI_WIFI_PASSWORD",
        "set WIFI_PASSWORD in the ignored .env.local"
    );
    const TIMEZONE: &str = "IST-5:30";
    const TIMEZONE_LABEL: &str = "IST";
    // The codec chunk is 256 stereo frames (10.67 ms at 24 kHz). The vendor
    // full-duplex example services read/write continuously; a 20 ms sleep here
    // cut both capture and playback throughput roughly in half.
    const LOOP_MS: u64 = 1;
    const DISPLAY_MS: u64 = 250;
    const ENV_MS: u64 = 2_000;
    const AUDIO_EVENT_MS: u64 = 1_000;
    const AUDIO_HISTORY_MS: u64 = 100;
    const BATTERY_MS: u64 = 5_000;
    const STORAGE_MS: u64 = 5_000;
    const RUNTIME_EVENT_MS: u64 = 5_000;
    const WIFI_RETRY_MS: u64 = 15_000;
    const AUDIO_SAMPLE_COUNT: usize = 512;

    pub fn run() -> anyhow::Result<()> {
        esp_idf_sys::link_patches();
        EspLogger::initialize_default();
        info!("HMI firmware booting on ESP32-S3-RLCD-4.2");

        let peripherals = Peripherals::take().context("take ESP-IDF peripherals")?;
        let pins = peripherals.pins;

        let spi_config = SpiConfig::new().baudrate(24.MHz().into());
        let spi_driver_config = SpiDriverConfig::new().dma(Dma::Auto(16 * 1024));
        let spi = SpiDeviceDriver::new_single(
            peripherals.spi3,
            pins.gpio11,
            pins.gpio12,
            Option::<esp_idf_hal::gpio::AnyIOPin>::None,
            Some(pins.gpio40),
            &spi_driver_config,
            &spi_config,
        )?;
        let dc = PinDriver::output(pins.gpio5)?;
        let reset = PinDriver::output(pins.gpio41)?;
        let mut display = St7305::new(spi, dc, reset);
        display
            .init(&mut Ets)
            .map_err(|_| anyhow!("ST7305 initialization failed"))?;
        let mut framebuffer = FrameBuffer::new();
        display
            .flush(&framebuffer)
            .map_err(|_| anyhow!("ST7305 startup clear failed"))?;
        let mut state = DashboardState::default();
        state.record(0, "BOOT", "display and input ready");
        render_dashboard(&mut framebuffer, &state).expect("framebuffer is infallible");
        display
            .flush(&framebuffer)
            .map_err(|_| anyhow!("ST7305 startup dashboard failed"))?;
        display
            .display_on()
            .map_err(|_| anyhow!("ST7305 display enable failed"))?;
        info!("display cleared and startup dashboard visible");

        let boot_button = PinDriver::input(pins.gpio0, Pull::Up)?;
        let key_button = PinDriver::input(pins.gpio18, Pull::Up)?;
        let mut te_sync_enabled = unsafe { hmi_board_te_init() } == 0;
        if !te_sync_enabled {
            warn!("TE rising-edge interrupt unavailable; display will use timed fallback");
        }

        let board_ready = unsafe { hmi_board_init() };
        state.audio.health = health_from_bit(board_ready, HMI_BOARD_AUDIO_READY);
        state.environment.health = health_from_bit(board_ready, HMI_BOARD_ENV_READY);
        state.battery.health = health_from_bit(board_ready, HMI_BOARD_BATTERY_READY);
        state.storage.health = health_from_bit(board_ready, HMI_BOARD_SD_READY);
        state.storage.mounted = state.storage.health == Health::Ok;
        record_board_status(&mut state, board_ready);
        if board_ready & HMI_BOARD_ENV_READY != 0 {
            sample_environment(&mut state, 0);
        }

        let mut wifi = match start_wifi(peripherals.modem) {
            Ok(wifi) => {
                state.wifi.health = Health::Stale;
                state.wifi.ssid = WIFI_SSID.into();
                state.record(0, "WIFI", "station started; waiting for DHCP");
                Some(wifi)
            }
            Err(err) => {
                error!("Wi-Fi start failed: {err:#}");
                state.wifi.health = Health::Error;
                state.record(0, "WIFI", "station initialization failed");
                None
            }
        };
        let timezone = CString::new(TIMEZONE).expect("static timezone contains no NUL");
        if unsafe { hmi_board_time_init(timezone.as_ptr()) } != 0 {
            warn!("network time initialization failed");
        }

        let boot = Instant::now();
        let mut buttons = ButtonEngine::default();
        let mut event_recorder = EventRecorder::default();
        let mut media = MediaRuntime::default();
        let mut audio_samples = [0i16; AUDIO_SAMPLE_COUNT];
        let mut last_display = 0;
        let mut last_env_attempt = 0;
        let mut last_env_success = 0;
        let mut last_battery = 0;
        let mut last_storage = 0;
        let mut last_audio_event = 0;
        let mut last_audio_history = 0;
        let mut last_clock = 0;
        let mut last_runtime_event = 0;
        let mut last_wifi_retry = 0;
        let mut loop_counter = 0u32;
        let mut loop_window_ms = 0u64;
        let mut display_pending = false;
        let mut display_pending_since = 0u64;

        loop {
            let loop_started = Instant::now();
            let now_ms = boot.elapsed().as_millis() as u64;
            state.runtime.uptime_ms = now_ms;
            state.environment.sample_age_ms = now_ms.saturating_sub(last_env_success);

            sample_button(
                &mut buttons,
                &mut state,
                Button::Boot,
                boot_button.is_low(),
                now_ms,
            );
            sample_button(
                &mut buttons,
                &mut state,
                Button::Key,
                key_button.is_low(),
                now_ms,
            );
            for button in Button::ALL {
                let stats = &mut state.buttons[button.index()];
                stats.held_ms = buttons.held_ms(button, now_ms);
                stats.pressed = stats.held_ms > 0;
            }

            if let Some(action) = state.take_action() {
                if let Err(err) = media.handle(action, &mut state, now_ms) {
                    warn!("media action {action:?} failed: {err:#}");
                    state.record(now_ms, "MEDIA", format!("action failed: {action:?}"));
                }
            }

            if state.prepare_poweroff_requested {
                state.poweroff_prepared = false;
                state.record(now_ms, "POWER", "preparing files and event log");
                let media_ready = match media.stop_all(&mut state) {
                    Ok(()) => true,
                    Err(err) => {
                        warn!("power-off media finalization failed: {err:#}");
                        false
                    }
                };
                let log_ready = event_recorder.flush(&mut state);
                state.poweroff_prepared = media_ready && log_ready;
                state.prepare_poweroff_requested = false;
                if state.poweroff_prepared {
                    info!("power-off prepared; hold the hardware PWR button to switch off");
                } else {
                    warn!("power-off preparation incomplete; keep the device powered");
                }
            }

            if board_ready & HMI_BOARD_AUDIO_READY != 0 {
                let captured = sample_audio(&mut state, &mut audio_samples, now_ms);
                if captured && state.recording {
                    if let Err(err) = media.append_recording(&audio_samples, &mut state) {
                        warn!("WAV write failed: {err:#}");
                        state.record(now_ms, "REC", "write failed; recording stopped");
                        let _ = media.stop_recording(&mut state);
                    }
                }
                if state.audio.health == Health::Ok
                    && now_ms.saturating_sub(last_audio_history) >= AUDIO_HISTORY_MS
                {
                    last_audio_history = now_ms;
                    state.audio.push_level(state.audio.rms);
                }
                if state.audio.health == Health::Ok
                    && now_ms.saturating_sub(last_audio_event) >= AUDIO_EVENT_MS
                {
                    last_audio_event = now_ms;
                    state.record(
                        now_ms,
                        "MIC",
                        format!("rms={} peak={}", state.audio.rms, state.audio.peak),
                    );
                }
            }
            if state.playing {
                if let Err(err) = media.service_playback(&mut state) {
                    warn!("playback failed: {err:#}");
                    state.record(now_ms, "PLAY", "speaker write failed");
                    media.stop_playback(&mut state)?;
                }
            }
            if now_ms.saturating_sub(last_clock) >= 1_000 {
                last_clock = now_ms;
                sample_clock(&mut state, now_ms);
            }
            if now_ms.saturating_sub(last_env_attempt) >= ENV_MS {
                last_env_attempt = now_ms;
                if sample_environment(&mut state, now_ms) {
                    last_env_success = now_ms;
                }
            }
            if now_ms.saturating_sub(last_battery) >= BATTERY_MS {
                last_battery = now_ms;
                sample_battery(&mut state, now_ms);
            }
            if now_ms.saturating_sub(last_storage) >= STORAGE_MS {
                last_storage = now_ms;
                sample_storage(&mut state, now_ms);
            }
            update_wifi(&mut wifi, &mut state, now_ms, &mut last_wifi_retry);
            update_runtime(&mut state);
            if now_ms.saturating_sub(last_runtime_event) >= RUNTIME_EVENT_MS {
                last_runtime_event = now_ms;
                state.record(
                    now_ms,
                    "PIPE",
                    format!(
                        "heap={}K psram={}K loop={}Hz lcd={}ms",
                        state.runtime.free_heap / 1024,
                        state.runtime.free_psram / 1024,
                        state.runtime.loop_hz,
                        state.runtime.display_flush_ms
                    ),
                );
            }
            event_recorder.sync(&mut state);

            if !display_pending
                && (state.refresh_requested || now_ms.saturating_sub(last_display) >= DISPLAY_MS)
            {
                state.refresh_requested = false;
                render_dashboard(&mut framebuffer, &state).expect("framebuffer is infallible");
                display_pending = true;
                display_pending_since = now_ms;
                unsafe {
                    hmi_board_te_take_rising_edge();
                }
            }

            let te_edge = unsafe { hmi_board_te_take_rising_edge() };
            let te_timeout = display_pending
                && te_sync_enabled
                && now_ms.saturating_sub(display_pending_since) >= 100;
            if display_pending && (!te_sync_enabled || te_edge || te_timeout) {
                if te_timeout {
                    te_sync_enabled = false;
                    warn!("TE rising edge timed out; disabling synchronization fallback");
                    state.record(now_ms, "LCD", "TE timeout; using timed flushes");
                }
                let flush_started = Instant::now();
                if display.flush(&framebuffer).is_err() {
                    warn!("display flush failed");
                    state.record(now_ms, "LCD", "flush failed");
                }
                state.runtime.display_flush_ms =
                    flush_started.elapsed().as_millis().min(u16::MAX as u128) as u16;
                last_display = now_ms;
                display_pending = false;
            }

            loop_counter = loop_counter.saturating_add(1);
            if now_ms.saturating_sub(loop_window_ms) >= 1_000 {
                state.runtime.loop_hz = loop_counter.min(u16::MAX as u32) as u16;
                loop_counter = 0;
                loop_window_ms = now_ms;
            }
            let elapsed = loop_started.elapsed();
            if elapsed < Duration::from_millis(LOOP_MS) {
                thread::sleep(Duration::from_millis(LOOP_MS) - elapsed);
            }
        }
    }

    fn health_from_bit(mask: u32, bit: u32) -> Health {
        if mask & bit != 0 {
            Health::Ok
        } else {
            Health::Error
        }
    }

    fn record_board_status(state: &mut DashboardState, ready: u32) {
        for (bit, name) in [
            (HMI_BOARD_AUDIO_READY, "audio"),
            (HMI_BOARD_ENV_READY, "SHTC3"),
            (HMI_BOARD_BATTERY_READY, "battery ADC"),
            (HMI_BOARD_SD_READY, "SD recorder"),
        ] {
            let detail = if ready & bit != 0 {
                "ready"
            } else {
                "unavailable"
            };
            info!("board subsystem {name}: {detail}");
            state.record(0, "BOARD", format!("{name} {detail}"));
        }
    }

    fn sample_button(
        engine: &mut ButtonEngine,
        state: &mut DashboardState,
        button: Button,
        pressed: bool,
        now_ms: u64,
    ) {
        if let Some(event) = engine.sample(button, pressed, now_ms) {
            info!(
                "{} {:?} held={}ms",
                button.label(),
                event.gesture,
                event.held_ms
            );
            state.apply_input(event, now_ms);
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
            hmi_board_time_read(
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
                zone: TIMEZONE_LABEL,
                year: year.clamp(0, u16::MAX as i32) as u16,
                month,
                day,
                weekday,
                hour,
                minute,
                second,
            };
            if first_sync {
                info!(
                    "network time synchronized: {hour:02}:{minute:02}:{second:02} {TIMEZONE_LABEL}"
                );
                state.record(now_ms, "TIME", "network time synchronized");
            }
        } else if state.clock.health == Health::Unknown {
            state.clock.health = Health::Stale;
        }
    }

    fn sample_environment(state: &mut DashboardState, now_ms: u64) -> bool {
        let mut temperature = 0i32;
        let mut humidity = 0u32;
        let result = unsafe { hmi_board_env_read(&mut temperature, &mut humidity) };
        if result == 0 {
            let recovered = state.environment.health != Health::Ok;
            info!("environment calibrated sample: t={temperature}cC rh={humidity}cPct");
            state.environment = EnvironmentTelemetry {
                health: Health::Ok,
                temperature_centi_c: temperature,
                humidity_centi_pct: humidity,
                sample_age_ms: 0,
                crc_errors: state.environment.crc_errors,
            };
            if recovered {
                state.record(now_ms, "SHTC3", "sampling recovered");
            }
            state.record(
                now_ms,
                "SHTC3",
                format!("t={}cC rh={}cPct", temperature, humidity),
            );
            true
        } else {
            warn!("environment sample failed: ESP-IDF error {result}");
            if result == esp_idf_sys::ESP_ERR_INVALID_CRC as i32 {
                state.environment.crc_errors = state.environment.crc_errors.saturating_add(1);
            }
            if state.environment.health != Health::Error {
                state.record(now_ms, "SHTC3", "sample failed");
            }
            state.environment.health = Health::Error;
            false
        }
    }

    fn sample_battery(state: &mut DashboardState, now_ms: u64) {
        let mut millivolts = 0u32;
        let mut raw = 0u16;
        if unsafe { hmi_board_battery_read(&mut millivolts, &mut raw) } == 0 {
            let percent = battery_percent(millivolts);
            state.battery = BatteryTelemetry {
                health: Health::Ok,
                millivolts,
                percent,
                raw,
            };
            state.record(
                now_ms,
                "BAT",
                format!("mv={millivolts} pct={percent} raw={raw}"),
            );
        } else {
            if state.battery.health != Health::Error {
                state.record(now_ms, "BAT", "ADC read failed");
            }
            state.battery.health = Health::Error;
        }
    }

    fn battery_percent(millivolts: u32) -> u8 {
        if millivolts <= 3_000 {
            0
        } else if millivolts >= 4_120 {
            100
        } else {
            (((millivolts - 3_000) * 100) / 1_120) as u8
        }
    }

    fn sample_audio(state: &mut DashboardState, samples: &mut [i16], now_ms: u64) -> bool {
        let result = unsafe { hmi_board_audio_read(samples.as_mut_ptr(), samples.len()) };
        if result != 0 {
            if state.audio.health != Health::Error {
                state.record(now_ms, "MIC", "capture read failed");
            }
            state.audio.health = Health::Error;
            return false;
        }
        let mut squares = 0u64;
        let mut peak = 0u16;
        for sample in samples.iter().step_by(2) {
            let magnitude = sample.unsigned_abs();
            peak = peak.max(magnitude);
            squares = squares.saturating_add((magnitude as u64) * (magnitude as u64));
        }
        let mono_frames = (samples.len() / 2) as u64;
        let mean = if mono_frames == 0 {
            0
        } else {
            squares / mono_frames
        };
        state.audio.health = Health::Ok;
        state.audio.rms = integer_sqrt(mean).min(u16::MAX as u64) as u16;
        state.audio.peak = peak;
        state.audio.frames_total = state.audio.frames_total.saturating_add(mono_frames);
        state.audio.buffer_capacity_frames = mono_frames as u32;
        state.audio.buffered_frames = mono_frames as u32;
        true
    }

    fn integer_sqrt(value: u64) -> u64 {
        if value < 2 {
            return value;
        }
        let mut x = value;
        let mut y = (x + value / x) / 2;
        while y < x {
            x = y;
            y = (x + value / x) / 2;
        }
        x
    }

    fn sample_storage(state: &mut DashboardState, now_ms: u64) {
        let mut total = 0u64;
        let mut free = 0u64;
        if unsafe { hmi_board_sd_stats(&mut total, &mut free) } == 0 {
            state.storage.health = Health::Ok;
            state.storage.mounted = true;
            state.storage.total_kib = total;
            state.storage.free_kib = free;
            state.record(now_ms, "SD", format!("free={}KiB total={}KiB", free, total));
        } else {
            if state.storage.health != Health::Error {
                state.record(now_ms, "SD", "card unavailable");
            }
            state.storage.health = Health::Error;
            state.storage.mounted = false;
        }
    }

    fn update_runtime(state: &mut DashboardState) {
        unsafe {
            state.runtime.free_heap = esp_idf_sys::esp_get_free_heap_size();
            state.runtime.min_free_heap = esp_idf_sys::esp_get_minimum_free_heap_size();
            state.runtime.free_psram =
                esp_idf_sys::heap_caps_get_free_size(esp_idf_sys::MALLOC_CAP_SPIRAM as u32) as u32;
        }
    }

    fn start_wifi<'d>(
        modem: esp_idf_hal::modem::Modem<'d>,
    ) -> anyhow::Result<BlockingWifi<EspWifi<'d>>> {
        if WIFI_SSID.is_empty() || WIFI_PASSWORD.is_empty() {
            return Err(anyhow!("Wi-Fi credentials are empty"));
        }
        let sys_loop = EspSystemEventLoop::take()?;
        let nvs = EspDefaultNvsPartition::take()?;
        let mut wifi =
            BlockingWifi::wrap(EspWifi::new(modem, sys_loop.clone(), Some(nvs))?, sys_loop)?;
        wifi.set_configuration(&Configuration::Client(ClientConfiguration {
            ssid: WIFI_SSID
                .try_into()
                .map_err(|_| anyhow!("SSID is too long"))?,
            password: WIFI_PASSWORD
                .try_into()
                .map_err(|_| anyhow!("Wi-Fi password is too long"))?,
            ..Default::default()
        }))?;
        wifi.start()?;
        wifi.connect()?;
        wifi.wait_netif_up()?;
        Ok(wifi)
    }

    fn update_wifi(
        wifi: &mut Option<BlockingWifi<EspWifi<'_>>>,
        state: &mut DashboardState,
        now_ms: u64,
        last_retry: &mut u64,
    ) {
        let Some(wifi) = wifi.as_mut() else { return };
        if wifi.is_connected().unwrap_or(false) {
            let recovered = state.wifi.health != Health::Ok;
            state.wifi.health = Health::Ok;
            if let Ok(info) = wifi.wifi().sta_netif().get_ip_info() {
                state.wifi.ipv4 = info.ip.to_string();
            }
            if let Ok(rssi) = wifi.wifi().get_rssi() {
                state.wifi.rssi_dbm = rssi.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            }
            if recovered {
                state.record(now_ms, "WIFI", "connected and address ready");
            }
        } else {
            if state.wifi.health == Health::Ok {
                state.wifi.health = Health::Stale;
                state.record(now_ms, "WIFI", "connection lost; retaining last address");
            }
            if now_ms.saturating_sub(*last_retry) >= WIFI_RETRY_MS {
                *last_retry = now_ms;
                state.wifi.reconnects = state.wifi.reconnects.saturating_add(1);
                if let Err(err) = wifi.connect() {
                    warn!("Wi-Fi reconnect failed: {err}");
                }
            }
        }
    }

    const SD_ROOT: &str = "/sdcard";
    const WAV_CHANNELS: u16 = 2;
    const WAV_RATE: u32 = 24_000;
    const WAV_BITS: u16 = 16;
    const WAV_BYTES_PER_SECOND: u64 = WAV_RATE as u64 * WAV_CHANNELS as u64 * 2;

    #[derive(Default)]
    struct MediaRuntime {
        recorder: Option<WavRecorder>,
        player: Option<WavPlayer>,
    }

    impl MediaRuntime {
        fn handle(
            &mut self,
            action: UiAction,
            state: &mut DashboardState,
            now_ms: u64,
        ) -> anyhow::Result<()> {
            match action {
                UiAction::RefreshFiles => refresh_files(state)?,
                UiAction::ToggleRecording => {
                    if state.recording {
                        self.stop_recording(state)?;
                        refresh_files(state)?;
                    } else {
                        self.start_recording(state, now_ms)?;
                    }
                }
                UiAction::OpenLastRecording => {
                    if !state.last_recording.is_empty() {
                        let name = state.last_recording.clone();
                        self.open_player(&name, state)?;
                    }
                }
                UiAction::ToggleLastRecording => {
                    if state.playing {
                        self.stop_playback(state)?;
                    } else if !state.last_recording.is_empty() {
                        let name = state.last_recording.clone();
                        self.open_player(&name, state)?;
                        state.playing = true;
                    }
                }
                UiAction::OpenSelectedFile => {
                    if let Some(entry) = state.selected_file().cloned() {
                        match entry.kind {
                            FileKind::Wav => self.open_player(&entry.name, state)?,
                            FileKind::Text | FileKind::Other => {
                                self.stop_playback(state)?;
                                open_viewer(&entry, 0, state)?;
                            }
                        }
                    }
                }
                UiAction::TogglePlayback => {
                    if self.player.is_some() {
                        state.playing = !state.playing;
                    }
                }
                UiAction::StopPlayback => self.stop_playback(state)?,
                UiAction::ViewerNext => {
                    if !state.viewer_name.is_empty() {
                        let entry = FileEntry {
                            name: state.viewer_name.clone(),
                            size: state.viewer_size,
                            kind: file_kind(&state.viewer_name),
                        };
                        let next = if state.viewer_next_offset >= state.viewer_size {
                            0
                        } else {
                            state.viewer_next_offset
                        };
                        open_viewer(&entry, next, state)?;
                    }
                }
                UiAction::ViewerTop => {
                    if !state.viewer_name.is_empty() {
                        let entry = FileEntry {
                            name: state.viewer_name.clone(),
                            size: state.viewer_size,
                            kind: file_kind(&state.viewer_name),
                        };
                        open_viewer(&entry, 0, state)?;
                    }
                }
                UiAction::RefreshDisplay => state.refresh_requested = true,
                UiAction::CycleVolume => {
                    let next_volume = match state.speaker_volume {
                        0..=25 => 40,
                        26..=40 => 55,
                        41..=55 => 70,
                        56..=70 => 85,
                        71..=85 => 100,
                        _ => 25,
                    };
                    let result = unsafe { hmi_board_audio_set_volume(next_volume) };
                    if result != 0 {
                        return Err(anyhow!("speaker volume failed: {result}"));
                    }
                    state.speaker_volume = next_volume;
                }
                UiAction::ToggleEventLog => {
                    state.event_logging_enabled = !state.event_logging_enabled;
                    state.record(
                        now_ms,
                        "LOG",
                        if state.event_logging_enabled {
                            "SD event log enabled"
                        } else {
                            "SD event log disabled"
                        },
                    );
                }
                UiAction::SpeakerTest => self.speaker_test(state, now_ms)?,
                UiAction::PreparePoweroff => {
                    state.prepare_poweroff_requested = true;
                }
            }
            state.refresh_requested = true;
            Ok(())
        }

        fn start_recording(
            &mut self,
            state: &mut DashboardState,
            now_ms: u64,
        ) -> anyhow::Result<()> {
            if state.audio.health != Health::Ok {
                return Err(anyhow!("microphone is not ready"));
            }
            if !state.storage.mounted {
                return Err(anyhow!("SD card is not mounted"));
            }
            state.poweroff_prepared = false;
            self.stop_playback(state)?;
            let recorder = WavRecorder::create()?;
            state.recording_name = recorder.name.clone();
            state.last_recording = recorder.name.clone();
            state.recording_bytes = 0;
            state.recording_started_ms = now_ms;
            state.recording = true;
            state.record(now_ms, "REC", format!("started {}", recorder.name));
            self.recorder = Some(recorder);
            Ok(())
        }

        fn append_recording(
            &mut self,
            samples: &[i16],
            state: &mut DashboardState,
        ) -> anyhow::Result<()> {
            let Some(recorder) = self.recorder.as_mut() else {
                state.recording = false;
                return Err(anyhow!("recording state has no open WAV file"));
            };
            recorder.append(samples)?;
            state.recording_bytes = recorder.data_bytes;
            Ok(())
        }

        fn stop_recording(&mut self, state: &mut DashboardState) -> anyhow::Result<()> {
            let result = if let Some(recorder) = self.recorder.take() {
                let name = recorder.name.clone();
                recorder.finish().map(|bytes| {
                    state.recording_bytes = bytes;
                    state.last_recording = name;
                })
            } else {
                Ok(())
            };
            state.recording = false;
            result
        }

        fn open_player(&mut self, name: &str, state: &mut DashboardState) -> anyhow::Result<()> {
            self.stop_recording(state)?;
            let player = WavPlayer::open(name)?;
            state.playback_name = name.into();
            state.playback_position_ms = 0;
            state.playback_duration_ms = player.duration_ms();
            state.playback_audio = Default::default();
            state.playing = false;
            state.view = View::Player;
            self.player = Some(player);
            Ok(())
        }

        fn service_playback(&mut self, state: &mut DashboardState) -> anyhow::Result<()> {
            let Some(player) = self.player.as_mut() else {
                state.playing = false;
                return Ok(());
            };
            let mut samples = [0i16; AUDIO_SAMPLE_COUNT];
            let count = player.read_samples(&mut samples)?;
            if count == 0 {
                state.playing = false;
                state.playback_position_ms = 0;
                player.rewind()?;
                return Ok(());
            }
            let result = unsafe { hmi_board_audio_write(samples.as_ptr(), count) };
            if result != 0 {
                return Err(anyhow!("speaker write failed: {result}"));
            }
            state.playback_position_ms = player.position_ms();
            let (rms, peak) = audio_levels(&samples[..count]);
            state.playback_audio.health = Health::Ok;
            state.playback_audio.rms = rms;
            state.playback_audio.peak = peak;
            state.playback_audio.push_level(rms);
            Ok(())
        }

        fn stop_playback(&mut self, state: &mut DashboardState) -> anyhow::Result<()> {
            self.player = None;
            state.playing = false;
            state.playback_position_ms = 0;
            Ok(())
        }

        fn stop_all(&mut self, state: &mut DashboardState) -> anyhow::Result<()> {
            self.stop_recording(state)?;
            self.stop_playback(state)
        }

        fn speaker_test(&mut self, state: &mut DashboardState, now_ms: u64) -> anyhow::Result<()> {
            if state.recording {
                return Err(anyhow!("stop recording before testing the speaker"));
            }
            self.stop_playback(state)?;
            let mut phase = 0u32;
            let phase_step = ((440u64 << 32) / WAV_RATE as u64) as u32;
            let mut samples = [0i16; AUDIO_SAMPLE_COUNT];
            for _ in 0..24 {
                for frame in 0..(AUDIO_SAMPLE_COUNT / 2) {
                    let position = (phase >> 16) as i32;
                    let triangle = if position < 32_768 {
                        position * 2 - 32_768
                    } else {
                        98_303 - position * 2
                    };
                    let sample = (triangle * 8_000 / 32_768) as i16;
                    samples[frame * 2] = sample;
                    samples[frame * 2 + 1] = sample;
                    phase = phase.wrapping_add(phase_step);
                }
                let result = unsafe { hmi_board_audio_write(samples.as_ptr(), samples.len()) };
                if result != 0 {
                    return Err(anyhow!("speaker test write failed: {result}"));
                }
            }
            info!(
                "speaker test tone completed at volume {}%",
                state.speaker_volume
            );
            state.record(now_ms, "SPK", "440 Hz test tone completed");
            Ok(())
        }
    }

    struct WavRecorder {
        file: File,
        name: String,
        data_bytes: u64,
        checkpoint_bytes: u64,
    }

    impl WavRecorder {
        fn create() -> anyhow::Result<Self> {
            for index in 1..=999_999u32 {
                let name = format!("R{index:06}.WAV");
                let path = Path::new(SD_ROOT).join(&name);
                match OpenOptions::new().write(true).create_new(true).open(&path) {
                    Ok(mut file) => {
                        write_wav_header(&mut file, 0)?;
                        return Ok(Self {
                            file,
                            name,
                            data_bytes: 0,
                            checkpoint_bytes: 0,
                        });
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(err) => {
                        return Err(err).with_context(|| format!("create {}", path.display()))
                    }
                }
            }
            Err(anyhow!("recording filename space exhausted"))
        }

        fn append(&mut self, samples: &[i16]) -> anyhow::Result<()> {
            let mut bytes = [0u8; AUDIO_SAMPLE_COUNT * 2];
            let count = samples.len().min(AUDIO_SAMPLE_COUNT);
            let appended_bytes = (count * 2) as u64;
            let wav_data_limit = (u32::MAX - 36) as u64;
            if self.data_bytes.saturating_add(appended_bytes) > wav_data_limit {
                return Err(anyhow!("WAV reached the RIFF size limit"));
            }
            for (index, sample) in samples[..count].iter().enumerate() {
                bytes[index * 2..index * 2 + 2].copy_from_slice(&sample.to_le_bytes());
            }
            if let Err(err) = self.file.write_all(&bytes[..count * 2]) {
                let committed_len = 44u64.saturating_add(self.data_bytes);
                let _ = self.file.set_len(committed_len);
                let _ = self.file.seek(SeekFrom::End(0));
                return Err(err).context("append WAV samples");
            }
            self.data_bytes = self.data_bytes.saturating_add(appended_bytes);
            if self.data_bytes.saturating_sub(self.checkpoint_bytes) >= WAV_BYTES_PER_SECOND {
                write_wav_header(&mut self.file, self.data_bytes.min(u32::MAX as u64) as u32)?;
                self.file.flush()?;
                self.checkpoint_bytes = self.data_bytes;
            }
            Ok(())
        }

        fn finish(mut self) -> anyhow::Result<u64> {
            write_wav_header(&mut self.file, self.data_bytes.min(u32::MAX as u64) as u32)?;
            self.file.flush()?;
            self.file.sync_all()?;
            Ok(self.data_bytes)
        }
    }

    fn write_wav_header(file: &mut File, data_bytes: u32) -> anyhow::Result<()> {
        let byte_rate = WAV_RATE * WAV_CHANNELS as u32 * (WAV_BITS as u32 / 8);
        let block_align = WAV_CHANNELS * (WAV_BITS / 8);
        let mut header = [0u8; 44];
        header[0..4].copy_from_slice(b"RIFF");
        header[4..8].copy_from_slice(&(36u32.saturating_add(data_bytes)).to_le_bytes());
        header[8..12].copy_from_slice(b"WAVE");
        header[12..16].copy_from_slice(b"fmt ");
        header[16..20].copy_from_slice(&16u32.to_le_bytes());
        header[20..22].copy_from_slice(&1u16.to_le_bytes());
        header[22..24].copy_from_slice(&WAV_CHANNELS.to_le_bytes());
        header[24..28].copy_from_slice(&WAV_RATE.to_le_bytes());
        header[28..32].copy_from_slice(&byte_rate.to_le_bytes());
        header[32..34].copy_from_slice(&block_align.to_le_bytes());
        header[34..36].copy_from_slice(&WAV_BITS.to_le_bytes());
        header[36..40].copy_from_slice(b"data");
        header[40..44].copy_from_slice(&data_bytes.to_le_bytes());
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header)?;
        file.seek(SeekFrom::End(0))?;
        Ok(())
    }

    struct WavPlayer {
        file: File,
        data_start: u64,
        data_len: u64,
        position: u64,
    }

    impl WavPlayer {
        fn open(name: &str) -> anyhow::Result<Self> {
            let path = safe_sd_path(name)?;
            let mut file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
            let file_len = file.metadata()?.len();
            let mut riff = [0u8; 12];
            file.read_exact(&mut riff)?;
            if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
                return Err(anyhow!("not a RIFF/WAVE file"));
            }
            let riff_end = 8u64
                .checked_add(u32::from_le_bytes(riff[4..8].try_into().unwrap()) as u64)
                .ok_or_else(|| anyhow!("invalid RIFF length"))?;
            if riff_end < 12 || riff_end > file_len {
                return Err(anyhow!("RIFF length exceeds the file"));
            }
            let mut format_ok = false;
            let mut data = None;
            loop {
                if file.stream_position()?.saturating_add(8) > riff_end {
                    break;
                }
                let mut chunk = [0u8; 8];
                file.read_exact(&mut chunk)?;
                let len = u32::from_le_bytes(chunk[4..8].try_into().unwrap()) as u64;
                let start = file.stream_position()?;
                let padded_end = start
                    .checked_add(len)
                    .and_then(|end| end.checked_add(len & 1))
                    .ok_or_else(|| anyhow!("WAV chunk length overflow"))?;
                if padded_end > riff_end {
                    return Err(anyhow!("WAV chunk exceeds RIFF bounds"));
                }
                if &chunk[0..4] == b"fmt " && len >= 16 {
                    let mut fmt = [0u8; 16];
                    file.read_exact(&mut fmt)?;
                    format_ok = u16::from_le_bytes(fmt[0..2].try_into().unwrap()) == 1
                        && u16::from_le_bytes(fmt[2..4].try_into().unwrap()) == WAV_CHANNELS
                        && u32::from_le_bytes(fmt[4..8].try_into().unwrap()) == WAV_RATE
                        && u16::from_le_bytes(fmt[12..14].try_into().unwrap())
                            == WAV_CHANNELS * (WAV_BITS / 8)
                        && u16::from_le_bytes(fmt[14..16].try_into().unwrap()) == WAV_BITS;
                } else if &chunk[0..4] == b"data" {
                    if len % (WAV_CHANNELS as u64 * (WAV_BITS as u64 / 8)) != 0 {
                        return Err(anyhow!("WAV data is not frame-aligned"));
                    }
                    data = Some((start, len));
                    break;
                }
                file.seek(SeekFrom::Start(padded_end))?;
            }
            if !format_ok {
                return Err(anyhow!("player requires PCM stereo 24 kHz 16-bit WAV"));
            }
            let (data_start, data_len) = data.ok_or_else(|| anyhow!("WAV data chunk missing"))?;
            file.seek(SeekFrom::Start(data_start))?;
            Ok(Self {
                file,
                data_start,
                data_len,
                position: 0,
            })
        }

        fn read_samples(&mut self, output: &mut [i16]) -> anyhow::Result<usize> {
            let remaining = self.data_len.saturating_sub(self.position) as usize;
            let bytes_wanted = remaining.min(output.len() * 2);
            if bytes_wanted < 2 {
                return Ok(0);
            }
            let mut bytes = [0u8; AUDIO_SAMPLE_COUNT * 2];
            self.file.read_exact(&mut bytes[..bytes_wanted])?;
            let read = bytes_wanted;
            for index in 0..read / 2 {
                output[index] = i16::from_le_bytes([bytes[index * 2], bytes[index * 2 + 1]]);
            }
            self.position = self.position.saturating_add(read as u64);
            Ok(read / 2)
        }

        fn duration_ms(&self) -> u64 {
            self.data_len.saturating_mul(1000) / WAV_BYTES_PER_SECOND
        }
        fn position_ms(&self) -> u64 {
            self.position.saturating_mul(1000) / WAV_BYTES_PER_SECOND
        }
        fn rewind(&mut self) -> anyhow::Result<()> {
            self.file.seek(SeekFrom::Start(self.data_start))?;
            self.position = 0;
            Ok(())
        }
    }

    fn refresh_files(state: &mut DashboardState) -> anyhow::Result<()> {
        let mut files = Vec::new();
        collect_files(Path::new(SD_ROOT), Path::new(""), &mut files, 0)?;
        files.sort_by(|left, right| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
        });
        state.files = files;
        state.file_index = state.file_index.min(state.files.len().saturating_sub(1));
        Ok(())
    }

    fn collect_files(
        base: &Path,
        relative: &Path,
        output: &mut Vec<FileEntry>,
        depth: u8,
    ) -> anyhow::Result<()> {
        if depth > 4 || output.len() >= 128 {
            return Ok(());
        }
        for entry in fs::read_dir(base.join(relative))? {
            let entry = entry?;
            let next = relative.join(entry.file_name());
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                collect_files(base, &next, output, depth + 1)?;
            } else if metadata.is_file() {
                let name = next.to_string_lossy().into_owned();
                output.push(FileEntry {
                    kind: file_kind(&name),
                    name,
                    size: metadata.len(),
                });
                if output.len() >= 128 {
                    break;
                }
            }
        }
        Ok(())
    }

    fn file_kind(name: &str) -> FileKind {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".wav") {
            FileKind::Wav
        } else if lower.ends_with(".txt")
            || lower.ends_with(".log")
            || lower.ends_with(".json")
            || lower.ends_with(".ndj")
            || lower.ends_with(".csv")
            || lower.ends_with(".md")
        {
            FileKind::Text
        } else {
            FileKind::Other
        }
    }

    fn safe_sd_path(name: &str) -> anyhow::Result<PathBuf> {
        let relative = Path::new(name);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(anyhow!("unsafe SD path"));
        }
        Ok(Path::new(SD_ROOT).join(relative))
    }

    fn open_viewer(
        entry: &FileEntry,
        offset: u64,
        state: &mut DashboardState,
    ) -> anyhow::Result<()> {
        let path = safe_sd_path(&entry.name)?;
        let mut file = File::open(&path)?;
        let actual_size = file.metadata()?.len();
        let aligned = offset.min(actual_size);
        file.seek(SeekFrom::Start(aligned))?;
        let mut bytes = [0u8; 1024];
        let count = file.read(&mut bytes)?;
        state.viewer_name = entry.name.clone();
        state.viewer_size = actual_size;
        state.viewer_offset = aligned;
        let (preview, consumed) = if entry.kind == FileKind::Text {
            text_preview(&bytes[..count])
        } else {
            hex_preview(&bytes[..count], aligned)
        };
        state.viewer_next_offset = aligned.saturating_add(consumed as u64);
        state.viewer_preview = preview;
        state.view = View::Viewer;
        Ok(())
    }

    fn text_preview(bytes: &[u8]) -> (String, usize) {
        let mut output = String::new();
        let mut column = 0usize;
        let mut rows = 0usize;
        let mut consumed = 0usize;
        for &byte in bytes {
            if rows >= 29 {
                break;
            }
            consumed += 1;
            let ch = match byte {
                b'\n' => '\n',
                b'\r' => continue,
                32..=126 => byte as char,
                _ => '.',
            };
            if ch == '\n' || column >= 44 {
                output.push('\n');
                rows += 1;
                column = 0;
                if ch == '\n' {
                    continue;
                }
            }
            output.push(ch);
            column += 1;
        }
        (output, consumed)
    }

    fn hex_preview(bytes: &[u8], offset: u64) -> (String, usize) {
        let mut output = String::new();
        let visible = bytes.len().min(20 * 12);
        for (row, chunk) in bytes[..visible].chunks(12).enumerate() {
            let _ = write!(output, "{:08X}  ", offset + (row * 12) as u64);
            for byte in chunk {
                let _ = write!(output, "{:02X} ", byte);
            }
            output.push('\n');
        }
        (output, visible)
    }

    fn audio_levels(samples: &[i16]) -> (u16, u16) {
        let mut squares = 0u64;
        let mut peak = 0u16;
        let mut frames = 0u64;
        for sample in samples.iter().step_by(2) {
            let magnitude = sample.unsigned_abs();
            peak = peak.max(magnitude);
            squares = squares.saturating_add((magnitude as u64) * (magnitude as u64));
            frames += 1;
        }
        let mean = if frames == 0 { 0 } else { squares / frames };
        (integer_sqrt(mean).min(u16::MAX as u64) as u16, peak)
    }

    #[derive(Default)]
    struct EventRecorder {
        last_seq: u64,
        last_attempt_ms: u64,
        wrote_once: bool,
        retrying_after_error: bool,
    }

    impl EventRecorder {
        fn sync(&mut self, state: &mut DashboardState) {
            self.sync_inner(state, false);
        }

        fn flush(&mut self, state: &mut DashboardState) -> bool {
            self.sync_inner(state, true);
            !state.event_logging_enabled || state.storage.pending_events == 0
        }

        fn sync_inner(&mut self, state: &mut DashboardState, force: bool) {
            if !state.event_logging_enabled {
                self.last_seq = state
                    .events
                    .back()
                    .map(|event| event.seq)
                    .unwrap_or(self.last_seq);
                state.storage.pending_events = 0;
                return;
            }
            let pending_count = state
                .events
                .iter()
                .filter(|event| event.seq > self.last_seq)
                .count();
            let retry_delay_ms = if self.retrying_after_error {
                5_000
            } else {
                500
            };
            state.storage.pending_events = pending_count.min(u32::MAX as usize) as u32;
            if pending_count == 0
                || !state.storage.mounted
                || (!force
                    && state.runtime.uptime_ms.saturating_sub(self.last_attempt_ms)
                        < retry_delay_ms)
            {
                return;
            }
            self.last_attempt_ms = state.runtime.uptime_ms;
            let pending: Vec<_> = state
                .events
                .iter()
                .filter(|event| event.seq > self.last_seq)
                .cloned()
                .collect();
            match append_events(&pending) {
                Ok(bytes) => {
                    self.last_seq = pending
                        .last()
                        .map(|event| event.seq)
                        .unwrap_or(self.last_seq);
                    state.storage.event_bytes_written =
                        state.storage.event_bytes_written.saturating_add(bytes);
                    state.storage.pending_events = 0;
                    self.retrying_after_error = false;
                    if !self.wrote_once {
                        self.wrote_once = true;
                        info!("SD event recorder append verified ({bytes} bytes)");
                    }
                }
                Err(err) => {
                    warn!("event recorder write failed: {err:#}");
                    state.storage.write_errors = state.storage.write_errors.saturating_add(1);
                    state.storage.health = Health::Error;
                    self.retrying_after_error = true;
                }
            }
        }
    }

    fn append_events(events: &[hmi_core::RecordedEvent]) -> anyhow::Result<u64> {
        let mut output = Vec::new();
        for event in events {
            let source = json_safe(event.source, 24);
            let detail = json_safe(&event.detail, 192);
            let line = format!(
                "{{\"v\":1,\"seq\":{},\"at_ms\":{},\"source\":\"{}\",\"detail\":\"{}\"}}\n",
                event.seq, event.at_ms, source, detail
            );
            output.extend_from_slice(line.as_bytes());
        }
        let result = unsafe { hmi_board_sd_append(output.as_ptr(), output.len()) };
        if result != 0 {
            return Err(anyhow!("ESP-IDF FATFS append failed: {result}"));
        }
        Ok(output.len() as u64)
    }

    fn json_safe(value: &str, max_chars: usize) -> String {
        let mut output = String::with_capacity(value.len());
        for ch in value.chars().take(max_chars) {
            match ch {
                '\\' => output.push_str("\\\\"),
                '"' => output.push_str("\\\""),
                '\n' => output.push_str("\\n"),
                '\r' => output.push_str("\\r"),
                ch if ch.is_control() => output.push('?'),
                ch => output.push(ch),
            }
        }
        output
    }
}
