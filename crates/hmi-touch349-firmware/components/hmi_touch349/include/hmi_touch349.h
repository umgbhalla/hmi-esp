#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define HMI_TOUCH349_WIDTH 172
#define HMI_TOUCH349_HEIGHT 640
#define HMI_TOUCH349_PIXELS (HMI_TOUCH349_WIDTH * HMI_TOUCH349_HEIGHT)
#define HMI_TOUCH349_FRAME_BYTES (HMI_TOUCH349_PIXELS * 2)
#define HMI_TOUCH349_BAND_ROWS 128
#define HMI_TOUCH349_BAND_PIXELS (HMI_TOUCH349_WIDTH * HMI_TOUCH349_BAND_ROWS)

typedef struct {
    uint32_t flush_us;
    uint32_t dma_wait_us;
    uint16_t bands;
} hmi_touch349_flush_stats_t;

typedef struct {
    uint64_t capacity_bytes;
    uint64_t free_bytes;
    uint32_t sector_size;
    uint8_t mounted;
} hmi_touch349_sd_stats_t;

typedef struct {
    uint32_t ipv4;
    uint32_t reconnects;
    int8_t rssi_dbm;
    uint8_t last_disconnect_reason;
    uint8_t target_visible;
    uint8_t target_channel;
    uint8_t connected;
    uint8_t time_synced;
} hmi_touch349_network_stats_t;

typedef struct {
    uint64_t bytes_written;
    uint32_t rms;
    uint32_t peak;
    int32_t last_error;
    uint8_t ready;
    uint8_t recording;
} hmi_touch349_recorder_stats_t;

int hmi_touch349_init(void);
uint16_t *hmi_touch349_framebuffer(size_t *pixel_count);
int hmi_touch349_flush_full(hmi_touch349_flush_stats_t *stats);
int hmi_touch349_backlight_set(uint8_t duty, bool enabled);
int hmi_touch349_sd_mount(hmi_touch349_sd_stats_t *stats);
int hmi_touch349_touch_read(uint8_t response[32]);
int hmi_touch349_network_start(const char *ssid, const char *password);
int hmi_touch349_network_scan(void);
int hmi_touch349_network_stats(hmi_touch349_network_stats_t *stats);
int hmi_touch349_recorder_start(const char *filename);
int hmi_touch349_recorder_stop(void);
int hmi_touch349_recorder_stats(hmi_touch349_recorder_stats_t *stats);
int hmi_touch349_audio_read_levels(uint32_t *rms, uint32_t *peak);
int hmi_touch349_audio_write(const int16_t *samples, size_t sample_count);
int hmi_touch349_audio_volume_set(uint8_t percent);
int hmi_touch349_audio_output_ready(void);
int hmi_touch349_audio_pending(void);
int hmi_touch349_audio_stop(uint32_t *dropped_samples);
int hmi_touch349_console_read(uint8_t *buffer, size_t capacity);
int hmi_touch349_battery_read(uint16_t *raw, uint32_t *millivolts);
uint32_t hmi_touch349_free_heap(void);
uint32_t hmi_touch349_free_psram(void);
bool hmi_touch349_power_button_pressed(void);
void hmi_touch349_power_off(void);
