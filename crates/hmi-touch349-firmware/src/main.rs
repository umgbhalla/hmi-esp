#[cfg(not(target_os = "espidf"))]
fn main() {
    println!("Touch349 product firmware targets ESP32-S3");
}

#[cfg(target_os = "espidf")]
mod firmware {
    use std::{
        env,
        ffi::CString,
        format, fs,
        fs::File,
        io::{Read, Seek, SeekFrom},
        path::{Component, Path, PathBuf},
        slice, thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use anyhow::{anyhow, ensure, Context};
    use esp_idf_sys::hmi_touch349::{
        hmi_touch349_audio_read_levels, hmi_touch349_backlight_set, hmi_touch349_battery_read,
        hmi_touch349_console_read, hmi_touch349_flush_full, hmi_touch349_flush_stats_t,
        hmi_touch349_framebuffer, hmi_touch349_free_heap, hmi_touch349_free_psram,
        hmi_touch349_init, hmi_touch349_network_scan, hmi_touch349_network_start,
        hmi_touch349_network_stats, hmi_touch349_network_stats_t,
        hmi_touch349_power_button_pressed, hmi_touch349_power_off, hmi_touch349_recorder_start,
        hmi_touch349_recorder_stats, hmi_touch349_recorder_stats_t, hmi_touch349_recorder_stop,
        hmi_touch349_sd_mount, hmi_touch349_sd_stats_t, hmi_touch349_touch_read,
    };
    use hmi_core::{
        decode_touch349_packet, render_touch349_dashboard, BatteryTelemetry, DashboardState,
        FileEntry, FileKind, Health, StorageTelemetry, Touch349FrameBuffer, UiAction, View,
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

    fn file_kind(name: &str) -> FileKind {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".wav") {
            FileKind::Wav
        } else if [".txt", ".log", ".csv", ".json", ".ndj", ".md"]
            .iter()
            .any(|extension| lower.ends_with(extension))
        {
            FileKind::Text
        } else {
            FileKind::Other
        }
    }

    fn collect_sd_files(
        root: &Path,
        relative: &Path,
        files: &mut Vec<FileEntry>,
        depth: u8,
    ) -> anyhow::Result<()> {
        if depth > 4 || files.len() >= 128 {
            return Ok(());
        }
        for entry in fs::read_dir(root.join(relative))? {
            let entry = entry?;
            let next = relative.join(entry.file_name());
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                collect_sd_files(root, &next, files, depth + 1)?;
            } else if metadata.is_file() {
                let name = next.to_string_lossy().into_owned();
                if !name.starts_with('.') && !name.contains("/.") {
                    files.push(FileEntry {
                        kind: file_kind(&name),
                        name,
                        size: metadata.len(),
                    });
                }
                if files.len() >= 128 {
                    break;
                }
            }
        }
        Ok(())
    }

