use std::{env, fs, path::PathBuf};

use anyhow::Context;
use embedded_graphics::{
    geometry::Size,
    pixelcolor::{BinaryColor, Rgb565},
};
use embedded_graphics_simulator::{OutputSettingsBuilder, SimulatorDisplay};
use hmi_core::{
    render_dashboard, render_touch349_dashboard, BatteryTelemetry, ClockTelemetry, DashboardState,
    EnvironmentTelemetry, FileEntry, FileKind, Health, StorageTelemetry, View,
};

fn main() -> anyhow::Result<()> {
    let (output, requested_page, board) = arguments();
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }

    let mut state = fixture();
    state.view = match requested_page.as_str() {
        "menu" => View::Menu,
        "recorder" => View::Recorder,
        "files" => View::Files,
        "player" => View::Player,
        "viewer" => View::Viewer,
        "live" => View::Live,
        "diagnostics" => View::Diagnostics,
        "settings" => View::Settings,
        _ => View::Home,
    };
    state.record(120, "BOOT", "firmware started");
    state.record(440, "WIFI", "connected, DHCP ready");
    state.record(690, "SHTC3", "sample accepted, CRC ok");
    state.record(830, "SD", "event recorder mounted");
    state.record(1_020, "MIC", "capture ring running");

    let settings = OutputSettingsBuilder::new()
        .scale(2)
        .pixel_spacing(0)
        .build();
    match board.as_str() {
        "touch349-v2" => {
            let mut display = SimulatorDisplay::<Rgb565>::new(Size::new(172, 640));
            render_touch349_dashboard(&mut display, &state)
                .expect("infallible Touch349 simulator target");
            display
                .to_rgb_output_image(&settings)
                .save_png(&output)
                .with_context(|| format!("write {}", output.display()))?;
        }
        _ => {
            let mut display = SimulatorDisplay::<BinaryColor>::new(Size::new(300, 400));
            render_dashboard(&mut display, &state).expect("infallible RLCD simulator target");
            display
                .to_rgb_output_image(&settings)
                .save_png(&output)
                .with_context(|| format!("write {}", output.display()))?;
        }
    }

    println!("rendered {}", output.display());
    Ok(())
}

fn arguments() -> (PathBuf, String, String) {
    let mut output = None;
    let mut page = None;
    let mut board = String::from("rlcd42");
    let mut positional = Vec::new();
    let mut args = env::args_os().skip(1);
    while let Some(argument) = args.next() {
        match argument.to_string_lossy().as_ref() {
            "--output" => output = args.next().map(PathBuf::from),
            "--page" => {
                page = args
                    .next()
                    .map(|value| value.to_string_lossy().into_owned())
            }
            "--board" => {
                if let Some(value) = args.next() {
                    board = value.to_string_lossy().into_owned();
                }
            }
            _ => positional.push(argument),
        }
    }
    if output.is_none() {
        output = positional.first().map(PathBuf::from);
    }
    if page.is_none() {
        page = positional
            .get(1)
            .map(|value| value.to_string_lossy().into_owned());
    }
    (
        output.unwrap_or_else(|| PathBuf::from("artifacts/dashboard.png")),
        page.unwrap_or_else(|| "home".into()),
        board,
    )
}

fn fixture() -> DashboardState {
    let mut state = DashboardState::default();
    state.runtime.uptime_ms = 12_450;
    state.runtime.free_heap = 182 * 1024;
    state.runtime.min_free_heap = 164 * 1024;
    state.runtime.free_psram = 6_940 * 1024;
    state.runtime.loop_hz = 50;
    state.runtime.display_flush_ms = 12;
    state.clock = ClockTelemetry {
        health: Health::Ok,
        zone: "IST",
        year: 2026,
        month: 8,
        day: 9,
        weekday: 0,
        hour: 23,
        minute: 24,
        second: 37,
    };

    state.wifi.health = Health::Ok;
    state.wifi.ssid = "current-wifi".into();
    state.wifi.ipv4 = "192.168.1.42".into();
    state.wifi.rssi_dbm = -57;
    state.environment = EnvironmentTelemetry {
        health: Health::Ok,
        temperature_centi_c: 2_684,
        humidity_centi_pct: 5_723,
        sample_age_ms: 450,
        crc_errors: 0,
    };
    state.battery = BatteryTelemetry {
        health: Health::Ok,
        millivolts: 4_031,
        percent: 92,
        raw: 2_204,
    };
    state.audio.health = Health::Ok;
    state.audio.sample_rate_hz = 24_000;
    state.audio.rms = 4_280;
    state.audio.peak = 11_902;
    state.audio.buffered_frames = 384;
    state.audio.buffer_capacity_frames = 2_048;
    state.audio.frames_total = 297_600;
    let sample_pattern = [
        320, 420, 380, 510, 680, 540, 830, 1_200, 940, 720, 1_480, 2_100, 1_760, 1_120, 880, 1_400,
        2_800, 4_100, 3_200, 2_300, 1_800, 2_900, 4_800, 3_700, 2_600, 1_900, 1_200, 980, 1_400,
        2_200, 3_600, 4_280,
    ];
    for index in 0..72 {
        state
            .audio
            .push_level(sample_pattern[index % sample_pattern.len()]);
    }
    state.storage = StorageTelemetry {
        health: Health::Ok,
        mounted: true,
        total_kib: 29_802_496,
        free_kib: 28_901_120,
        event_bytes_written: 18_432,
        pending_events: 0,
        write_errors: 0,
    };
    state.files = vec![
        FileEntry {
            name: "R0000042.WAV".into(),
            size: 1_728_044,
            kind: FileKind::Wav,
        },
        FileEntry {
            name: "R0000041.WAV".into(),
            size: 924_044,
            kind: FileKind::Wav,
        },
        FileEntry {
            name: "events.ndj".into(),
            size: 18_432,
            kind: FileKind::Text,
        },
        FileEntry {
            name: "notes.txt".into(),
            size: 2_048,
            kind: FileKind::Text,
        },
    ];
    state.last_recording = "R0000042.WAV".into();
    state.recording_name = "R0000043.WAV".into();
    state.recording_bytes = 684_032;
    state.playback_name = "R0000042.WAV".into();
    state.playback_position_ms = 8_300;
    state.playback_duration_ms = 18_000;
    state.playback_audio = state.audio;
    state.viewer_name = "events.ndj".into();
    state.viewer_size = 18_432;
    state.viewer_offset = 512;
    state.viewer_preview = "{\"v\":1,\"source\":\"BOOT\"}\n{\"v\":1,\"source\":\"WIFI\"}\n\nPaged text and hex viewer\nkeeps large files off the heap.".into();
    state
}
