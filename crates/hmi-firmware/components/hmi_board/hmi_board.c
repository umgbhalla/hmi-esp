#include "hmi_board.h"

#include <errno.h>
#include <stdbool.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "codec_board.h"
#include "codec_init.h"
#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "driver/sdmmc_host.h"
#include "esp_adc/adc_cali.h"
#include "esp_adc/adc_cali_scheme.h"
#include "esp_adc/adc_oneshot.h"
#include "esp_codec_dev.h"
#include "esp_err.h"
#include "esp_log.h"
#include "esp_sntp.h"
#include "esp_vfs_fat.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"
#include "sdmmc_cmd.h"

static const char *TAG = "hmi_board";
static esp_codec_dev_handle_t s_record;
static esp_codec_dev_handle_t s_playback;
static i2c_master_bus_handle_t s_i2c_bus;
static i2c_master_dev_handle_t s_shtc3;
static adc_oneshot_unit_handle_t s_adc;
static adc_cali_handle_t s_adc_cali;
static sdmmc_card_t *s_sdcard;
static bool s_time_started;
static atomic_bool s_te_rising_edge;

enum { AUDIO_CHUNK_SAMPLES = 512, AUDIO_QUEUE_DEPTH = 8 };

typedef struct {
    size_t sample_count;
    int16_t samples[AUDIO_CHUNK_SAMPLES];
} audio_chunk_t;

static QueueHandle_t s_capture_queue;
static QueueHandle_t s_playback_queue;

static void audio_io_task(void *argument)
{
    (void)argument;
    audio_chunk_t capture = { .sample_count = AUDIO_CHUNK_SAMPLES };
    audio_chunk_t playback;
    audio_chunk_t discarded;
    for (;;) {
        int result = esp_codec_dev_read(
            s_record,
            capture.samples,
            sizeof(capture.samples));
        if (result == ESP_CODEC_DEV_OK) {
            if (xQueueSend(s_capture_queue, &capture, 0) != pdTRUE) {
                xQueueReceive(s_capture_queue, &discarded, 0);
                xQueueSend(s_capture_queue, &capture, 0);
            }
        } else {
            vTaskDelay(pdMS_TO_TICKS(1));
        }

        if (xQueueReceive(s_playback_queue, &playback, 0) == pdTRUE) {
            result = esp_codec_dev_write(
                s_playback,
                playback.samples,
                (int)(playback.sample_count * sizeof(int16_t)));
            if (result != ESP_CODEC_DEV_OK) {
                ESP_LOGW(TAG, "speaker queue write failed: %d", result);
            }
        }
    }
}

static void IRAM_ATTR te_rising_isr(void *argument)
{
    (void)argument;
    atomic_store_explicit(&s_te_rising_edge, true, memory_order_relaxed);
}

// Waveshare's board example compensates for the SHTC3's placement inside the
// enclosure, where PCB heat makes the raw temperature read about 4 C high.
static const int32_t SHTC3_BOARD_HEAT_OFFSET_CENTI_C = 400;

static uint8_t shtc3_crc(const uint8_t *data, size_t len)
{
    uint8_t crc = 0xff;
    for (size_t i = 0; i < len; ++i) {
        crc ^= data[i];
        for (int bit = 0; bit < 8; ++bit) {
            crc = (crc & 0x80) ? (uint8_t)((crc << 1) ^ 0x31) : (uint8_t)(crc << 1);
        }
    }
    return crc;
}

static esp_err_t shtc3_write_command(uint16_t command)
{
    const uint8_t bytes[] = { (uint8_t)(command >> 8), (uint8_t)(command & 0xff) };
    return i2c_master_transmit(s_shtc3, bytes, sizeof(bytes), 1000);
}

static esp_err_t shtc3_measure_once(uint8_t data[6])
{
    esp_err_t result = shtc3_write_command(0x7866);
    if (result != ESP_OK) return result;
    vTaskDelay(pdMS_TO_TICKS(20));
    return i2c_master_receive(s_shtc3, data, 6, 1000);
}

