#[cfg(not(target_os = "espidf"))]
fn main() {
    println!("Touch349 minimal firmware targets ESP32-S3");
}

#[cfg(target_os = "espidf")]
fn main() -> anyhow::Result<()> {
    use std::{slice, thread, time::Duration};

    use anyhow::ensure;
    use esp_idf_sys::hmi_touch349::{
        hmi_touch349_backlight_set, hmi_touch349_flush_full, hmi_touch349_flush_stats_t,
        hmi_touch349_framebuffer, hmi_touch349_init, hmi_touch349_sd_mount,
        hmi_touch349_sd_stats_t,
    };

    const WIDTH: usize = 172;
    const HEIGHT: usize = 640;
    const PIXELS: usize = WIDTH * HEIGHT;

    esp_idf_sys::link_patches();
    println!("MINIMAL BOOT Touch349 V2");
    let init_result = unsafe { hmi_touch349_init() };
    ensure!(init_result == 0, "display init failed: {init_result}");

    let mut pixel_count = 0usize;
    let pointer = unsafe { hmi_touch349_framebuffer(&mut pixel_count) };
    ensure!(!pointer.is_null(), "framebuffer pointer is null");
    ensure!(pixel_count == PIXELS, "framebuffer length is {pixel_count}");
    let framebuffer = unsafe { slice::from_raw_parts_mut(pointer, pixel_count) };

    // Static black/white checkerboard with a solid border. Both colors are
    // invariant under RGB565 byte swapping, making this a transport test rather
    // than a color-order test.
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let border = x < 8 || x >= WIDTH - 8 || y < 8 || y >= HEIGHT - 8;
            let checker_white = ((x / 32) + (y / 32)) % 2 == 0;
            framebuffer[y * WIDTH + x] = if border || !checker_white {
                0x0000
            } else {
                0xffff
            };
        }
    }

    let mut hash = 2_166_136_261u32;
    let mut white_pixels = 0usize;
    for pixel in framebuffer.iter().copied() {
        hash ^= u32::from(pixel);
        hash = hash.wrapping_mul(16_777_619);
        white_pixels += usize::from(pixel == 0xffff);
    }

    let mut stats = hmi_touch349_flush_stats_t {
        flush_us: 0,
        dma_wait_us: 0,
        bands: 0,
    };
    let flush_result = unsafe { hmi_touch349_flush_full(&mut stats) };
    ensure!(flush_result == 0, "frame flush failed: {flush_result}");
    let backlight_result = unsafe { hmi_touch349_backlight_set(0, true) };
    ensure!(
        backlight_result == 0,
        "backlight enable failed: {backlight_result}"
    );

    println!(
        "MINIMAL READY pattern=checkerboard hash={hash:08x} white_pixels={white_pixels} flush_us={} dma_wait_us={} bands={}",
        stats.flush_us, stats.dma_wait_us, stats.bands
    );

    // SD failure is deliberately non-fatal: display recovery must remain visible
    // even when no card is inserted or the filesystem cannot be mounted.
    let mut sd = hmi_touch349_sd_stats_t {
        capacity_bytes: 0,
        sector_size: 0,
        mounted: 0,
    };
    let sd_result = unsafe { hmi_touch349_sd_mount(&mut sd) };
    if sd_result == 0 {
        println!(
            "SD READY mount=/sdcard capacity_bytes={} sector_size={}",
            sd.capacity_bytes, sd.sector_size
        );
    } else {
        println!("SD UNAVAILABLE error={sd_result}");
    }

    let mut heartbeat = 0u64;
    loop {
        heartbeat = heartbeat.wrapping_add(1);
        println!(
            "MINIMAL HEARTBEAT {heartbeat} sd_mounted={} sd_error={sd_result}",
            sd.mounted
        );
        thread::sleep(Duration::from_secs(2));
    }
}
