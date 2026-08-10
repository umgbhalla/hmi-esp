#pragma once

#include <stddef.h>
#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

enum {
    HMI_BOARD_AUDIO_READY = 1u << 0,
    HMI_BOARD_ENV_READY = 1u << 1,
    HMI_BOARD_BATTERY_READY = 1u << 2,
    HMI_BOARD_SD_READY = 1u << 3,
};

uint32_t hmi_board_init(void);
int hmi_board_env_read(int32_t *temperature_centi_c, uint32_t *humidity_centi_pct);
int hmi_board_battery_read(uint32_t *millivolts, uint16_t *raw);
int hmi_board_audio_read(int16_t *samples, size_t sample_count);
int hmi_board_audio_write(const int16_t *samples, size_t sample_count);
int hmi_board_audio_set_volume(uint8_t percent);
int hmi_board_te_init(void);
bool hmi_board_te_take_rising_edge(void);
int hmi_board_sd_stats(uint64_t *total_kib, uint64_t *free_kib);
int hmi_board_sd_append(const uint8_t *data, size_t data_len);
int hmi_board_time_init(const char *timezone);
int hmi_board_time_read(int32_t *year, uint8_t *month, uint8_t *day,
                        uint8_t *weekday, uint8_t *hour, uint8_t *minute,
                        uint8_t *second);

#ifdef __cplusplus
}
#endif