static bool init_audio_and_i2c(void)
{
    set_codec_board_type("S3_RLCD_4_2");
    codec_init_cfg_t cfg = {
        .in_mode = CODEC_I2S_MODE_TDM,
        .out_mode = CODEC_I2S_MODE_TDM,
        .in_use_tdm = false,
        .reuse_dev = false,
    };
    if (init_codec(&cfg) != 0) {
        ESP_LOGE(TAG, "codec init failed");
        return false;
    }
    s_record = get_record_handle();
    s_playback = get_playback_handle();
    if (!s_record || !s_playback) {
        ESP_LOGE(TAG, "audio handle missing (record=%p playback=%p)", s_record, s_playback);
        return false;
    }
    esp_codec_dev_sample_info_t sample = {
        .sample_rate = 24000,
        .channel = 2,
        .bits_per_sample = 16,
    };
    if (esp_codec_dev_open(s_record, &sample) != ESP_CODEC_DEV_OK) {
        ESP_LOGE(TAG, "microphone open failed");
        s_record = NULL;
        return false;
    }
    if (esp_codec_dev_open(s_playback, &sample) != ESP_CODEC_DEV_OK) {
        ESP_LOGE(TAG, "speaker open failed");
        s_playback = NULL;
        return false;
    }
    if (esp_codec_dev_set_in_gain(s_record, 25.0f) != ESP_CODEC_DEV_OK
        || esp_codec_dev_set_out_vol(s_playback, 55) != ESP_CODEC_DEV_OK) {
        ESP_LOGE(TAG, "initial codec gain/volume setup failed");
        return false;
    }
    s_capture_queue = xQueueCreate(AUDIO_QUEUE_DEPTH, sizeof(audio_chunk_t));
    s_playback_queue = xQueueCreate(AUDIO_QUEUE_DEPTH, sizeof(audio_chunk_t));
    if (!s_capture_queue || !s_playback_queue) {
        ESP_LOGE(TAG, "audio queue allocation failed");
        return false;
    }
    if (xTaskCreatePinnedToCore(
            audio_io_task,
            "hmi_audio",
            6144,
            NULL,
            8,
            NULL,
            1) != pdPASS) {
        ESP_LOGE(TAG, "audio task creation failed");
        return false;
    }
    return true;
}

static bool init_shtc3(void)
{
    s_i2c_bus = (i2c_master_bus_handle_t)get_i2c_bus_handle(0);
    if (!s_i2c_bus) {
        if (init_i2c(0) != 0) return false;
        s_i2c_bus = (i2c_master_bus_handle_t)get_i2c_bus_handle(0);
    }
    if (!s_i2c_bus) return false;

    i2c_device_config_t config = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address = 0x70,
        .scl_speed_hz = 400000,
    };
    if (i2c_master_bus_add_device(s_i2c_bus, &config, &s_shtc3) != ESP_OK) return false;

    // A CPU reset does not power-cycle the sensor. Recover the shared bus and
    // explicitly wake/reset SHTC3 so stale sleep/transaction state cannot leak
    // across firmware flashes or watchdog resets.
    esp_err_t result = i2c_master_bus_reset(s_i2c_bus);
    if (result != ESP_OK) ESP_LOGW(TAG, "SHTC3 bus reset: %s", esp_err_to_name(result));
    result = shtc3_write_command(0x3517);
    if (result != ESP_OK) {
        ESP_LOGW(TAG, "SHTC3 wake retry after %s", esp_err_to_name(result));
        i2c_master_bus_reset(s_i2c_bus);
        result = shtc3_write_command(0x3517);
    }
    if (result != ESP_OK) return false;
    vTaskDelay(pdMS_TO_TICKS(50));
    if (shtc3_write_command(0x805d) != ESP_OK) return false;
    vTaskDelay(pdMS_TO_TICKS(20));

    const uint8_t read_id[] = { 0xef, 0xc8 };
    uint8_t id[3] = { 0 };
    result = i2c_master_transmit_receive(s_shtc3, read_id, sizeof(read_id), id, sizeof(id), 1000);
    if (result != ESP_OK || shtc3_crc(id, 2) != id[2]) return false;
    ESP_LOGI(TAG, "SHTC3 ready: id=0x%02x%02x with -4.00 C board compensation", id[0], id[1]);
    return true;
}

