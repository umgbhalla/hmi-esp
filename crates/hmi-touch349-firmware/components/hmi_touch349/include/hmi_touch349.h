#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define HMI_TOUCH349_WIDTH 172
#define HMI_TOUCH349_HEIGHT 640
#define HMI_TOUCH349_PIXELS (HMI_TOUCH349_WIDTH * HMI_TOUCH349_HEIGHT)
#define HMI_TOUCH349_FRAME_BYTES (HMI_TOUCH349_PIXELS * 2)
#define HMI_TOUCH349_BAND_ROWS 64
#define HMI_TOUCH349_BAND_PIXELS (HMI_TOUCH349_WIDTH * HMI_TOUCH349_BAND_ROWS)

typedef struct {
    uint32_t flush_us;
    uint32_t dma_wait_us;
    uint16_t bands;
} hmi_touch349_flush_stats_t;

typedef struct {
    uint64_t capacity_bytes;
    uint32_t sector_size;
    uint8_t mounted;
} hmi_touch349_sd_stats_t;

int hmi_touch349_init(void);
uint16_t *hmi_touch349_framebuffer(size_t *pixel_count);
int hmi_touch349_flush_full(hmi_touch349_flush_stats_t *stats);
int hmi_touch349_backlight_set(uint8_t duty, bool enabled);
int hmi_touch349_sd_mount(hmi_touch349_sd_stats_t *stats);
