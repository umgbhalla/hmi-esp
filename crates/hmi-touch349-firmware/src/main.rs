#[cfg(not(target_os = "espidf"))]
fn main() {
    println!("Touch349 safe-mode firmware targets ESP32-S3");
}

#[cfg(target_os = "espidf")]
mod firmware {
    use std::{
        convert::Infallible,
        format, slice, thread,
        time::{Duration, Instant},
    };

    use anyhow::ensure;
    use embedded_graphics::{
        mono_font::{
            ascii::{FONT_6X10, FONT_9X15_BOLD},
            MonoTextStyle,
        },
        pixelcolor::Rgb565,
        prelude::*,
        primitives::{PrimitiveStyle, Rectangle},
        text::{Baseline, Text},
    };
    use esp_idf_sys::hmi_touch349::{
        hmi_touch349_backlight_set, hmi_touch349_flush_full, hmi_touch349_flush_stats_t,
        hmi_touch349_framebuffer, hmi_touch349_init, hmi_touch349_power_button_pressed,
        hmi_touch349_power_off, hmi_touch349_sd_mount, hmi_touch349_sd_stats_t,
    };

    const WIDTH: usize = 172;
    const HEIGHT: usize = 640;
    const PIXELS: usize = WIDTH * HEIGHT;
    const POWER_HOLD: Duration = Duration::from_millis(1_200);

    const BG: Rgb565 = Rgb565::new(1, 3, 6);
    const SURFACE: Rgb565 = Rgb565::new(3, 9, 14);
    const SURFACE_ALT: Rgb565 = Rgb565::new(5, 14, 20);
    const TEXT: Rgb565 = Rgb565::new(29, 60, 30);
    const MUTED: Rgb565 = Rgb565::new(15, 35, 21);
    const ACCENT: Rgb565 = Rgb565::new(2, 49, 29);
    const OK: Rgb565 = Rgb565::new(7, 52, 19);
    const WARN: Rgb565 = Rgb565::new(31, 40, 4);
    const OFF: Rgb565 = Rgb565::new(29, 11, 8);