static bool init_battery(void)
{
    adc_oneshot_unit_init_cfg_t unit = { .unit_id = ADC_UNIT_1 };
    if (adc_oneshot_new_unit(&unit, &s_adc) != ESP_OK) return false;
    adc_oneshot_chan_cfg_t channel = {
        .atten = ADC_ATTEN_DB_12,
        .bitwidth = ADC_BITWIDTH_12,
    };
    if (adc_oneshot_config_channel(s_adc, ADC_CHANNEL_3, &channel) != ESP_OK) return false;

    adc_cali_curve_fitting_config_t calibration = {
        .unit_id = ADC_UNIT_1,
        .chan = ADC_CHANNEL_3,
        .atten = ADC_ATTEN_DB_12,
        .bitwidth = ADC_BITWIDTH_12,
    };
    if (adc_cali_create_scheme_curve_fitting(&calibration, &s_adc_cali) != ESP_OK) {
        s_adc_cali = NULL;
        ESP_LOGW(TAG, "ADC calibration unavailable; using nominal conversion");
    }
    return true;
}

static bool init_sd(void)
{
    esp_vfs_fat_sdmmc_mount_config_t mount = {
        .format_if_mount_failed = false,
        .max_files = 6,
        .allocation_unit_size = 16 * 1024,
    };
    sdmmc_host_t host = SDMMC_HOST_DEFAULT();
    sdmmc_slot_config_t slot = SDMMC_SLOT_CONFIG_DEFAULT();
    slot.width = 1;
    slot.clk = GPIO_NUM_38;
    slot.cmd = GPIO_NUM_21;
    slot.d0 = GPIO_NUM_39;
    esp_err_t result = esp_vfs_fat_sdmmc_mount("/sdcard", &host, &slot, &mount, &s_sdcard);
    if (result != ESP_OK) {
        s_sdcard = NULL;
        ESP_LOGW(TAG, "SD mount unavailable: %s", esp_err_to_name(result));
        return false;
    }
    return true;
}

uint32_t hmi_board_init(void)
{
    uint32_t ready = 0;
    bool audio = init_audio_and_i2c();
    if (audio) ready |= HMI_BOARD_AUDIO_READY;
    if (init_shtc3()) ready |= HMI_BOARD_ENV_READY;
    if (init_battery()) ready |= HMI_BOARD_BATTERY_READY;
    if (init_sd()) ready |= HMI_BOARD_SD_READY;
    return ready;
}

int hmi_board_env_read(int32_t *temperature_centi_c, uint32_t *humidity_centi_pct)
{
    if (!s_shtc3 || !temperature_centi_c || !humidity_centi_pct) return ESP_ERR_INVALID_STATE;
    uint8_t data[6];

    // Keep SHTC3 awake. The reference emulator found that repeatedly sleeping
    // it on this shared codec bus can leave warm-reset wakeups unable to ACK.
    esp_err_t result = shtc3_measure_once(data);
    if (result != ESP_OK && s_i2c_bus) {
        ESP_LOGW(TAG, "SHTC3 measurement recovery after %s", esp_err_to_name(result));
        i2c_master_bus_reset(s_i2c_bus);
        if (shtc3_write_command(0x3517) == ESP_OK) {
            vTaskDelay(pdMS_TO_TICKS(50));
            result = shtc3_measure_once(data);
        }
    }
    if (result != ESP_OK) return result;
    if (shtc3_crc(data, 2) != data[2] || shtc3_crc(data + 3, 2) != data[5]) return ESP_ERR_INVALID_CRC;

    uint32_t raw_t = ((uint32_t)data[0] << 8) | data[1];
    uint32_t raw_h = ((uint32_t)data[3] << 8) | data[4];
    *temperature_centi_c = -4500
        + (int32_t)((17500u * raw_t + 32768u) / 65536u)
        - SHTC3_BOARD_HEAT_OFFSET_CENTI_C;
    *humidity_centi_pct = (10000u * raw_h + 32768u) / 65536u;
    if (*humidity_centi_pct > 10000u) *humidity_centi_pct = 10000u;
    return ESP_OK;
}