    fn scan_sd_files() -> anyhow::Result<Vec<FileEntry>> {
        let mut files = Vec::new();
        collect_sd_files(Path::new("/sdcard"), Path::new(""), &mut files, 0)?;
        files.sort_by(|left, right| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
        });
        Ok(files)
    }

    fn safe_sd_path(name: &str) -> anyhow::Result<PathBuf> {
        let relative = Path::new(name);
        ensure!(!relative.is_absolute(), "absolute SD path is not allowed");
        ensure!(
            !relative
                .components()
                .any(|component| matches!(component, Component::ParentDir)),
            "parent SD path is not allowed"
        );
        Ok(Path::new("/sdcard").join(relative))
    }

    fn refresh_storage(
        state: &mut DashboardState,
        sd: &mut hmi_touch349_sd_stats_t,
    ) -> anyhow::Result<()> {
        let result = unsafe { hmi_touch349_sd_mount(sd) };
        ensure!(result == 0 && sd.mounted == 1, "SD mount failed: {result}");
        state.storage.mounted = true;
        state.storage.health = Health::Ok;
        state.storage.total_kib = sd.capacity_bytes / 1024;
        state.storage.free_kib = sd.free_bytes / 1024;
        state.files = scan_sd_files().context("scan SD card")?;
        state.file_index = state.file_index.min(state.files.len().saturating_sub(1));
        Ok(())
    }

    fn open_viewer(
        entry: &FileEntry,
        offset: u64,
        state: &mut DashboardState,
    ) -> anyhow::Result<()> {
        let path = safe_sd_path(&entry.name)?;
        let mut file = File::open(&path).with_context(|| format!("open {}", entry.name))?;
        let size = file.metadata()?.len();
        let offset = offset.min(size);
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = [0u8; 768];
        let count = file.read(&mut bytes)?;
        let preview = if entry.kind == FileKind::Text {
            String::from_utf8_lossy(&bytes[..count])
                .chars()
                .map(|character| {
                    if character == '\n' || character == '\t' || !character.is_control() {
                        character
                    } else {
                        ' '
                    }
                })
                .collect()
        } else {
            bytes[..count]
                .chunks(8)
                .take(18)
                .enumerate()
                .map(|(row, chunk)| {
                    let hex = chunk
                        .iter()
                        .map(|byte| format!("{byte:02X}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    format!("{:06X}  {hex}", offset + (row * 8) as u64)
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        state.viewer_name = entry.name.clone();
        state.viewer_size = size;
        state.viewer_offset = offset;
        state.viewer_next_offset = offset.saturating_add(count as u64);
        state.viewer_preview = preview;
        state.view = View::Viewer;
        Ok(())
    }

    fn wav_duration_ms(entry: &FileEntry) -> anyhow::Result<u64> {
        let mut file = File::open(safe_sd_path(&entry.name)?)?;
        let mut header = [0u8; 44];
        file.read_exact(&mut header)?;
        ensure!(
            &header[0..4] == b"RIFF" && &header[8..12] == b"WAVE",
            "invalid WAV"
        );
        ensure!(
            &header[12..16] == b"fmt " && &header[36..40] == b"data",
            "unsupported WAV layout"
        );
        let channels = u16::from_le_bytes([header[22], header[23]]);
        let rate = u32::from_le_bytes(header[24..28].try_into().unwrap());
        let bits = u16::from_le_bytes([header[34], header[35]]);
        ensure!(
            channels == 2 && rate == 24_000 && bits == 16,
            "WAV must be 24 kHz stereo 16-bit PCM"
        );
        let bytes = u32::from_le_bytes(header[40..44].try_into().unwrap()) as u64;
        Ok(bytes.saturating_mul(1000) / (24_000 * 2 * 2))
    }

    fn next_recording_name(_state: &DashboardState, now_ms: u64) -> String {
        // FAT long names are disabled. Keep every name inside the 8.3 format.
        let seed = (now_ms / 1000) as u32 % 1_000_000;
        for suffix in 0..100u32 {
            let name = format!("R{:06}.WAV", (seed + suffix) % 1_000_000);
            if !Path::new("/sdcard").join(&name).exists() {
                return name;
            }
        }
        "RECFAIL.WAV".into()
    }

    fn handle_action(
        action: UiAction,
        state: &mut DashboardState,
        sd: &mut hmi_touch349_sd_stats_t,
        now_ms: u64,
        recording_stop_pending: &mut bool,
    ) -> anyhow::Result<()> {
        match action {
            UiAction::RefreshFiles => refresh_storage(state, sd),
            UiAction::ToggleRecording if state.recording => {
                let result = unsafe { hmi_touch349_recorder_stop() };
                ensure!(result == 0, "stop recording failed: {result}");
                *recording_stop_pending = true;
                state.record(now_ms, "REC", "finalizing WAV");
                Ok(())
            }
            UiAction::ToggleRecording => {
                ensure!(state.storage.mounted, "SD card is not mounted");
                ensure!(state.audio.health == Health::Ok, "microphone is not ready");
                let name = next_recording_name(state, now_ms);
                let c_name = CString::new(name.as_str()).expect("generated name contains NUL");
                let result = unsafe { hmi_touch349_recorder_start(c_name.as_ptr()) };
                ensure!(result == 0, "start recording failed: {result}");
                state.recording = true;
                state.recording_name = name.clone();
                state.recording_started_ms = now_ms;
                state.recording_bytes = 0;
                state.record(now_ms, "REC", format!("started {name}"));
                Ok(())
            }
            UiAction::OpenLastRecording => {
                ensure!(
                    !state.last_recording.is_empty(),
                    "no recording is available"
                );
                let entry = state
                    .files
                    .iter()
                    .find(|entry| entry.name == state.last_recording)
                    .cloned()
                    .ok_or_else(|| anyhow!("last recording is not on the SD card"))?;
                open_media_entry(entry, state)
            }
            UiAction::OpenSelectedFile => {
                let entry = state
                    .selected_file()
                    .cloned()
                    .ok_or_else(|| anyhow!("no SD file is selected"))?;
                open_media_entry(entry, state)
            }
            UiAction::ViewerNext => {
                ensure!(!state.viewer_name.is_empty(), "no file is open");
                let entry = FileEntry {
                    name: state.viewer_name.clone(),
                    size: state.viewer_size,
                    kind: file_kind(&state.viewer_name),
                };
                let offset = if state.viewer_next_offset >= state.viewer_size {
                    0
                } else {
                    state.viewer_next_offset
                };
                open_viewer(&entry, offset, state)
            }
            UiAction::ViewerTop => {
                ensure!(!state.viewer_name.is_empty(), "no file is open");
                let entry = FileEntry {
                    name: state.viewer_name.clone(),
                    size: state.viewer_size,
                    kind: file_kind(&state.viewer_name),
                };
                open_viewer(&entry, 0, state)
            }
            UiAction::StopPlayback => {
                state.playing = false;
                Ok(())
            }
            UiAction::RefreshDisplay => Ok(()),
            UiAction::PreparePoweroff => {
                state.prepare_poweroff_requested = true;
                Ok(())
            }
            UiAction::TogglePlayback | UiAction::CycleVolume | UiAction::SpeakerTest => {
                state.playing = false;
                Err(anyhow!(
                    "speaker playback is not installed in this firmware"
                ))
            }
            UiAction::ToggleEventLog => {
                state.event_logging_enabled = !state.event_logging_enabled;
                Ok(())
            }
        }
    }

    fn open_media_entry(entry: FileEntry, state: &mut DashboardState) -> anyhow::Result<()> {
        if entry.kind == FileKind::Wav {
            state.playback_duration_ms = wav_duration_ms(&entry)?;
            state.playback_position_ms = 0;
            state.playback_name = entry.name;
            state.playback_audio.health = Health::Error;
            state.playing = false;
            state.view = View::Player;
            Ok(())
        } else {
            open_viewer(&entry, 0, state)
        }
    }

    fn poll_recorder(
        state: &mut DashboardState,
        sd: &mut hmi_touch349_sd_stats_t,
        now_ms: u64,
        recording_stop_pending: &mut bool,
    ) -> bool {
        let mut stats = hmi_touch349_recorder_stats_t {
            bytes_written: 0,
            rms: 0,
            peak: 0,
            last_error: 0,
            ready: 0,
            recording: 0,
        };
        if unsafe { hmi_touch349_recorder_stats(&mut stats) } != 0 {
            state.audio.health = Health::Error;
            return false;
        }
        state.audio.health = if stats.ready == 1 {
            Health::Ok
        } else {
            Health::Error
        };
        state.audio.rms = stats.rms.min(u32::from(u16::MAX)) as u16;
        state.audio.peak = stats.peak.min(u32::from(u16::MAX)) as u16;
        state.audio.frames_total = stats.bytes_written / 4;
        state.audio.push_level(state.audio.rms);
        state.recording_bytes = stats.bytes_written;
        if stats.recording == 1 {
            state.recording = true;
            return true;
        }
        if state.recording || *recording_stop_pending {
            state.recording = false;
            *recording_stop_pending = false;
            if stats.last_error == 0 {
                state.last_recording = state.recording_name.clone();
                state.record(
                    now_ms,
                    "REC",
                    format!("saved {} bytes", stats.bytes_written),
                );
                if let Err(error) = refresh_storage(state, sd) {
                    state.storage.health = Health::Error;
                    println!("SD REFRESH ERROR {error:#}");
                }
            } else {
                state.storage.write_errors = state.storage.write_errors.saturating_add(1);
                state.storage.health = Health::Error;
                state.record(now_ms, "REC", format!("save failed {}", stats.last_error));
            }
            return true;
        }
        let mut rms = 0u32;
        let mut peak = 0u32;
        if unsafe { hmi_touch349_audio_read_levels(&mut rms, &mut peak) } == 0 {
            state.audio.rms = rms.min(u32::from(u16::MAX)) as u16;
            state.audio.peak = peak.min(u32::from(u16::MAX)) as u16;
            state.audio.push_level(state.audio.rms);
            return matches!(state.view, View::Live | View::Recorder);
        }
        false
    }

    fn stop_recorder_before_poweroff(state: &mut DashboardState) -> bool {
        if !state.recording {
            return true;
        }
        let _ = unsafe { hmi_touch349_recorder_stop() };
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            let mut stats = hmi_touch349_recorder_stats_t {
                bytes_written: 0,
                rms: 0,
                peak: 0,
                last_error: 0,
                ready: 0,
                recording: 0,
            };
            if unsafe { hmi_touch349_recorder_stats(&mut stats) } == 0 && stats.recording == 0 {
                state.recording = false;
                return stats.last_error == 0;
            }
            thread::sleep(Duration::from_millis(50));
        }
        false
    }
    fn execute_console_command(
        command: &str,
        state: &mut DashboardState,
        sd: &mut hmi_touch349_sd_stats_t,
        network: &mut hmi_touch349_network_stats_t,
        now_ms: u64,
        recording_stop_pending: &mut bool,
    ) -> bool {
        let command = command.trim();
        if command.is_empty() {
            return false;
        }
        println!("CMD request={command}");
        let result = (|| -> anyhow::Result<()> {
            match command {
                "help" => {
                    println!("CMD commands=help,status,wifi scan,sd scan,record start,record stop");
                    Ok(())
                }
                "status" => {
                    unsafe { hmi_touch349_network_stats(network) };
                    println!(
                    "CMD STATUS view={} sd={} files={} wifi={} target={} channel={} reason={} retries={} time={} rec={} bytes={} mic={} rms={} peak={} brightness={}",
                    state.view.title(), sd.mounted, state.files.len(), network.connected,
                    network.target_visible, network.target_channel, network.last_disconnect_reason,
                    network.reconnects, network.time_synced, state.recording,
                    state.recording_bytes, state.audio.health.label(), state.audio.rms,
                    state.audio.peak, state.display_brightness.max(MIN_BRIGHTNESS_PERCENT),
                );
                    Ok(())
                }
                "wifi scan" => {
                    let result = unsafe { hmi_touch349_network_scan() };
                    ensure!(result == 0, "Wi-Fi scan failed: {result}");
                    unsafe { hmi_touch349_network_stats(network) };
                    println!(
                        "CMD WIFI target={} channel={} connected={} reason={}",
                        network.target_visible,
                        network.target_channel,
                        network.connected,
                        network.last_disconnect_reason,
                    );
                    Ok(())
                }
                "sd scan" => {
                    refresh_storage(state, sd)?;
                    println!(
                        "CMD SD mounted={} files={} free_bytes={}",
                        sd.mounted,
                        state.files.len(),
                        sd.free_bytes
                    );
                    Ok(())
                }
                "record start" => handle_action(
                    UiAction::ToggleRecording,
                    state,
                    sd,
                    now_ms,
                    recording_stop_pending,
                ),
                "record stop" => {
                    ensure!(state.recording, "recorder is not active");
                    handle_action(
                        UiAction::ToggleRecording,
                        state,
                        sd,
                        now_ms,
                        recording_stop_pending,
                    )
                }
                _ => Err(anyhow!("unknown command; send help")),
            }
        })();
        match result {
            Ok(()) => println!("CMD OK request={command}"),
            Err(error) => println!("CMD ERROR request={command} error={error:#}"),
        }
        true
    }

    fn poll_console(
        line: &mut String,
        state: &mut DashboardState,
        sd: &mut hmi_touch349_sd_stats_t,
        network: &mut hmi_touch349_network_stats_t,
        now_ms: u64,
        recording_stop_pending: &mut bool,
    ) -> bool {
        let mut input = [0u8; 64];
        let count = unsafe { hmi_touch349_console_read(input.as_mut_ptr(), input.len()) };
        if count <= 0 {
            return false;
        }
        let mut redraw = false;
        for byte in &input[..count as usize] {
            match *byte {
                b'\r' | b'\n' => {
                    if !line.is_empty() {
                        redraw |= execute_console_command(
                            line,
                            state,
                            sd,
                            network,
                            now_ms,
                            recording_stop_pending,
                        );
                        line.clear();
                    }
                }
                8 | 127 => {
                    line.pop();
                }
                byte if byte.is_ascii_graphic() || byte == b' ' => {
                    if line.len() < 96 {
                        line.push(byte as char);
                    }
                }
                _ => {}
            }
        }
        redraw
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
        state.wifi.reconnects = network.reconnects;
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
        match scan_sd_files() {
            Ok(files) => state.files = files,
            Err(error) => {
                state.storage.health = Health::Error;
                println!("SD SCAN ERROR {error:#}");
            }
        }
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
        let mut recorder_refresh = Instant::now() - Duration::from_secs(1);
        let mut recording_stop_pending = false;
        let mut console_line = String::new();
        let mut network = hmi_touch349_network_stats_t {
            ipv4: 0,
            reconnects: 0,
            rssi_dbm: 0,
            last_disconnect_reason: 0,
            target_visible: 0,
            target_channel: 0,
            connected: 0,
            time_synced: 0,
        };

        loop {
            let now_ms = boot_time.elapsed().as_millis() as u64;
            redraw |= poll_console(
                &mut console_line,
                &mut state,
                &mut sd,
                &mut network,
                now_ms,
                &mut recording_stop_pending,
            );
            let power_pressed = unsafe { hmi_touch349_power_button_pressed() };
            if power_pressed {
                let started = *power_pressed_since.get_or_insert_with(Instant::now);
                let held = started.elapsed();
                if held >= POWER_HOLD {
                    if !stop_recorder_before_poweroff(&mut state) {
                        println!("POWER OFF blocked: recorder did not finalize safely");
                        power_pressed_since = None;
                        redraw = true;
                        continue;
                    }
                    println!("POWER OFF threshold reached; recorder safe; releasing SYS_EN");
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
                        if let Some(action) = state.take_action() {
                            println!("ACTION dispatch={action:?}");
                            if let Err(error) = handle_action(
                                action,
                                &mut state,
                                &mut sd,
                                now_ms,
                                &mut recording_stop_pending,
                            ) {
                                println!("ACTION ERROR action={action:?} error={error:#}");
                                state.record(now_ms, "ACTION", format!("{action:?}: {error}"));
                            } else {
                                println!("ACTION OK action={action:?}");
                            }
                        }
                        last_touch = None;
                        redraw = true;
                    }
                }
            }

            state.runtime.uptime_ms = now_ms;
            if recorder_refresh.elapsed() >= Duration::from_millis(100) {
                redraw |= poll_recorder(&mut state, &mut sd, now_ms, &mut recording_stop_pending);
                recorder_refresh = Instant::now();
            }
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
                    "PRODUCT HEARTBEAT view={} touch={} sd={} wifi={} target={} ch={} reason={} retries={} time={} battery={}mV/{}% heap={}K psram={}K frame={}us flush={}us fps={}",
                    state.view.title(),
                    touch_result,
                    sd.mounted,
                    network.connected,
                    network.target_visible,
                    network.target_channel,
                    network.last_disconnect_reason,
                    network.reconnects,
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