    struct Frame<'a> {
        pixels: &'a mut [u16],
    }

    impl OriginDimensions for Frame<'_> {
        fn size(&self) -> Size {
            Size::new(WIDTH as u32, HEIGHT as u32)
        }
    }

    impl DrawTarget for Frame<'_> {
        type Color = Rgb565;
        type Error = Infallible;

        fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
        where
            I: IntoIterator<Item = Pixel<Self::Color>>,
        {
            for Pixel(point, color) in pixels {
                if point.x >= 0 && point.y >= 0 && point.x < WIDTH as i32 && point.y < HEIGHT as i32
                {
                    self.pixels[point.y as usize * WIDTH + point.x as usize] = color.into_storage();
                }
            }
            Ok(())
        }

        fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
            self.pixels.fill(color.into_storage());
            Ok(())
        }
    }

    #[derive(Clone, Copy, Eq, PartialEq)]
    enum PowerState {
        Ready,
        Holding(u8),
        PoweringOff,
    }

    fn text<D: DrawTarget<Color = Rgb565>>(
        display: &mut D,
        value: &str,
        x: i32,
        y: i32,
        color: Rgb565,
        large: bool,
    ) -> Result<(), D::Error> {
        let style = if large {
            MonoTextStyle::new(&FONT_9X15_BOLD, color)
        } else {
            MonoTextStyle::new(&FONT_6X10, color)
        };
        Text::with_baseline(value, Point::new(x, y), style, Baseline::Top)
            .draw(display)
            .map(|_| ())
    }

    fn card<D: DrawTarget<Color = Rgb565>>(
        display: &mut D,
        y: i32,
        label: &str,
        value: &str,
        color: Rgb565,
    ) -> Result<(), D::Error> {
        Rectangle::new(Point::new(8, y), Size::new(156, 68))
            .into_styled(PrimitiveStyle::with_fill(SURFACE))
            .draw(display)?;
        Rectangle::new(Point::new(8, y), Size::new(5, 68))
            .into_styled(PrimitiveStyle::with_fill(color))
            .draw(display)?;
        text(display, label, 23, y + 11, MUTED, false)?;
        text(display, value, 23, y + 34, TEXT, true)
    }

    fn render(
        display: &mut Frame<'_>,
        sd: &hmi_touch349_sd_stats_t,
        state: PowerState,
    ) -> Result<(), Infallible> {
        display.clear(BG)?;
        Rectangle::new(Point::zero(), Size::new(172, 58))
            .into_styled(PrimitiveStyle::with_fill(SURFACE))
            .draw(display)?;
        text(display, "ZONKO HMI", 8, 8, ACCENT, true)?;
        text(
            display,
            if state == PowerState::PoweringOff {
                "POWERING OFF"
            } else {
                "DEVICE READY"
            },
            8,
            34,
            TEXT,
            false,
        )?;

        card(display, 74, "DISPLAY", "ONLINE", OK)?;
        card(
            display,
            154,
            "SD CARD",
            if sd.mounted == 1 {
                "MOUNTED"
            } else {
                "NOT FOUND"
            },
            if sd.mounted == 1 { OK } else { WARN },
        )?;
        let storage = if sd.mounted == 1 {
            let whole = sd.capacity_bytes / 1_000_000_000;
            let tenth = (sd.capacity_bytes % 1_000_000_000) / 100_000_000;
            format!("{whole}.{tenth} GB")
        } else {
            "NO CARD".into()
        };
        card(
            display,
            234,
            "STORAGE",
            &storage,
            if sd.mounted == 1 { OK } else { WARN },
        )?;

        Rectangle::new(Point::new(8, 326), Size::new(156, 176))
            .into_styled(PrimitiveStyle::with_fill(SURFACE_ALT))
            .draw(display)?;
        text(display, "POWER", 18, 342, MUTED, false)?;
        let (message, progress, color) = match state {
            PowerState::Ready => ("HOLD PWR 1.2 SEC", 0, ACCENT),
            PowerState::Holding(progress) => ("KEEP HOLDING", progress, WARN),
            PowerState::PoweringOff => ("SHUTTING DOWN", 100, OFF),
        };
        Text::with_baseline(
            message,
            Point::new(18, 378),
            MonoTextStyle::new(&FONT_9X15_BOLD, TEXT),
            Baseline::Top,
        )
        .draw(display)?;
        Rectangle::new(Point::new(18, 424), Size::new(136, 14))
            .into_styled(PrimitiveStyle::with_fill(SURFACE))
            .draw(display)?;
        if progress > 0 {
            Rectangle::new(
                Point::new(18, 424),
                Size::new(136 * u32::from(progress) / 100, 14),
            )
            .into_styled(PrimitiveStyle::with_fill(color))
            .draw(display)?;
        }
        text(display, "SCREEN AND SYSTEM", 18, 458, MUTED, false)?;
        text(display, "WILL TURN OFF", 18, 476, MUTED, false)?;

        Rectangle::new(Point::new(0, 568), Size::new(172, 72))
            .into_styled(PrimitiveStyle::with_fill(SURFACE))
            .draw(display)?;
        text(display, "SAFE MODE", 58, 586, ACCENT, true)?;
        text(display, "LCD + SD + POWER", 34, 612, MUTED, false)
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

    pub fn run() -> anyhow::Result<()> {
        esp_idf_sys::link_patches();
        println!("SAFE MODE BOOT Touch349 V2");
        let init_result = unsafe { hmi_touch349_init() };
        ensure!(init_result == 0, "display init failed: {init_result}");

        let mut pixel_count = 0usize;
        let pointer = unsafe { hmi_touch349_framebuffer(&mut pixel_count) };
        ensure!(!pointer.is_null(), "framebuffer pointer is null");
        ensure!(pixel_count == PIXELS, "framebuffer length is {pixel_count}");
        let pixels = unsafe { slice::from_raw_parts_mut(pointer, pixel_count) };
        let mut display = Frame { pixels };

        let mut sd = hmi_touch349_sd_stats_t {
            capacity_bytes: 0,
            sector_size: 0,
            mounted: 0,
        };
        let sd_result = unsafe { hmi_touch349_sd_mount(&mut sd) };
        render(&mut display, &sd, PowerState::Ready)?;
        let stats = flush()?;
        let backlight_result = unsafe { hmi_touch349_backlight_set(0, true) };
        ensure!(
            backlight_result == 0,
            "backlight enable failed: {backlight_result}"
        );
        println!(
            "SAFE MODE READY sd_mounted={} sd_error={sd_result} capacity_bytes={} flush_us={}",
            sd.mounted, sd.capacity_bytes, stats.flush_us
        );

        let mut pressed_since: Option<Instant> = None;
        let mut rendered_progress = 0u8;
        let mut heartbeat = Instant::now();
        loop {
            let pressed = unsafe { hmi_touch349_power_button_pressed() };
            if pressed {
                let started = *pressed_since.get_or_insert_with(Instant::now);
                let held = started.elapsed();
                let progress = ((held.as_millis() * 100 / POWER_HOLD.as_millis()).min(100)) as u8;
                let progress_step = progress / 10 * 10;
                if progress_step != rendered_progress {
                    rendered_progress = progress_step;
                    render(&mut display, &sd, PowerState::Holding(progress_step))?;
                    flush()?;
                    println!("POWER HOLD progress={progress_step}%");
                }
                if held >= POWER_HOLD {
                    render(&mut display, &sd, PowerState::PoweringOff)?;
                    flush()?;
                    println!("POWER OFF threshold reached; releasing SYS_EN");
                    thread::sleep(Duration::from_millis(250));
                    unsafe { hmi_touch349_power_off() };
                    unreachable!("deep sleep returned");
                }
            } else if pressed_since.take().is_some() {
                rendered_progress = 0;
                render(&mut display, &sd, PowerState::Ready)?;
                flush()?;
                println!("POWER HOLD cancelled");
            }

            if heartbeat.elapsed() >= Duration::from_secs(2) {
                println!(
                    "SAFE MODE HEARTBEAT sd_mounted={} pwr_pressed={pressed}",
                    sd.mounted
                );
                heartbeat = Instant::now();
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(target_os = "espidf")]
fn main() -> anyhow::Result<()> {
    firmware::run()
}