int hmi_board_battery_read(uint32_t *millivolts, uint16_t *raw)
{
    if (!s_adc || !millivolts || !raw) return ESP_ERR_INVALID_STATE;
    int value = 0;
    esp_err_t result = adc_oneshot_read(s_adc, ADC_CHANNEL_3, &value);
    if (result != ESP_OK) return result;
    int pin_mv = 0;
    if (!s_adc_cali || adc_cali_raw_to_voltage(s_adc_cali, value, &pin_mv) != ESP_OK) {
        pin_mv = (value * 2500) / 4095;
    }
    *raw = (uint16_t)value;
    *millivolts = (uint32_t)(pin_mv * 3);
    return ESP_OK;
}

int hmi_board_audio_read(int16_t *samples, size_t sample_count)
{
    if (!s_capture_queue || !samples || sample_count == 0) return ESP_ERR_INVALID_STATE;
    audio_chunk_t chunk;
    if (xQueueReceive(s_capture_queue, &chunk, pdMS_TO_TICKS(50)) != pdTRUE) {
        return ESP_ERR_TIMEOUT;
    }
    size_t count = sample_count < chunk.sample_count ? sample_count : chunk.sample_count;
    memcpy(samples, chunk.samples, count * sizeof(int16_t));
    return ESP_OK;
}

int hmi_board_audio_write(const int16_t *samples, size_t sample_count)
{
    if (!s_playback_queue || !samples || sample_count == 0
        || sample_count > AUDIO_CHUNK_SAMPLES) return ESP_ERR_INVALID_ARG;
    audio_chunk_t chunk = { .sample_count = sample_count };
    memcpy(chunk.samples, samples, sample_count * sizeof(int16_t));
    return xQueueSend(s_playback_queue, &chunk, pdMS_TO_TICKS(1000)) == pdTRUE
        ? ESP_OK
        : ESP_ERR_TIMEOUT;
}

int hmi_board_audio_set_volume(uint8_t percent)
{
    if (!s_playback) return ESP_ERR_INVALID_STATE;
    if (percent > 100) percent = 100;
    return esp_codec_dev_set_out_vol(s_playback, percent);
}

int hmi_board_te_init(void)
{
    gpio_config_t config = {
        .pin_bit_mask = 1ULL << GPIO_NUM_6,
        .mode = GPIO_MODE_INPUT,
        .pull_up_en = GPIO_PULLUP_DISABLE,
        .pull_down_en = GPIO_PULLDOWN_ENABLE,
        .intr_type = GPIO_INTR_POSEDGE,
    };
    esp_err_t result = gpio_config(&config);
    if (result != ESP_OK) return result;
    result = gpio_install_isr_service(0);
    if (result != ESP_OK && result != ESP_ERR_INVALID_STATE) return result;
    result = gpio_isr_handler_add(GPIO_NUM_6, te_rising_isr, NULL);
    if (result != ESP_OK) return result;
    atomic_store_explicit(&s_te_rising_edge, false, memory_order_relaxed);
    return gpio_intr_enable(GPIO_NUM_6);
}

bool hmi_board_te_take_rising_edge(void)
{
    return atomic_exchange_explicit(&s_te_rising_edge, false, memory_order_relaxed);
}

