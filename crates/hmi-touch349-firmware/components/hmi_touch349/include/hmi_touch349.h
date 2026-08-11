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
    uint16_t failures;
} hmi_touch349_flush_stats_t;

int hmi_touch349_init(void);
uint16_t *hmi_touch349_framebuffer(size_t *pixel_count);
int hmi_touch349_flush_full(hmi_touch349_flush_stats_t *stats);
int hmi_touch349_backlight_set(uint8_t duty, bool enabled);
int hmi_touch349_touch_read(uint16_t *x, uint16_t *y, bool *pressed);
int hmi_touch349_time_init(const char *timezone);
int hmi_touch349_time_read(int32_t *year, uint8_t *month, uint8_t *day,
                           uint8_t *weekday, uint8_t *hour, uint8_t *minute,
                           uint8_t *second);
