#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::{collections::VecDeque, format, string::String, vec::Vec};
use core::fmt::Write as _;

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_10X20, FONT_6X10, FONT_9X15_BOLD},
        MonoTextStyle,
    },
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{Line, PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};

pub const EVENT_CAPACITY: usize = 96;
pub const AUDIO_HISTORY_CAPACITY: usize = 72;
pub const BUTTON_DEBOUNCE_MS: u64 = 35;
pub const BUTTON_LONG_PRESS_MS: u64 = 650;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Button {
    Boot,
    Key,
}

impl Button {
    pub const ALL: [Self; 2] = [Self::Boot, Self::Key];

    pub const fn index(self) -> usize {
        match self {
            Self::Boot => 0,
            Self::Key => 1,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Boot => "BOOT",
            Self::Key => "KEY",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Gesture {
    Click,
    LongPress,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputEvent {
    pub button: Button,
    pub gesture: Gesture,
    pub held_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Health {
    Unknown,
    Ok,
    Stale,
    Error,
}

impl Health {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "WAIT",
            Self::Ok => "OK",
            Self::Stale => "STALE",
            Self::Error => "ERR",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum View {
    Home,
    Menu,
    Recorder,
    Files,
    Player,
    Viewer,
    Live,
    Diagnostics,
    Settings,
}

impl View {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Home => "HOME",
            Self::Menu => "APPS",
            Self::Recorder => "RECORDER",
            Self::Files => "SD FILES",
            Self::Player => "AUDIO PLAYER",
            Self::Viewer => "FILE VIEWER",
            Self::Live => "LIVE AUDIO",
            Self::Diagnostics => "DIAGNOSTICS",
            Self::Settings => "SETTINGS",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiAction {
    RefreshFiles,
    ToggleRecording,
    OpenLastRecording,
    OpenSelectedFile,
    TogglePlayback,
    StopPlayback,
    ViewerNext,
    ViewerTop,
    RefreshDisplay,
    CycleVolume,
    ToggleEventLog,
    SpeakerTest,
    PreparePoweroff,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileKind {
    Wav,
    Text,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileEntry {
    pub name: String,
    pub size: u64,
    pub kind: FileKind,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ButtonTelemetry {
    pub pressed: bool,
    pub held_ms: u64,
    pub clicks: u32,
    pub long_presses: u32,
}

#[derive(Clone, Debug)]
pub struct WifiTelemetry {
    pub health: Health,
    pub ssid: String,
    pub ipv4: String,
    pub rssi_dbm: i16,
    pub reconnects: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct ClockTelemetry {
    pub health: Health,
    pub zone: &'static str,
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub weekday: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl Default for ClockTelemetry {
    fn default() -> Self {
        Self {
            health: Health::Unknown,
            zone: "LOCAL",
            year: 0,
            month: 0,
            day: 0,
            weekday: 0,
            hour: 0,
            minute: 0,
            second: 0,
        }
    }
}

impl Default for WifiTelemetry {
    fn default() -> Self {
        Self {
            health: Health::Unknown,
            ssid: String::new(),
            ipv4: String::new(),
            rssi_dbm: 0,
            reconnects: 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct EnvironmentTelemetry {
    pub health: Health,
    pub temperature_centi_c: i32,
    pub humidity_centi_pct: u32,
    pub sample_age_ms: u64,
    pub crc_errors: u32,
}

impl Default for EnvironmentTelemetry {
    fn default() -> Self {
        Self {
            health: Health::Unknown,
            temperature_centi_c: 0,
            humidity_centi_pct: 0,
            sample_age_ms: 0,
            crc_errors: 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BatteryTelemetry {
    pub health: Health,
    pub millivolts: u32,
    pub percent: u8,
    pub raw: u16,
}

impl Default for BatteryTelemetry {
    fn default() -> Self {
        Self {
            health: Health::Unknown,
            millivolts: 0,
            percent: 0,
            raw: 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AudioTelemetry {
    pub health: Health,
    pub sample_rate_hz: u32,
    pub rms: u16,
    pub peak: u16,
    pub buffered_frames: u32,
    pub buffer_capacity_frames: u32,
    pub frames_total: u64,
    pub overruns: u32,
    pub level_history: [u16; AUDIO_HISTORY_CAPACITY],
    pub history_len: u8,
    pub history_head: u8,
}

impl Default for AudioTelemetry {
    fn default() -> Self {
        Self {
            health: Health::Unknown,
            sample_rate_hz: 24_000,
            rms: 0,
            peak: 0,
            buffered_frames: 0,
            buffer_capacity_frames: 0,
            frames_total: 0,
            overruns: 0,
            level_history: [0; AUDIO_HISTORY_CAPACITY],
            history_len: 0,
            history_head: 0,
        }
    }
}

impl AudioTelemetry {
    pub fn push_level(&mut self, rms: u16) {
        let head = self.history_head as usize;
        self.level_history[head] = rms;
        self.history_head = ((head + 1) % AUDIO_HISTORY_CAPACITY) as u8;
        self.history_len = self
            .history_len
            .saturating_add(1)
            .min(AUDIO_HISTORY_CAPACITY as u8);
    }

    fn history_sample(&self, chronological_index: usize) -> Option<u16> {
        let len = self.history_len as usize;
        if chronological_index >= len {
            return None;
        }
        let oldest = if len == AUDIO_HISTORY_CAPACITY {
            self.history_head as usize
        } else {
            0
        };
        Some(self.level_history[(oldest + chronological_index) % AUDIO_HISTORY_CAPACITY])
    }

    fn history_peak(&self) -> u16 {
        (0..self.history_len as usize)
            .filter_map(|index| self.history_sample(index))
            .max()
            .unwrap_or(1)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StorageTelemetry {
    pub health: Health,
    pub mounted: bool,
    pub total_kib: u64,
    pub free_kib: u64,
    pub event_bytes_written: u64,
    pub pending_events: u32,
    pub write_errors: u32,
}

impl Default for StorageTelemetry {
    fn default() -> Self {
        Self {
            health: Health::Unknown,
            mounted: false,
            total_kib: 0,
            free_kib: 0,
            event_bytes_written: 0,
            pending_events: 0,
            write_errors: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RuntimeTelemetry {
    pub uptime_ms: u64,
    pub free_heap: u32,
    pub min_free_heap: u32,
    pub free_psram: u32,
    pub loop_hz: u16,
    pub display_flush_ms: u16,
    pub dropped_events: u32,
}

#[derive(Clone, Debug)]
pub struct RecordedEvent {
    pub seq: u64,
    pub at_ms: u64,
    pub source: &'static str,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct DashboardState {
    pub view: View,
    pub menu_index: u8,
    pub event_offset: usize,
    pub clock: ClockTelemetry,
    pub wifi: WifiTelemetry,
    pub environment: EnvironmentTelemetry,
    pub battery: BatteryTelemetry,
    pub audio: AudioTelemetry,
    pub storage: StorageTelemetry,
    pub runtime: RuntimeTelemetry,
    pub buttons: [ButtonTelemetry; 2],
    pub settings_index: u8,
    pub speaker_volume: u8,
    pub event_logging_enabled: bool,
    pub recording: bool,
    pub recording_name: String,
    pub recording_bytes: u64,
    pub recording_started_ms: u64,
    pub last_recording: String,
    pub files: Vec<FileEntry>,
    pub file_index: usize,
    pub playing: bool,
    pub playback_name: String,
    pub playback_position_ms: u64,
    pub playback_duration_ms: u64,
    pub playback_audio: AudioTelemetry,
    pub viewer_name: String,
    pub viewer_size: u64,
    pub viewer_offset: u64,
    pub viewer_next_offset: u64,
    pub viewer_preview: String,
    pub poweroff_prepared: bool,
    pub prepare_poweroff_requested: bool,
    pub refresh_requested: bool,
    pending_action: Option<UiAction>,
    pub events: VecDeque<RecordedEvent>,
    next_event_seq: u64,
}

impl Default for DashboardState {
    fn default() -> Self {
        Self {
            view: View::Home,
            menu_index: 0,
            event_offset: 0,
            clock: ClockTelemetry::default(),
            wifi: WifiTelemetry::default(),
            environment: EnvironmentTelemetry::default(),
            battery: BatteryTelemetry::default(),
            audio: AudioTelemetry::default(),
            storage: StorageTelemetry::default(),
            runtime: RuntimeTelemetry::default(),
            buttons: [ButtonTelemetry::default(); 2],
            settings_index: 0,
            speaker_volume: 55,
            event_logging_enabled: false,
            recording: false,
            recording_name: String::new(),
            recording_bytes: 0,
            recording_started_ms: 0,
            last_recording: String::new(),
            files: Vec::new(),
            file_index: 0,
            playing: false,
            playback_name: String::new(),
            playback_position_ms: 0,
            playback_duration_ms: 0,
            playback_audio: AudioTelemetry::default(),
            viewer_name: String::new(),
            viewer_size: 0,
            viewer_offset: 0,
            viewer_next_offset: 0,
            viewer_preview: String::new(),
            poweroff_prepared: false,
            prepare_poweroff_requested: false,
            refresh_requested: false,
            pending_action: None,
            events: VecDeque::with_capacity(EVENT_CAPACITY),
            next_event_seq: 1,
        }
    }
}

impl DashboardState {
    pub fn take_action(&mut self) -> Option<UiAction> {
        self.pending_action.take()
    }

    pub fn selected_file(&self) -> Option<&FileEntry> {
        self.files.get(self.file_index)
    }

    pub fn record(&mut self, at_ms: u64, source: &'static str, detail: impl Into<String>) -> u64 {
        let seq = self.next_event_seq;
        self.next_event_seq = self.next_event_seq.saturating_add(1);
        if self.events.len() == EVENT_CAPACITY {
            self.events.pop_front();
            self.runtime.dropped_events = self.runtime.dropped_events.saturating_add(1);
        }
        self.events.push_back(RecordedEvent {
            seq,
            at_ms,
            source,
            detail: detail.into(),
        });
        seq
    }

    pub fn apply_input(&mut self, event: InputEvent, now_ms: u64) {
        // An action belongs to exactly one input event. Discard any stale
        // reducer output before handling the next physical gesture.
        self.pending_action = None;
        // Any interaction after a successful prepare makes that promise stale.
        self.poweroff_prepared = false;

        let stats = &mut self.buttons[event.button.index()];
        match event.gesture {
            Gesture::Click => stats.clicks = stats.clicks.saturating_add(1),
            Gesture::LongPress => stats.long_presses = stats.long_presses.saturating_add(1),
        }
        stats.held_ms = event.held_ms;

        match (event.button, event.gesture) {
            (Button::Key, Gesture::LongPress) => {
                self.pending_action = match self.view {
                    View::Recorder if self.recording => Some(UiAction::ToggleRecording),
                    View::Player => Some(UiAction::StopPlayback),
                    _ => None,
                };
                self.view = View::Home;
            }
            (Button::Key, Gesture::Click) => {
                self.pending_action = match self.view {
                    View::Recorder if self.recording => Some(UiAction::ToggleRecording),
                    View::Player => Some(UiAction::StopPlayback),
                    _ => None,
                };
                self.view = match self.view {
                    View::Home => View::Home,
                    View::Menu => View::Home,
                    View::Player | View::Viewer => View::Files,
                    _ => View::Menu,
                };
            }
            (Button::Boot, Gesture::Click) => match self.view {
                View::Home => {
                    self.menu_index = 0;
                    self.view = View::Menu;
                }
                View::Menu => self.menu_index = (self.menu_index + 1) % 5,
                View::Files => {
                    if !self.files.is_empty() {
                        self.file_index = (self.file_index + 1) % self.files.len();
                    }
                }
                View::Recorder => self.pending_action = Some(UiAction::ToggleRecording),
                View::Player => self.pending_action = Some(UiAction::TogglePlayback),
                View::Viewer => self.pending_action = Some(UiAction::ViewerNext),
                View::Live => {}
                View::Diagnostics => self.event_offset = self.event_offset.saturating_add(5),
                View::Settings => {
                    self.settings_index = (self.settings_index + 1) % 6;
                }
            },
            (Button::Boot, Gesture::LongPress) => match self.view {
                View::Home => {
                    self.menu_index = 0;
                    self.view = View::Menu;
                }
                View::Menu => {
                    self.view = match self.menu_index {
                        0 => View::Recorder,
                        1 => View::Files,
                        2 => View::Live,
                        3 => View::Diagnostics,
                        _ => View::Settings,
                    };
                    if self.view == View::Files {
                        self.pending_action = Some(UiAction::RefreshFiles);
                    }
                }
                View::Files => self.pending_action = Some(UiAction::OpenSelectedFile),
                View::Recorder => self.pending_action = Some(UiAction::OpenLastRecording),
                View::Player => self.pending_action = Some(UiAction::StopPlayback),
                View::Viewer => self.pending_action = Some(UiAction::ViewerTop),
                View::Live => self.view = View::Recorder,
                View::Diagnostics => self.event_offset = 0,
                View::Settings => {
                    self.pending_action = Some(match self.settings_index {
                        0 => UiAction::RefreshDisplay,
                        1 => UiAction::CycleVolume,
                        2 => UiAction::SpeakerTest,
                        3 => UiAction::ToggleEventLog,
                        4 => UiAction::RefreshFiles,
                        _ => UiAction::PreparePoweroff,
                    });
                }
            },
        }

        let mut detail = String::new();
        let gesture = match event.gesture {
            Gesture::Click => "click",
            Gesture::LongPress => "long",
        };
        let _ = write!(
            detail,
            "{} {} {}ms",
            event.button.label(),
            gesture,
            event.held_ms
        );
        self.record(now_ms, "KEY", detail);
    }
}

#[derive(Clone, Copy, Debug)]
struct DebouncedButton {
    raw_pressed: bool,
    stable_pressed: bool,
    raw_changed_ms: u64,
    pressed_at_ms: u64,
    long_reported: bool,
}

impl Default for DebouncedButton {
    fn default() -> Self {
        Self {
            raw_pressed: false,
            stable_pressed: false,
            raw_changed_ms: 0,
            pressed_at_ms: 0,
            long_reported: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ButtonEngine {
    buttons: [DebouncedButton; 2],
}

impl ButtonEngine {
    pub fn sample(&mut self, button: Button, pressed: bool, now_ms: u64) -> Option<InputEvent> {
        let state = &mut self.buttons[button.index()];
        if pressed != state.raw_pressed {
            state.raw_pressed = pressed;
            state.raw_changed_ms = now_ms;
        }

        if state.raw_pressed != state.stable_pressed
            && now_ms.saturating_sub(state.raw_changed_ms) >= BUTTON_DEBOUNCE_MS
        {
            state.stable_pressed = state.raw_pressed;
            if state.stable_pressed {
                state.pressed_at_ms = now_ms;
                state.long_reported = false;
            } else if !state.long_reported {
                return Some(InputEvent {
                    button,
                    gesture: Gesture::Click,
                    held_ms: now_ms.saturating_sub(state.pressed_at_ms),
                });
            }
        }

        if state.stable_pressed
            && !state.long_reported
            && now_ms.saturating_sub(state.pressed_at_ms) >= BUTTON_LONG_PRESS_MS
        {
            state.long_reported = true;
            return Some(InputEvent {
                button,
                gesture: Gesture::LongPress,
                held_ms: now_ms.saturating_sub(state.pressed_at_ms),
            });
        }

        None
    }

    pub fn held_ms(&self, button: Button, now_ms: u64) -> u64 {
        let state = &self.buttons[button.index()];
        if state.stable_pressed {
            now_ms.saturating_sub(state.pressed_at_ms)
        } else {
            0
        }
    }
}

const BG: BinaryColor = BinaryColor::On;
const INK: BinaryColor = BinaryColor::Off;

pub fn render_dashboard<D>(display: &mut D, state: &DashboardState) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor> + OriginDimensions,
{
    display.clear(BG)?;
    let style = MonoTextStyle::new(&FONT_6X10, INK);
    let inverse = MonoTextStyle::new(&FONT_6X10, BG);

    if state.view == View::Home {
        render_clock_home(display, state, style)?;
    } else {
        render_title(display, state.view.title(), style)?;
        match state.view {
            View::Menu => render_menu(display, state, style)?,
            View::Recorder => render_recorder(display, state, style)?,
            View::Files => render_files(display, state, style)?,
            View::Player => render_player(display, state, style)?,
            View::Viewer => render_viewer(display, state, style)?,
            View::Live => render_live(display, state, style)?,
            View::Diagnostics => render_diagnostics(display, state, style)?,
            View::Settings => render_system_settings(display, state, style)?,
            View::Home => {}
        }
    }

    Rectangle::new(Point::new(0, 376), Size::new(300, 24))
        .into_styled(PrimitiveStyle::with_fill(INK))
        .draw(display)?;
    let (key, boot) = footer_labels(state.view);
    Text::with_baseline(key, Point::new(6, 383), inverse, Baseline::Top).draw(display)?;
    Text::with_baseline("PWR HOLD OFF", Point::new(112, 383), inverse, Baseline::Top)
        .draw(display)?;
    Text::with_baseline(boot, Point::new(216, 383), inverse, Baseline::Top).draw(display)?;
    Ok(())
}

fn footer_labels(view: View) -> (&'static str, &'static str) {
    match view {
        View::Home => ("KEY HOME", "BOOT MENU"),
        View::Menu => ("KEY BACK", "BOOT NEXT/OPEN"),
        View::Recorder => ("KEY BACK", "BOOT REC/PLAY"),
        View::Files => ("KEY BACK", "BOOT NEXT/OPEN"),
        View::Player => ("KEY BACK", "BOOT PLAY/STOP"),
        View::Viewer => ("KEY BACK", "BOOT NEXT/TOP"),
        View::Live => ("KEY BACK", "BOOT HOLD REC"),
        View::Diagnostics => ("KEY BACK", "BOOT MORE/TOP"),
        View::Settings => ("KEY BACK", "BOOT NEXT/SET"),
    }
}

fn render_title<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D,
    title: &str,
    style: MonoTextStyle<'_, BinaryColor>,
) -> Result<(), D::Error> {
    let bold = MonoTextStyle::new(&FONT_9X15_BOLD, INK);
    Text::with_baseline(title, Point::new(10, 10), bold, Baseline::Top).draw(display)?;
    Line::new(Point::new(10, 32), Point::new(290, 32))
        .into_styled(PrimitiveStyle::with_stroke(INK, 2))
        .draw(display)?;
    line(display, "HOLD KEY = HOME", 198, 13, style)
}

fn render_clock_home<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D,
    state: &DashboardState,
    style: MonoTextStyle<'_, BinaryColor>,
) -> Result<(), D::Error> {
    let bold = MonoTextStyle::new(&FONT_9X15_BOLD, INK);
    let mut s = String::new();
    if state.clock.health == Health::Ok {
        draw_large_clock(display, 8, 16, state.clock.hour, state.clock.minute)?;
        line(
            display,
            &format!("{:02}", state.clock.second),
            252,
            38,
            bold,
        )?;
        line(display, state.clock.zone, 252, 65, style)?;
        s.clear();
        let _ = write!(
            s,
            "{}  {:02} {} {}",
            weekday_name(state.clock.weekday),
            state.clock.day,
            month_name(state.clock.month),
            state.clock.year
        );
        line(display, &s, 14, 108, bold)?;
    } else {
        Text::with_baseline(
            "--:--",
            Point::new(40, 36),
            MonoTextStyle::new(&FONT_10X20, INK),
            Baseline::Top,
        )
        .draw(display)?;
        line(display, "SYNCING NETWORK TIME", 40, 78, bold)?;
    }
    Line::new(Point::new(10, 132), Point::new(290, 132))
        .into_styled(PrimitiveStyle::with_stroke(INK, 2))
        .draw(display)?;
    Rectangle::new(Point::new(10, 146), Size::new(135, 70))
        .into_styled(PrimitiveStyle::with_stroke(INK, 1))
        .draw(display)?;
    line(display, "ENV CAL", 20, 155, style)?;
    if state.environment.health == Health::Ok {
        s.clear();
        let _ = write!(
            s,
            "{}.{:01} C",
            state.environment.temperature_centi_c / 100,
            (state.environment.temperature_centi_c.unsigned_abs() / 10) % 10
        );
        line(display, &s, 20, 176, bold)?;
        s.clear();
        let _ = write!(s, "{}% RH", state.environment.humidity_centi_pct / 100);
        line(display, &s, 20, 200, style)?;
    } else {
        line(display, "SENSOR ERR", 20, 176, bold)?;
        line(display, "NO FRESH SAMPLE", 20, 200, style)?;
    }

    Rectangle::new(Point::new(155, 146), Size::new(135, 70))
        .into_styled(PrimitiveStyle::with_stroke(INK, 1))
        .draw(display)?;
    line(display, "NETWORK", 165, 155, style)?;
    draw_wifi(display, 165, 183, state.wifi.rssi_dbm, state.wifi.health)?;
    line(
        display,
        if state.wifi.health == Health::Ok {
            "ONLINE"
        } else if state.wifi.health == Health::Error {
            "OFFLINE"
        } else {
            "CONNECTING"
        },
        195,
        176,
        bold,
    )?;
    line(
        display,
        if state.wifi.ipv4.is_empty() {
            "NO ADDRESS"
        } else {
            state.wifi.ipv4.as_str()
        },
        165,
        200,
        style,
    )?;

    Rectangle::new(Point::new(10, 226), Size::new(280, 54))
        .into_styled(PrimitiveStyle::with_stroke(INK, 1))
        .draw(display)?;
    line(display, "BATTERY", 20, 237, style)?;
    if state.battery.health == Health::Ok {
        draw_battery(display, 76, 236, state.battery.percent)?;
        s.clear();
        let _ = write!(s, "{}%", state.battery.percent);
        line(display, &s, 115, 236, bold)?;
    } else {
        line(display, "ADC ERR", 76, 236, bold)?;
    }
    line(
        display,
        if state.storage.mounted {
            "SD READY"
        } else {
            "NO SD CARD"
        },
        188,
        237,
        style,
    )?;

    line(
        display,
        if state.recording {
            "REC  Recording to SD"
        } else {
            "Recorder idle - audio is not being saved"
        },
        12,
        292,
        style,
    )?;
    draw_waveform(display, &state.audio, 311, 48)
}

fn draw_large_clock<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D,
    x: i32,
    y: i32,
    hour: u8,
    minute: u8,
) -> Result<(), D::Error> {
    draw_large_digit(display, x, y, hour / 10)?;
    draw_large_digit(display, x + 50, y, hour % 10)?;
    Rectangle::new(Point::new(x + 101, y + 20), Size::new(7, 7))
        .into_styled(PrimitiveStyle::with_fill(INK))
        .draw(display)?;
    Rectangle::new(Point::new(x + 101, y + 53), Size::new(7, 7))
        .into_styled(PrimitiveStyle::with_fill(INK))
        .draw(display)?;
    draw_large_digit(display, x + 116, y, minute / 10)?;
    draw_large_digit(display, x + 166, y, minute % 10)
}

fn draw_large_digit<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D,
    x: i32,
    y: i32,
    digit: u8,
) -> Result<(), D::Error> {
    const SEGMENTS: [u8; 10] = [0x3f, 0x06, 0x5b, 0x4f, 0x66, 0x6d, 0x7d, 0x07, 0x7f, 0x6f];
    let mask = SEGMENTS[digit.min(9) as usize];
    for (bit, sy) in [(0u8, y), (6, y + 38), (3, y + 76)] {
        draw_clock_segment(
            display,
            Point::new(x + 6, sy),
            Size::new(32, 7),
            mask & (1 << bit) != 0,
        )?;
    }
    for (bit, sx, sy) in [
        (1u8, x + 38, y + 7),
        (2, x + 38, y + 45),
        (4, x, y + 45),
        (5, x, y + 7),
    ] {
        draw_clock_segment(
            display,
            Point::new(sx, sy),
            Size::new(7, 31),
            mask & (1 << bit) != 0,
        )?;
    }
    Ok(())
}

fn draw_clock_segment<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D,
    origin: Point,
    size: Size,
    active: bool,
) -> Result<(), D::Error> {
    if active {
        let horizontal = size.width > size.height;
        let (outer_origin, outer_size) = if horizontal {
            (
                origin - Point::new(0, 1),
                Size::new(size.width, size.height + 2),
            )
        } else {
            (
                origin - Point::new(1, 0),
                Size::new(size.width + 2, size.height),
            )
        };
        if horizontal {
            Rectangle::new(
                outer_origin + Point::new(2, 0),
                Size::new(outer_size.width.saturating_sub(4), outer_size.height),
            )
            .into_styled(PrimitiveStyle::with_fill(INK))
            .draw(display)?;
            Rectangle::new(
                outer_origin + Point::new(0, 2),
                Size::new(outer_size.width, outer_size.height.saturating_sub(4)),
            )
            .into_styled(PrimitiveStyle::with_fill(INK))
            .draw(display)?;
        } else {
            Rectangle::new(
                outer_origin + Point::new(0, 2),
                Size::new(outer_size.width, outer_size.height.saturating_sub(4)),
            )
            .into_styled(PrimitiveStyle::with_fill(INK))
            .draw(display)?;
            Rectangle::new(
                outer_origin + Point::new(2, 0),
                Size::new(outer_size.width.saturating_sub(4), outer_size.height),
            )
            .into_styled(PrimitiveStyle::with_fill(INK))
            .draw(display)?;
        }
        // Sparse shoulder pixels soften the chamfer without animated gray.
        for corner in [
            Point::new(1, 1),
            Point::new(outer_size.width as i32 - 2, 1),
            Point::new(1, outer_size.height as i32 - 2),
            Point::new(outer_size.width as i32 - 2, outer_size.height as i32 - 2),
        ] {
            Pixel(outer_origin + corner, INK).draw(display)?;
        }
        return Ok(());
    }

    // Fixed 20% diagonal halftone: apparent gray without temporal flicker.
    for dy in 0..size.height as i32 {
        for dx in 0..size.width as i32 {
            let point = origin + Point::new(dx, dy);
            if (point.x + point.y * 2).rem_euclid(5) == 0 {
                Pixel(point, INK).draw(display)?;
            }
        }
    }
    Ok(())
}

fn render_menu<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D,
    state: &DashboardState,
    style: MonoTextStyle<'_, BinaryColor>,
) -> Result<(), D::Error> {
    let labels = ["RECORD", "FILES", "LIVE AUDIO", "DIAGNOSTICS", "SETTINGS"];
    for (index, label) in labels.iter().enumerate() {
        let y = 48 + index as i32 * 58;
        let selected = state.menu_index as usize == index;
        Rectangle::new(Point::new(12, y), Size::new(276, 44))
            .into_styled(if selected {
                PrimitiveStyle::with_fill(INK)
            } else {
                PrimitiveStyle::with_stroke(INK, 1)
            })
            .draw(display)?;
        line(
            display,
            if selected { ">" } else { " " },
            24,
            y + 15,
            if selected {
                MonoTextStyle::new(&FONT_6X10, BG)
            } else {
                style
            },
        )?;
        line(
            display,
            label,
            46,
            y + 14,
            if selected {
                MonoTextStyle::new(&FONT_9X15_BOLD, BG)
            } else {
                MonoTextStyle::new(&FONT_9X15_BOLD, INK)
            },
        )?;
    }
    Ok(())
}

fn draw_waveform<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D,
    audio: &AudioTelemetry,
    top: i32,
    height: u32,
) -> Result<(), D::Error> {
    Rectangle::new(Point::new(10, top), Size::new(280, height))
        .into_styled(PrimitiveStyle::with_stroke(INK, 1))
        .draw(display)?;
    let center = top + height as i32 / 2;
    Line::new(Point::new(14, center), Point::new(286, center))
        .into_styled(PrimitiveStyle::with_stroke(INK, 1))
        .draw(display)?;
    let scale = audio.history_peak().max(256) as u32;
    let len = audio.history_len as usize;
    let mut previous = None;
    for index in 0..len {
        if let Some(sample) = audio.history_sample(index) {
            let x = 15 + index as i32 * 270 / (AUDIO_HISTORY_CAPACITY as i32 - 1);
            let amplitude =
                ((sample as u32 * (height - 8) / 2) / scale).min((height - 8) / 2) as i32;
            let y = if index % 2 == 0 {
                center - amplitude
            } else {
                center + amplitude
            };
            let point = Point::new(x, y);
            if let Some(last) = previous {
                Line::new(last, point)
                    .into_styled(PrimitiveStyle::with_stroke(INK, 2))
                    .draw(display)?;
            }
            previous = Some(point);
        }
    }
    Ok(())
}

fn render_recorder<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D,
    state: &DashboardState,
    style: MonoTextStyle<'_, BinaryColor>,
) -> Result<(), D::Error> {
    let bold = MonoTextStyle::new(&FONT_9X15_BOLD, INK);
    line(
        display,
        if state.recording {
            "RECORDING"
        } else {
            "READY - NOT RECORDING"
        },
        14,
        48,
        bold,
    )?;
    draw_waveform(display, &state.audio, 78, 170)?;
    let mut s = String::new();
    let elapsed = if state.recording {
        state
            .runtime
            .uptime_ms
            .saturating_sub(state.recording_started_ms)
            / 1000
    } else {
        0
    };
    let _ = write!(
        s,
        "{:02}:{:02}   {} KiB   24 kHz stereo WAV",
        elapsed / 60,
        elapsed % 60,
        state.recording_bytes / 1024
    );
    line(display, &s, 14, 265, style)?;
    line(
        display,
        if state.last_recording.is_empty() {
            "No recording yet"
        } else {
            state.last_recording.as_str()
        },
        14,
        291,
        style,
    )?;
    line(
        display,
        "BOOT click: record/stop   BOOT hold: play last",
        14,
        338,
        style,
    )
}

fn render_files<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D,
    state: &DashboardState,
    style: MonoTextStyle<'_, BinaryColor>,
) -> Result<(), D::Error> {
    if state.files.is_empty() {
        line(
            display,
            "No files found. Hold BOOT in Settings to rescan.",
            14,
            58,
            style,
        )?;
        return Ok(());
    }
    let start = state.file_index.saturating_sub(4);
    for (row, entry) in state.files.iter().skip(start).take(9).enumerate() {
        let index = start + row;
        let y = 44 + row as i32 * 33;
        let selected = index == state.file_index;
        Rectangle::new(Point::new(10, y), Size::new(280, 27))
            .into_styled(if selected {
                PrimitiveStyle::with_fill(INK)
            } else {
                PrimitiveStyle::with_stroke(INK, 1)
            })
            .draw(display)?;
        let item_style = if selected {
            MonoTextStyle::new(&FONT_6X10, BG)
        } else {
            style
        };
        let mut s = String::new();
        let _ = write!(
            s,
            "{:<32} {:>5}K",
            clipped_name(&entry.name, 32),
            entry.size / 1024
        );
        line(display, &s, 17, y + 9, item_style)?;
    }
    let mut s = String::new();
    let _ = write!(
        s,
        "{} files  |  {} MiB free",
        state.files.len(),
        state.storage.free_kib / 1024
    );
    line(display, &s, 12, 352, style)
}

fn clipped_name(name: &str, columns: usize) -> String {
    if name.chars().count() <= columns {
        return name.into();
    }
    let mut clipped: String = name.chars().take(columns.saturating_sub(3)).collect();
    clipped.push_str("...");
    clipped
}

fn render_player<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D,
    state: &DashboardState,
    style: MonoTextStyle<'_, BinaryColor>,
) -> Result<(), D::Error> {
    let bold = MonoTextStyle::new(&FONT_9X15_BOLD, INK);
    line(
        display,
        if state.playing {
            "||  PLAYING"
        } else {
            ">   PAUSED"
        },
        14,
        48,
        bold,
    )?;
    line(
        display,
        &clipped_name(state.playback_name.as_str(), 44),
        14,
        72,
        style,
    )?;
    draw_waveform(display, &state.playback_audio, 94, 176)?;
    let width = if state.playback_duration_ms == 0 {
        0
    } else {
        (268u64 * state.playback_position_ms / state.playback_duration_ms).min(268) as u32
    };
    Rectangle::new(Point::new(16, 287), Size::new(268, 14))
        .into_styled(PrimitiveStyle::with_stroke(INK, 1))
        .draw(display)?;
    if width > 0 {
        Rectangle::new(Point::new(16, 287), Size::new(width, 14))
            .into_styled(PrimitiveStyle::with_fill(INK))
            .draw(display)?;
    }
    let mut s = String::new();
    let _ = write!(
        s,
        "{:02}:{:02} / {:02}:{:02}       VOL {}%",
        state.playback_position_ms / 60_000,
        state.playback_position_ms / 1_000 % 60,
        state.playback_duration_ms / 60_000,
        state.playback_duration_ms / 1_000 % 60,
        state.speaker_volume
    );
    line(display, &s, 16, 316, style)?;
    line(
        display,
        "BOOT click play/pause  |  hold stop",
        16,
        348,
        style,
    )
}

fn render_viewer<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D,
    state: &DashboardState,
    style: MonoTextStyle<'_, BinaryColor>,
) -> Result<(), D::Error> {
    let mut s = String::new();
    let _ = write!(
        s,
        "{}  {} B  @{}",
        clipped_name(&state.viewer_name, 24),
        state.viewer_size,
        state.viewer_offset
    );
    line(display, &s, 10, 43, style)?;
    Rectangle::new(Point::new(8, 58), Size::new(284, 305))
        .into_styled(PrimitiveStyle::with_stroke(INK, 1))
        .draw(display)?;
    for (index, row) in state.viewer_preview.lines().take(29).enumerate() {
        line(display, row, 13, 65 + index as i32 * 10, style)?;
    }
    Ok(())
}

fn render_live<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D,
    state: &DashboardState,
    style: MonoTextStyle<'_, BinaryColor>,
) -> Result<(), D::Error> {
    draw_waveform(display, &state.audio, 48, 240)?;
    let mut s = String::new();
    let _ = write!(
        s,
        "RMS {}   PEAK {}   {} Hz",
        state.audio.rms, state.audio.peak, state.audio.sample_rate_hz
    );
    line(
        display,
        &s,
        14,
        306,
        MonoTextStyle::new(&FONT_9X15_BOLD, INK),
    )?;
    line(
        display,
        "Visualization only. Audio is not saved.",
        14,
        335,
        style,
    )?;
    line(display, "Hold BOOT to open the recorder.", 14, 352, style)
}

fn render_diagnostics<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D,
    state: &DashboardState,
    style: MonoTextStyle<'_, BinaryColor>,
) -> Result<(), D::Error> {
    let mut s = String::new();
    let _ = write!(
        s,
        "NET {}  ENV {}  MIC {}  SD {}",
        state.wifi.health.label(),
        state.environment.health.label(),
        state.audio.health.label(),
        state.storage.health.label()
    );
    line(display, &s, 12, 44, style)?;
    s.clear();
    let _ = write!(
        s,
        "HEAP {}K  MIN {}K",
        state.runtime.free_heap / 1024,
        state.runtime.min_free_heap / 1024,
    );
    line(display, &s, 12, 62, style)?;
    s.clear();
    let _ = write!(
        s,
        "PSRAM {}K  LOOP {}Hz  LCD {}ms",
        state.runtime.free_psram / 1024,
        state.runtime.loop_hz,
        state.runtime.display_flush_ms
    );
    line(display, &s, 12, 78, style)?;
    Line::new(Point::new(10, 98), Point::new(290, 98))
        .into_styled(PrimitiveStyle::with_stroke(INK, 1))
        .draw(display)?;
    let available = state.events.len().saturating_sub(state.event_offset);
    let skip = available.saturating_sub(12);
    for (index, event) in state.events.iter().skip(skip).take(12).enumerate() {
        s.clear();
        let _ = write!(s, "{:>6} {:<6} {}", event.at_ms, event.source, event.detail);
        line(display, &s, 12, 108 + index as i32 * 20, style)?;
    }
    Ok(())
}

fn render_system_settings<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D,
    state: &DashboardState,
    style: MonoTextStyle<'_, BinaryColor>,
) -> Result<(), D::Error> {
    let mut volume = String::new();
    let _ = write!(volume, "Speaker volume: {}%", state.speaker_volume);
    let labels = [
        "Refresh display",
        volume.as_str(),
        "Test speaker tone",
        if state.event_logging_enabled {
            "Event log: ON"
        } else {
            "Event log: OFF"
        },
        "Rescan SD files",
        if state.poweroff_prepared {
            "Power off: READY"
        } else {
            "Prepare power off"
        },
    ];
    for (index, label) in labels.iter().enumerate() {
        let y = 44 + index as i32 * 50;
        let selected = state.settings_index as usize == index;
        Rectangle::new(Point::new(10, y), Size::new(280, 38))
            .into_styled(if selected {
                PrimitiveStyle::with_fill(INK)
            } else {
                PrimitiveStyle::with_stroke(INK, 1)
            })
            .draw(display)?;
        line(
            display,
            label,
            20,
            y + 14,
            if selected {
                MonoTextStyle::new(&FONT_6X10, BG)
            } else {
                style
            },
        )?;
    }
    line(
        display,
        "Event logging and WAV recording are opt-in.",
        12,
        353,
        style,
    )
}

fn line<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D,
    text: &str,
    x: i32,
    y: i32,
    style: MonoTextStyle<'_, BinaryColor>,
) -> Result<(), D::Error> {
    Text::with_baseline(text, Point::new(x, y), style, Baseline::Top)
        .draw(display)
        .map(|_| ())
}

fn draw_wifi<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D,
    x: i32,
    y: i32,
    rssi: i16,
    health: Health,
) -> Result<(), D::Error> {
    let bars = if health != Health::Ok {
        0
    } else if rssi >= -55 {
        4
    } else if rssi >= -65 {
        3
    } else if rssi >= -75 {
        2
    } else {
        1
    };
    for index in 0..4 {
        let height = 3 + index * 3;
        Rectangle::new(
            Point::new(x + index as i32 * 6, y + 12 - height as i32),
            Size::new(4, height),
        )
        .into_styled(if index < bars {
            PrimitiveStyle::with_fill(INK)
        } else {
            PrimitiveStyle::with_stroke(INK, 1)
        })
        .draw(display)?;
    }
    Ok(())
}

fn draw_battery<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D,
    x: i32,
    y: i32,
    percent: u8,
) -> Result<(), D::Error> {
    Rectangle::new(Point::new(x, y), Size::new(30, 12))
        .into_styled(PrimitiveStyle::with_stroke(INK, 1))
        .draw(display)?;
    Rectangle::new(Point::new(x + 30, y + 3), Size::new(3, 6))
        .into_styled(PrimitiveStyle::with_fill(INK))
        .draw(display)?;
    let width = ((percent.min(100) as u32 * 26) / 100).max(1);
    Rectangle::new(Point::new(x + 2, y + 2), Size::new(width, 8))
        .into_styled(PrimitiveStyle::with_fill(INK))
        .draw(display)?;
    Ok(())
}

fn weekday_name(day: u8) -> &'static str {
    ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"]
        .get(day as usize)
        .copied()
        .unwrap_or("---")
}

fn month_name(month: u8) -> &'static str {
    [
        "---", "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
    ]
    .get(month as usize)
    .copied()
    .unwrap_or("---")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_is_emitted_after_debounced_release() {
        let mut engine = ButtonEngine::default();
        assert_eq!(engine.sample(Button::Key, true, 10), None);
        assert_eq!(engine.sample(Button::Key, true, 50), None);
        assert_eq!(engine.sample(Button::Key, false, 100), None);
        let event = engine.sample(Button::Key, false, 140).unwrap();
        assert_eq!(event.gesture, Gesture::Click);
        assert_eq!(event.held_ms, 90);
    }

    #[test]
    fn long_press_is_emitted_once_and_suppresses_click() {
        let mut engine = ButtonEngine::default();
        engine.sample(Button::Boot, true, 0);
        engine.sample(Button::Boot, true, 40);
        let event = engine.sample(Button::Boot, true, 700).unwrap();
        assert_eq!(event.gesture, Gesture::LongPress);
        assert_eq!(engine.sample(Button::Boot, true, 900), None);
        engine.sample(Button::Boot, false, 1_000);
        assert_eq!(engine.sample(Button::Boot, false, 1_040), None);
    }

    #[test]
    fn event_buffer_is_bounded() {
        let mut state = DashboardState::default();
        for i in 0..(EVENT_CAPACITY + 5) {
            state.record(i as u64, "TEST", "sample");
        }
        assert_eq!(state.events.len(), EVENT_CAPACITY);
        assert_eq!(state.runtime.dropped_events, 5);
        assert_eq!(state.events.front().unwrap().seq, 6);
    }

    #[test]
    fn all_four_runtime_gestures_have_safe_actions() {
        let mut state = DashboardState::default();
        for button in Button::ALL {
            for gesture in [Gesture::Click, Gesture::LongPress] {
                state.apply_input(
                    InputEvent {
                        button,
                        gesture,
                        held_ms: 700,
                    },
                    1_000,
                );
            }
        }
        assert_eq!(state.events.len(), 4);
        assert_eq!(state.view, View::Home);
        assert!(!state.prepare_poweroff_requested);
    }

    #[test]
    fn runtime_navigation_matches_physical_left_and_right_buttons() {
        let mut state = DashboardState::default();
        state.apply_input(
            InputEvent {
                button: Button::Key,
                gesture: Gesture::Click,
                held_ms: 90,
            },
            100,
        );
        assert_eq!(state.view, View::Home);

        state.apply_input(
            InputEvent {
                button: Button::Boot,
                gesture: Gesture::Click,
                held_ms: 90,
            },
            200,
        );
        assert_eq!(state.view, View::Menu);

        state.view = View::Player;
        state.apply_input(
            InputEvent {
                button: Button::Key,
                gesture: Gesture::Click,
                held_ms: 90,
            },
            300,
        );
        assert_eq!(state.view, View::Files);
        assert_eq!(state.take_action(), Some(UiAction::StopPlayback));
    }

    #[test]
    fn poweroff_preparation_is_explicit_and_never_enters_sleep() {
        let mut state = DashboardState::default();
        state.view = View::Settings;
        state.settings_index = 5;
        state.apply_input(
            InputEvent {
                button: Button::Boot,
                gesture: Gesture::LongPress,
                held_ms: 700,
            },
            100,
        );
        assert_eq!(state.take_action(), Some(UiAction::PreparePoweroff));
    }

    #[test]
    fn audio_history_scrolls_and_stays_bounded() {
        let mut audio = AudioTelemetry::default();
        for level in 0..(AUDIO_HISTORY_CAPACITY as u16 + 5) {
            audio.push_level(level);
        }
        assert_eq!(audio.history_len as usize, AUDIO_HISTORY_CAPACITY);
        assert_eq!(audio.history_sample(0), Some(5));
        assert_eq!(
            audio.history_sample(AUDIO_HISTORY_CAPACITY - 1),
            Some(AUDIO_HISTORY_CAPACITY as u16 + 4)
        );
    }

    #[test]
    fn persistence_is_opt_in_and_menu_entry_is_predictable() {
        let mut state = DashboardState::default();
        assert!(!state.recording);
        assert!(!state.event_logging_enabled);
        state.menu_index = 4;
        state.apply_input(
            InputEvent {
                button: Button::Boot,
                gesture: Gesture::Click,
                held_ms: 100,
            },
            10,
        );
        assert_eq!(state.view, View::Menu);
        assert_eq!(state.menu_index, 0);
    }

    #[test]
    fn key_moves_up_exactly_one_level_and_hold_returns_home() {
        let mut state = DashboardState::default();
        state.view = View::Player;
        state.apply_input(
            InputEvent {
                button: Button::Key,
                gesture: Gesture::Click,
                held_ms: 100,
            },
            10,
        );
        assert_eq!(state.view, View::Files);
        state.apply_input(
            InputEvent {
                button: Button::Key,
                gesture: Gesture::Click,
                held_ms: 100,
            },
            20,
        );
        assert_eq!(state.view, View::Menu);
        state.apply_input(
            InputEvent {
                button: Button::Key,
                gesture: Gesture::LongPress,
                held_ms: 700,
            },
            30,
        );
        assert_eq!(state.view, View::Home);
    }

    #[test]
    fn leaving_recorder_stops_an_active_recording() {
        let mut state = DashboardState::default();
        state.view = View::Recorder;
        state.recording = true;
        state.apply_input(
            InputEvent {
                button: Button::Key,
                gesture: Gesture::Click,
                held_ms: 100,
            },
            10,
        );
        assert_eq!(state.view, View::Menu);
        assert_eq!(state.take_action(), Some(UiAction::ToggleRecording));
    }

    #[test]
    fn a_new_input_invalidates_poweroff_readiness_and_stale_actions() {
        let mut state = DashboardState::default();
        state.view = View::Settings;
        state.settings_index = 5;
        state.apply_input(
            InputEvent {
                button: Button::Boot,
                gesture: Gesture::LongPress,
                held_ms: 700,
            },
            10,
        );
        assert_eq!(state.take_action(), Some(UiAction::PreparePoweroff));
        state.poweroff_prepared = true;

        state.apply_input(
            InputEvent {
                button: Button::Boot,
                gesture: Gesture::Click,
                held_ms: 100,
            },
            20,
        );
        assert!(!state.poweroff_prepared);
        assert_eq!(state.take_action(), None);
    }
}