int hmi_board_sd_stats(uint64_t *total_kib, uint64_t *free_kib)
{
    if (!s_sdcard || !total_kib || !free_kib) return ESP_ERR_INVALID_STATE;
    uint64_t total_bytes = 0;
    uint64_t free_bytes = 0;
    esp_err_t result = esp_vfs_fat_info("/sdcard", &total_bytes, &free_bytes);
    if (result != ESP_OK) return result;
    *total_kib = total_bytes / 1024u;
    *free_kib = free_bytes / 1024u;
    return result;
}

int hmi_board_sd_append(const uint8_t *data, size_t data_len)
{
    if (!s_sdcard || !data || data_len == 0) return ESP_ERR_INVALID_STATE;
    FILE *file = fopen("/sdcard/events.ndj", "ab");
    if (!file) {
        ESP_LOGE(TAG, "event log open failed: errno=%d (%s)", errno, strerror(errno));
        return ESP_FAIL;
    }
    if (fseek(file, 0, SEEK_END) != 0) {
        fclose(file);
        return ESP_FAIL;
    }
    long original_size = ftell(file);
    if (original_size < 0) {
        fclose(file);
        return ESP_FAIL;
    }
    size_t written = fwrite(data, 1, data_len, file);
    int flush_result = fflush(file);
    int sync_result = flush_result == 0 ? fsync(fileno(file)) : -1;
    bool complete = written == data_len && flush_result == 0 && sync_result == 0;
    if (!complete) {
        int saved_errno = errno;
        if (ftruncate(fileno(file), original_size) != 0) {
            ESP_LOGE(TAG, "event log rollback failed at offset %ld: errno=%d (%s)",
                     original_size, errno, strerror(errno));
        } else {
            fsync(fileno(file));
        }
        errno = saved_errno;
    }
    int close_result = fclose(file);
    if (!complete) {
        ESP_LOGE(TAG, "event log append failed: wrote=%u/%u flush=%d sync=%d close=%d errno=%d (%s)",
                 (unsigned)written, (unsigned)data_len, flush_result, sync_result, close_result,
                 errno, strerror(errno));
        return ESP_FAIL;
    }
    if (close_result != 0) {
        // The fsync above established durability. Retrying would duplicate the
        // committed records, so report the close anomaly without failing.
        ESP_LOGW(TAG, "event log close failed after durable append: errno=%d (%s)",
                 errno, strerror(errno));
    }
    return ESP_OK;
}

int hmi_board_time_init(const char *timezone)
{
    if (!timezone) return ESP_ERR_INVALID_ARG;
    if (setenv("TZ", timezone, 1) != 0) return ESP_FAIL;
    tzset();
    if (!s_time_started) {
        esp_sntp_setoperatingmode(ESP_SNTP_OPMODE_POLL);
        esp_sntp_setservername(0, "pool.ntp.org");
        esp_sntp_init();
        s_time_started = true;
        ESP_LOGI(TAG, "SNTP started with timezone %s", timezone);
    }
    return ESP_OK;
}

int hmi_board_time_read(int32_t *year, uint8_t *month, uint8_t *day,
                        uint8_t *weekday, uint8_t *hour, uint8_t *minute,
                        uint8_t *second)
{
    if (!year || !month || !day || !weekday || !hour || !minute || !second) {
        return ESP_ERR_INVALID_ARG;
    }
    time_t now;
    time(&now);
    if (now < 1700000000) return ESP_ERR_INVALID_STATE;
    struct tm local;
    if (!localtime_r(&now, &local)) return ESP_FAIL;
    *year = local.tm_year + 1900;
    *month = local.tm_mon + 1;
    *day = local.tm_mday;
    *weekday = local.tm_wday;
    *hour = local.tm_hour;
    *minute = local.tm_min;
    *second = local.tm_sec;
    return ESP_OK;
}
