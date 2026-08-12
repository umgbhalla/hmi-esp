#include "hmi_touch349.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "driver/i2s_tdm.h"
#include "driver/ledc.h"
#include "driver/sdmmc_host.h"
#include "driver/spi_master.h"
#include "esp_adc/adc_cali.h"
#include "esp_adc/adc_cali_scheme.h"
#include "esp_adc/adc_oneshot.h"
#include "esp_check.h"
#include "esp_heap_caps.h"
#include "esp_io_expander_tca9554.h"
#include "esp_lcd_axs15231b.h"
#include "esp_lcd_panel_io.h"
#include "esp_lcd_panel_ops.h"
#include "esp_log.h"
#include "esp_netif.h"
#include "esp_netif_sntp.h"
#include "esp_sleep.h"
#include "esp_wifi.h"
#include "esp_event.h"
#include "esp_heap_caps.h"
#include "nvs_flash.h"
#include "esp_timer.h"
#include "esp_vfs_fat.h"
#include "hal/usb_serial_jtag_ll.h"
#include "esp_codec_dev.h"
#include "esp_codec_dev_defaults.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/semphr.h"
#include "freertos/task.h"
#include "sdmmc_cmd.h"

#define LCD_HOST SPI3_HOST
#define LCD_CS GPIO_NUM_9
#define LCD_CLK GPIO_NUM_10
#define LCD_D0 GPIO_NUM_11
#define LCD_D1 GPIO_NUM_12
#define LCD_D2 GPIO_NUM_13
#define LCD_D3 GPIO_NUM_14
#define LCD_BACKLIGHT GPIO_NUM_42
#define POWER_BUTTON GPIO_NUM_16

#define AUDIO_MCLK GPIO_NUM_7
#define AUDIO_BCLK GPIO_NUM_15
#define AUDIO_WS GPIO_NUM_46
#define AUDIO_DIN GPIO_NUM_6
#define AUDIO_DOUT GPIO_NUM_45
#define AUDIO_SAMPLE_RATE 24000
#define AUDIO_CHANNELS 2
#define AUDIO_BITS 16
#define AUDIO_PLAYBACK_SAMPLES 1024
#define AUDIO_PLAYBACK_QUEUE_DEPTH 4

#define SYSTEM_I2C_SDA GPIO_NUM_47
#define SYSTEM_I2C_SCL GPIO_NUM_48

#define TOUCH_I2C_SDA GPIO_NUM_17
#define TOUCH_I2C_SCL GPIO_NUM_18
#define TOUCH_I2C_ADDRESS 0x3B

#define SDMMC_CMD GPIO_NUM_39
#define SDMMC_D0 GPIO_NUM_40
#define SDMMC_CLK GPIO_NUM_41
#define SD_MOUNT_POINT "/sdcard"

#define EXIO_BACKLIGHT_ENABLE (1ULL << 1)
#define EXIO_LCD_RESET (1ULL << 5)
#define EXIO_SYSTEM_ENABLE (1ULL << 6)

static const char *TAG = "touch349_minimal";
static esp_lcd_panel_handle_t panel;
static esp_io_expander_handle_t expander;
static i2c_master_bus_handle_t system_i2c_bus;
static SemaphoreHandle_t transfer_done;
static uint16_t *framebuffer;
static uint16_t *dma_bands[2];
static sdmmc_card_t *sd_card;
static i2c_master_dev_handle_t touch_device;
static adc_oneshot_unit_handle_t battery_adc;
static adc_cali_handle_t battery_calibration;
static bool battery_calibrated;
static volatile bool wifi_connected;
static bool sntp_started;
static uint32_t station_ipv4;
static uint32_t wifi_reconnects;
static uint8_t wifi_last_disconnect_reason;
static uint8_t wifi_target_visible;
static uint8_t wifi_target_channel;
static char wifi_target_ssid[33];
static bool wifi_manual_scan;
static i2s_chan_handle_t audio_rx_channel;
static i2s_chan_handle_t audio_tx_channel;
static esp_codec_dev_handle_t audio_record_device;
static esp_codec_dev_handle_t audio_play_device;
static bool audio_output_open;
static QueueHandle_t audio_playback_queue;
static TaskHandle_t audio_playback_task_handle;
static volatile bool audio_output_busy;
static TaskHandle_t recorder_task_handle;
static SemaphoreHandle_t recorder_lock;
static SemaphoreHandle_t audio_read_lock;
static volatile bool recorder_stop_requested;
static hmi_touch349_recorder_stats_t recorder_stats;
static char recorder_part_path[160];
static char recorder_final_path[160];
static bool initialized;

typedef struct {
    size_t sample_count;
    int16_t samples[AUDIO_PLAYBACK_SAMPLES];
} audio_playback_block_t;

static void audio_playback_task(void *argument) {
    (void)argument;
    audio_playback_block_t block;
    while (true) {
        if (xQueueReceive(audio_playback_queue, &block, portMAX_DELAY) != pdTRUE) {
            continue;
        }
        audio_output_busy = true;
        const int result = esp_codec_dev_write(
            audio_play_device,
            block.samples,
            (int)(block.sample_count * sizeof(int16_t))
        );
        audio_output_busy = false;
        if (result != ESP_CODEC_DEV_OK) {
            ESP_LOGE(TAG, "PLAYBACK write failed: %d", result);
        }
    }
}

// Exact external initialization sequence used by the pinned Waveshare V2
// ESP-IDF display, battery, audio, LVGL, and factory examples.
static const axs15231b_lcd_init_cmd_t panel_init_commands[] = {
    {0x11, NULL, 0, 100},
    {0x29, NULL, 0, 100},
};

static void put_u16_le(uint8_t *destination, uint16_t value) {
    destination[0] = (uint8_t)value;
    destination[1] = (uint8_t)(value >> 8);
}

static void put_u32_le(uint8_t *destination, uint32_t value) {
    destination[0] = (uint8_t)value;
    destination[1] = (uint8_t)(value >> 8);
    destination[2] = (uint8_t)(value >> 16);
    destination[3] = (uint8_t)(value >> 24);
}

static void wav_header(uint8_t header[44], uint32_t data_bytes) {
    memset(header, 0, 44);
    memcpy(header, "RIFF", 4);
    put_u32_le(header + 4, 36 + data_bytes);
    memcpy(header + 8, "WAVEfmt ", 8);
    put_u32_le(header + 16, 16);
    put_u16_le(header + 20, 1);
    put_u16_le(header + 22, AUDIO_CHANNELS);
    put_u32_le(header + 24, AUDIO_SAMPLE_RATE);
    put_u32_le(
        header + 28,
        AUDIO_SAMPLE_RATE * AUDIO_CHANNELS * AUDIO_BITS / 8
    );
    put_u16_le(header + 32, AUDIO_CHANNELS * AUDIO_BITS / 8);
    put_u16_le(header + 34, AUDIO_BITS);
    memcpy(header + 36, "data", 4);
    put_u32_le(header + 40, data_bytes);
}

static void audio_levels(const int16_t *samples, size_t sample_count, uint32_t *rms, uint32_t *peak) {
    uint64_t sum_squares = 0;
    uint32_t maximum = 0;
    for (size_t index = 0; index < sample_count; ++index) {
        const int32_t value = samples[index];
        const uint32_t magnitude = value < 0 ? (uint32_t)-value : (uint32_t)value;
        maximum = maximum > magnitude ? maximum : magnitude;
        sum_squares += (uint64_t)value * (uint64_t)value;
    }
    const uint32_t mean_square = sample_count == 0 ? 0 : (uint32_t)(sum_squares / sample_count);
    uint32_t root = mean_square;
    uint32_t estimate = (root + 1) / 2;
    while (estimate < root) {
        root = estimate;
        estimate = (root + mean_square / root) / 2;
    }
    *rms = root;
    *peak = maximum;
}

static void recorder_task(void *argument) {
    (void)argument;
    FILE *file = fopen(recorder_part_path, "wb+");
    int32_t error = file == NULL ? ESP_FAIL : ESP_OK;
    uint64_t data_bytes = 0;
    uint8_t header[44];
    wav_header(header, 0);
    if (file != NULL && fwrite(header, 1, sizeof(header), file) != sizeof(header)) {
        error = ESP_FAIL;
    }

    int16_t *samples = heap_caps_malloc(2048, MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT);
    if (samples == NULL) {
        error = ESP_ERR_NO_MEM;
    }
    while (error == ESP_OK && !recorder_stop_requested) {
        xSemaphoreTake(audio_read_lock, portMAX_DELAY);
        const int read_result = esp_codec_dev_read(audio_record_device, samples, 2048);
        xSemaphoreGive(audio_read_lock);
        if (read_result != ESP_CODEC_DEV_OK) {
            ESP_LOGE(TAG, "RECORDER audio read failed: %d", read_result);
            error = ESP_FAIL;
            break;
        }
        if (fwrite(samples, 1, 2048, file) != 2048) {
            error = ESP_FAIL;
            break;
        }
        data_bytes += 2048;
        uint32_t root = 0;
        uint32_t peak = 0;
        audio_levels(samples, 2048 / sizeof(int16_t), &root, &peak);
        xSemaphoreTake(recorder_lock, portMAX_DELAY);
        recorder_stats.bytes_written = data_bytes;
        recorder_stats.rms = root;
        recorder_stats.peak = peak;
        xSemaphoreGive(recorder_lock);
    }

    if (file != NULL) {
        wav_header(header, (uint32_t)data_bytes);
        if (fseek(file, 0, SEEK_SET) != 0 ||
            fwrite(header, 1, sizeof(header), file) != sizeof(header) ||
            fflush(file) != 0 || fsync(fileno(file)) != 0) {
            error = ESP_FAIL;
        }
        if (fclose(file) != 0) {
            error = ESP_FAIL;
        }
    }
    if (samples != NULL) {
        free(samples);
    }
    if (error == ESP_OK) {
        if (rename(recorder_part_path, recorder_final_path) != 0) {
            error = ESP_FAIL;
        }
    } else {
        unlink(recorder_part_path);
    }
    xSemaphoreTake(recorder_lock, portMAX_DELAY);
    recorder_stats.last_error = error;
    recorder_stats.recording = 0;
    recorder_task_handle = NULL;
    xSemaphoreGive(recorder_lock);
    ESP_LOGI(
        TAG,
        "RECORDER STOP file=%s bytes=%llu result=%ld",
        recorder_final_path,
        (unsigned long long)data_bytes,
        (long)error
    );
    vTaskDelete(NULL);
}

static bool transfer_complete(
    esp_lcd_panel_io_handle_t panel_io,
    esp_lcd_panel_io_event_data_t *event_data,
    void *user_context
) {
    (void)panel_io;
    (void)event_data;
    (void)user_context;
    BaseType_t task_woken = pdFALSE;
    xSemaphoreGiveFromISR(transfer_done, &task_woken);
    return task_woken == pdTRUE;
}

static esp_err_t init_expander(void) {
    i2c_master_bus_config_t bus_config = {
        .i2c_port = I2C_NUM_0,
        .sda_io_num = SYSTEM_I2C_SDA,
        .scl_io_num = SYSTEM_I2C_SCL,
        .clk_source = I2C_CLK_SRC_DEFAULT,
        .glitch_ignore_cnt = 7,
        .flags.enable_internal_pullup = true,
    };
    ESP_RETURN_ON_ERROR(
        i2c_new_master_bus(&bus_config, &system_i2c_bus),
        TAG,
        "system I2C"
    );
    ESP_RETURN_ON_ERROR(
        esp_io_expander_new_i2c_tca9554(
            system_i2c_bus,
            ESP_IO_EXPANDER_I2C_TCA9554_ADDRESS_000,
            &expander
        ),
        TAG,
        "TCA9554"
    );

    // P6 is the V2 battery/system power hold. Claim it before the display reset.
    ESP_RETURN_ON_ERROR(
        esp_io_expander_set_dir(expander, EXIO_SYSTEM_ENABLE, IO_EXPANDER_OUTPUT),
        TAG,
        "SYS_EN direction"
    );
    ESP_RETURN_ON_ERROR(
        esp_io_expander_set_level(expander, EXIO_SYSTEM_ENABLE, 1),
        TAG,
        "SYS_EN high"
    );
    ESP_RETURN_ON_ERROR(
        esp_io_expander_set_dir(
            expander,
            EXIO_BACKLIGHT_ENABLE | EXIO_LCD_RESET,
            IO_EXPANDER_OUTPUT
        ),
        TAG,
        "display output direction"
    );
    ESP_RETURN_ON_ERROR(
        esp_io_expander_set_level(expander, EXIO_BACKLIGHT_ENABLE, 0),
        TAG,
        "backlight disabled"
    );
    return esp_io_expander_set_level(expander, EXIO_LCD_RESET, 1);
}

static esp_err_t init_audio_input(void) {
    i2s_chan_config_t channel_config =
        I2S_CHANNEL_DEFAULT_CONFIG(I2S_NUM_AUTO, I2S_ROLE_MASTER);
    channel_config.auto_clear = true;
    ESP_RETURN_ON_ERROR(
        i2s_new_channel(&channel_config, &audio_tx_channel, &audio_rx_channel),
        TAG,
        "audio RX channel"
    );
    const i2s_tdm_slot_mask_t slots =
        I2S_TDM_SLOT0 | I2S_TDM_SLOT1 | I2S_TDM_SLOT2 | I2S_TDM_SLOT3;
    i2s_tdm_config_t tdm_config = {
        .clk_cfg = I2S_TDM_CLK_DEFAULT_CONFIG(AUDIO_SAMPLE_RATE),
        .slot_cfg = I2S_TDM_PHILIPS_SLOT_DEFAULT_CONFIG(
            I2S_DATA_BIT_WIDTH_32BIT,
            I2S_SLOT_MODE_STEREO,
            slots
        ),
        .gpio_cfg = {
            .mclk = AUDIO_MCLK,
            .bclk = AUDIO_BCLK,
            .ws = AUDIO_WS,
            .dout = AUDIO_DOUT,
            .din = AUDIO_DIN,
            .invert_flags = {
                .mclk_inv = false,
                .bclk_inv = false,
                .ws_inv = false,
            },
        },
    };
    tdm_config.slot_cfg.total_slot = 4;
    ESP_RETURN_ON_ERROR(
        i2s_channel_init_tdm_mode(audio_rx_channel, &tdm_config),
        TAG,
        "audio TDM mode"
    );
    ESP_RETURN_ON_ERROR(
        i2s_channel_init_tdm_mode(audio_tx_channel, &tdm_config),
        TAG,
        "audio TX TDM mode"
    );
    ESP_RETURN_ON_ERROR(i2s_channel_enable(audio_rx_channel), TAG, "audio RX enable");
    ESP_RETURN_ON_ERROR(i2s_channel_enable(audio_tx_channel), TAG, "audio TX enable");

    audio_codec_i2c_cfg_t i2c_config = {
        .port = I2C_NUM_0,
        .addr = ES7210_CODEC_DEFAULT_ADDR,
        .bus_handle = system_i2c_bus,
    };
    const audio_codec_ctrl_if_t *control = audio_codec_new_i2c_ctrl(&i2c_config);
    ESP_RETURN_ON_FALSE(control != NULL, ESP_ERR_NO_MEM, TAG, "ES7210 control");
    audio_codec_i2s_cfg_t i2s_config = {
        .port = I2S_NUM_0,
        .rx_handle = audio_rx_channel,
        .tx_handle = audio_tx_channel,
    };
    const audio_codec_data_if_t *data = audio_codec_new_i2s_data(&i2s_config);
    ESP_RETURN_ON_FALSE(data != NULL, ESP_ERR_NO_MEM, TAG, "ES7210 data");
    es7210_codec_cfg_t codec_config = {
        .ctrl_if = control,
        .master_mode = false,
        .mic_selected = ES7120_SEL_MIC1 | ES7120_SEL_MIC2 |
            ES7120_SEL_MIC3 | ES7120_SEL_MIC4,
        .mclk_src = ES7210_MCLK_FROM_PAD,
        .mclk_div = 256,
    };
    const audio_codec_if_t *codec = es7210_codec_new(&codec_config);
    ESP_RETURN_ON_FALSE(codec != NULL, ESP_FAIL, TAG, "ES7210 codec");
    esp_codec_dev_cfg_t device_config = {
        .dev_type = ESP_CODEC_DEV_TYPE_IN,
        .codec_if = codec,
        .data_if = data,
    };
    audio_record_device = esp_codec_dev_new(&device_config);
    ESP_RETURN_ON_FALSE(audio_record_device != NULL, ESP_FAIL, TAG, "audio input device");
    esp_codec_dev_sample_info_t sample_config = {
        .bits_per_sample = AUDIO_BITS,
        .channel = AUDIO_CHANNELS,
        // Match the vendor echo example. A zero mask lets esp_codec_dev choose
        // the first two stereo slots without creating an invalid 3-slot TDM frame.
        .channel_mask = 0,
        .sample_rate = AUDIO_SAMPLE_RATE,
        .mclk_multiple = 256,
    };
    ESP_RETURN_ON_FALSE(
        esp_codec_dev_open(audio_record_device, &sample_config) == ESP_CODEC_DEV_OK,
        ESP_FAIL,
        TAG,
        "open ES7210 input"
    );
    ESP_RETURN_ON_FALSE(
        esp_codec_dev_set_in_gain(audio_record_device, 30) == ESP_CODEC_DEV_OK,
        ESP_FAIL,
        TAG,
        "ES7210 gain"
    );

    // Input recording stays usable if the independent speaker codec is absent
    // or fails to start. Output readiness is reported separately.
    recorder_lock = xSemaphoreCreateMutex();
    audio_read_lock = xSemaphoreCreateMutex();
    ESP_RETURN_ON_FALSE(
        recorder_lock != NULL && audio_read_lock != NULL,
        ESP_ERR_NO_MEM,
        TAG,
        "audio locks"
    );
    recorder_stats.ready = 1;

    audio_codec_i2c_cfg_t output_i2c_config = {
        .port = I2C_NUM_0,
        .addr = ES8311_CODEC_DEFAULT_ADDR,
        .bus_handle = system_i2c_bus,
    };
    const audio_codec_ctrl_if_t *output_control = audio_codec_new_i2c_ctrl(&output_i2c_config);
    ESP_RETURN_ON_FALSE(output_control != NULL, ESP_ERR_NO_MEM, TAG, "ES8311 control");
    es8311_codec_cfg_t output_codec_config = {
        .ctrl_if = output_control,
        .gpio_if = audio_codec_new_gpio(),
        .codec_mode = ESP_CODEC_DEV_WORK_MODE_DAC,
        .pa_pin = -1,
        .master_mode = false,
        .use_mclk = true,
        .mclk_div = 256,
        .hw_gain = {.pa_gain = 6},
    };
    const audio_codec_if_t *output_codec = es8311_codec_new(&output_codec_config);
    ESP_RETURN_ON_FALSE(output_codec != NULL, ESP_FAIL, TAG, "ES8311 codec");
    esp_codec_dev_cfg_t output_device_config = {
        .dev_type = ESP_CODEC_DEV_TYPE_OUT,
        .codec_if = output_codec,
        .data_if = data,
    };
    audio_play_device = esp_codec_dev_new(&output_device_config);
    ESP_RETURN_ON_FALSE(audio_play_device != NULL, ESP_FAIL, TAG, "audio output device");
    ESP_RETURN_ON_FALSE(
        esp_codec_dev_open(audio_play_device, &sample_config) == ESP_CODEC_DEV_OK,
        ESP_FAIL,
        TAG,
        "open ES8311 output"
    );
    ESP_RETURN_ON_FALSE(
        esp_codec_dev_set_out_vol(audio_play_device, 55) == ESP_CODEC_DEV_OK,
        ESP_FAIL,
        TAG,
        "ES8311 volume"
    );
    audio_output_open = true;
    audio_playback_queue = xQueueCreate(
        AUDIO_PLAYBACK_QUEUE_DEPTH,
        sizeof(audio_playback_block_t)
    );
    ESP_RETURN_ON_FALSE(audio_playback_queue != NULL, ESP_ERR_NO_MEM, TAG, "playback queue");
    ESP_RETURN_ON_FALSE(
        xTaskCreatePinnedToCore(
            audio_playback_task,
            "audio_playback",
            6144,
            NULL,
            9,
            &audio_playback_task_handle,
            1
        ) == pdPASS,
        ESP_ERR_NO_MEM,
        TAG,
        "playback task"
    );
    ESP_LOGI(
        TAG,
        "AUDIO READY ES7210 TDM %uHz %uch GPIO mclk=%d bclk=%d ws=%d din=%d",
        AUDIO_SAMPLE_RATE,
        AUDIO_CHANNELS,
        AUDIO_MCLK,
        AUDIO_BCLK,
        AUDIO_WS,
        AUDIO_DIN
    );
    return ESP_OK;
}

static esp_err_t init_backlight(void) {
    ledc_timer_config_t timer = {
        .speed_mode = LEDC_LOW_SPEED_MODE,
        .duty_resolution = LEDC_TIMER_8_BIT,
        .timer_num = LEDC_TIMER_3,
        .freq_hz = 50 * 1000,
        .clk_cfg = LEDC_AUTO_CLK,
    };
    ESP_RETURN_ON_ERROR(ledc_timer_config(&timer), TAG, "backlight timer");
    ledc_channel_config_t channel = {
        .gpio_num = LCD_BACKLIGHT,
        .speed_mode = LEDC_LOW_SPEED_MODE,
        .channel = LEDC_CHANNEL_1,
        .intr_type = LEDC_INTR_DISABLE,
        .timer_sel = LEDC_TIMER_3,
        // GPIO42 PWM is active-low. Duty 255 is fully off.
        .duty = 255,
        .hpoint = 0,
    };
    return ledc_channel_config(&channel);
}

static esp_err_t init_touch(void) {
    i2c_master_bus_config_t bus_config = {
        .i2c_port = I2C_NUM_1,
        .sda_io_num = TOUCH_I2C_SDA,
        .scl_io_num = TOUCH_I2C_SCL,
        .clk_source = I2C_CLK_SRC_DEFAULT,
        .glitch_ignore_cnt = 7,
        .flags.enable_internal_pullup = true,
    };
    i2c_master_bus_handle_t bus = NULL;
    ESP_RETURN_ON_ERROR(i2c_new_master_bus(&bus_config, &bus), TAG, "touch I2C");
    const i2c_device_config_t device_config = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address = TOUCH_I2C_ADDRESS,
        .scl_speed_hz = 300000,
    };
    return i2c_master_bus_add_device(bus, &device_config, &touch_device);
}

static esp_err_t init_battery_adc(void) {
    const adc_oneshot_unit_init_cfg_t unit_config = {.unit_id = ADC_UNIT_1};
    ESP_RETURN_ON_ERROR(adc_oneshot_new_unit(&unit_config, &battery_adc), TAG, "battery ADC unit");
    const adc_oneshot_chan_cfg_t channel_config = {
        .atten = ADC_ATTEN_DB_12,
        .bitwidth = ADC_BITWIDTH_12,
    };
    ESP_RETURN_ON_ERROR(
        adc_oneshot_config_channel(battery_adc, ADC_CHANNEL_3, &channel_config),
        TAG,
        "battery ADC channel"
    );
    const adc_cali_curve_fitting_config_t calibration_config = {
        .unit_id = ADC_UNIT_1,
        .chan = ADC_CHANNEL_3,
        .atten = ADC_ATTEN_DB_12,
        .bitwidth = ADC_BITWIDTH_12,
    };
    battery_calibrated = adc_cali_create_scheme_curve_fitting(
        &calibration_config,
        &battery_calibration
    ) == ESP_OK;
    return ESP_OK;
}

static void network_event_handler(
    void *argument,
    esp_event_base_t event_base,
    int32_t event_id,
    void *event_data
) {
    (void)argument;
    if (event_base == WIFI_EVENT && event_id == WIFI_EVENT_STA_DISCONNECTED) {
        const wifi_event_sta_disconnected_t *event = event_data;
        wifi_connected = false;
        station_ipv4 = 0;
        wifi_last_disconnect_reason = event->reason;
        wifi_reconnects++;
        ESP_LOGW(TAG, "WIFI DISCONNECTED reason=%u rssi=%d", event->reason, event->rssi);
        if (!wifi_manual_scan) {
            vTaskDelay(pdMS_TO_TICKS(500));
            esp_wifi_connect();
        }
    } else if (event_base == IP_EVENT && event_id == IP_EVENT_STA_GOT_IP) {
        const ip_event_got_ip_t *event = event_data;
        station_ipv4 = event->ip_info.ip.addr;
        wifi_connected = true;
        wifi_last_disconnect_reason = 0;
        ESP_LOGI(TAG, "WIFI READY ip=" IPSTR, IP2STR(&event->ip_info.ip));
        if (!sntp_started) {
            const esp_sntp_config_t sntp_config = ESP_NETIF_SNTP_DEFAULT_CONFIG("pool.ntp.org");
            if (esp_netif_sntp_init(&sntp_config) == ESP_OK) {
                sntp_started = true;
            }
        }
    }
}

static esp_err_t scan_wifi_target(void) {
    const wifi_scan_config_t scan_config = {
        .ssid = NULL,
        .bssid = NULL,
        .channel = 0,
        .show_hidden = true,
        .scan_type = WIFI_SCAN_TYPE_ACTIVE,
    };
    ESP_RETURN_ON_ERROR(esp_wifi_scan_start(&scan_config, true), TAG, "Wi-Fi scan");
    uint16_t count = 0;
    ESP_RETURN_ON_ERROR(esp_wifi_scan_get_ap_num(&count), TAG, "Wi-Fi scan count");
    const uint16_t capacity = count > 64 ? 64 : count;
    wifi_ap_record_t *records = capacity == 0 ? NULL : calloc(capacity, sizeof(*records));
    ESP_RETURN_ON_FALSE(capacity == 0 || records != NULL, ESP_ERR_NO_MEM, TAG, "Wi-Fi scan records");
    uint16_t fetched = capacity;
    esp_err_t error = capacity == 0 ? ESP_OK : esp_wifi_scan_get_ap_records(&fetched, records);
    uint16_t channel_counts[15] = {0};
    wifi_target_visible = 0;
    wifi_target_channel = 0;
    int8_t target_rssi = -127;
    wifi_auth_mode_t target_auth = WIFI_AUTH_OPEN;
    if (error == ESP_OK) {
        for (uint16_t index = 0; index < fetched; ++index) {
            const uint8_t channel = records[index].primary;
            if (channel < 15) {
                channel_counts[channel]++;
            }
            if (strcmp((const char *)records[index].ssid, wifi_target_ssid) == 0 &&
                (!wifi_target_visible || records[index].rssi > target_rssi)) {
                wifi_target_visible = 1;
                wifi_target_channel = channel;
                target_rssi = records[index].rssi;
                target_auth = records[index].authmode;
            }
            ESP_LOGI(
                TAG,
                "WIFI AP ssid=%s channel=%u rssi=%d auth=%u",
                records[index].ssid,
                channel,
                records[index].rssi,
                records[index].authmode
            );
        }
    }
    ESP_LOGI(TAG, "WIFI SCAN total=%u inspected=%u", count, fetched);
    for (uint8_t channel = 1; channel < 15; ++channel) {
        if (channel_counts[channel] != 0) {
            ESP_LOGI(TAG, "WIFI SCAN channel=%u aps=%u", channel, channel_counts[channel]);
        }
    }
    ESP_LOGI(
        TAG,
        "WIFI TARGET exact=%u channel=%u rssi=%d auth=%u",
        wifi_target_visible,
        wifi_target_channel,
        target_rssi,
        target_auth
    );
    free(records);
    return error;
}

static esp_err_t init_power_button(void) {
    const gpio_config_t config = {
        .pin_bit_mask = 1ULL << POWER_BUTTON,
        .mode = GPIO_MODE_INPUT,
        .pull_up_en = GPIO_PULLUP_ENABLE,
        .pull_down_en = GPIO_PULLDOWN_DISABLE,
        .intr_type = GPIO_INTR_DISABLE,
    };
    return gpio_config(&config);
}

static esp_err_t reset_panel(void) {
    ESP_RETURN_ON_ERROR(
        esp_io_expander_set_level(expander, EXIO_LCD_RESET, 1),
        TAG,
        "reset high"
    );
    vTaskDelay(pdMS_TO_TICKS(30));
    ESP_RETURN_ON_ERROR(
        esp_io_expander_set_level(expander, EXIO_LCD_RESET, 0),
        TAG,
        "reset low"
    );
    vTaskDelay(pdMS_TO_TICKS(250));
    ESP_RETURN_ON_ERROR(
        esp_io_expander_set_level(expander, EXIO_LCD_RESET, 1),
        TAG,
        "reset release"
    );
    vTaskDelay(pdMS_TO_TICKS(30));
    return ESP_OK;
}

static esp_err_t init_panel(void) {
    spi_bus_config_t bus_config = {
        .sclk_io_num = LCD_CLK,
        .data0_io_num = LCD_D0,
        .data1_io_num = LCD_D1,
        .data2_io_num = LCD_D2,
        .data3_io_num = LCD_D3,
        .max_transfer_sz = HMI_TOUCH349_BAND_PIXELS * sizeof(uint16_t),
    };
    ESP_RETURN_ON_ERROR(
        spi_bus_initialize(LCD_HOST, &bus_config, SPI_DMA_CH_AUTO),
        TAG,
        "QSPI bus"
    );

    esp_lcd_panel_io_spi_config_t io_config = {
        .cs_gpio_num = LCD_CS,
        .dc_gpio_num = -1,
        .spi_mode = 3,
        .pclk_hz = 40 * 1000 * 1000,
        .trans_queue_depth = 10,
        .on_color_trans_done = transfer_complete,
        .lcd_cmd_bits = 32,
        .lcd_param_bits = 8,
        .flags.quad_mode = true,
    };
    esp_lcd_panel_io_handle_t panel_io = NULL;
    ESP_RETURN_ON_ERROR(
        esp_lcd_new_panel_io_spi(LCD_HOST, &io_config, &panel_io),
        TAG,
        "panel IO"
    );

    axs15231b_vendor_config_t vendor_config = {
        .init_cmds = panel_init_commands,
        .init_cmds_size = sizeof(panel_init_commands) / sizeof(panel_init_commands[0]),
        .flags.use_qspi_interface = 1,
    };
    esp_lcd_panel_dev_config_t panel_config = {
        .reset_gpio_num = -1,
        .rgb_ele_order = LCD_RGB_ELEMENT_ORDER_RGB,
        .bits_per_pixel = 16,
        .vendor_config = &vendor_config,
    };
    ESP_RETURN_ON_ERROR(
        esp_lcd_new_panel_axs15231b(panel_io, &panel_config, &panel),
        TAG,
        "AXS15231B"
    );
    ESP_RETURN_ON_ERROR(reset_panel(), TAG, "panel reset");
    return esp_lcd_panel_init(panel);
}

int hmi_touch349_init(void) {
    if (initialized) {
        return ESP_OK;
    }
    ESP_RETURN_ON_ERROR(init_expander(), TAG, "expander");
    ESP_RETURN_ON_ERROR(init_backlight(), TAG, "backlight PWM");
    ESP_RETURN_ON_ERROR(init_touch(), TAG, "touch controller");
    ESP_RETURN_ON_ERROR(init_battery_adc(), TAG, "battery ADC");
    const esp_err_t audio_error = init_audio_input();
    if (audio_error != ESP_OK) {
        ESP_LOGE(TAG, "AUDIO unavailable: %s", esp_err_to_name(audio_error));
    }
    ESP_RETURN_ON_ERROR(init_power_button(), TAG, "power button");

    transfer_done = xSemaphoreCreateCounting(2, 0);
    ESP_RETURN_ON_FALSE(transfer_done != NULL, ESP_ERR_NO_MEM, TAG, "transfer semaphore");
    framebuffer = heap_caps_malloc(
        HMI_TOUCH349_FRAME_BYTES,
        MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT
    );
    dma_bands[0] = heap_caps_malloc(
        HMI_TOUCH349_BAND_PIXELS * sizeof(uint16_t),
        MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL
    );
    dma_bands[1] = heap_caps_malloc(
        HMI_TOUCH349_BAND_PIXELS * sizeof(uint16_t),
        MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL
    );
    ESP_RETURN_ON_FALSE(
        framebuffer != NULL && dma_bands[0] != NULL && dma_bands[1] != NULL,
        ESP_ERR_NO_MEM,
        TAG,
        "frame buffers"
    );
    memset(framebuffer, 0, HMI_TOUCH349_FRAME_BYTES);

    ESP_RETURN_ON_ERROR(init_panel(), TAG, "panel");
    initialized = true;
    ESP_LOGI(TAG, "READY 172x640 QSPI mode3 40MHz RGB565");
    return ESP_OK;
}

int hmi_touch349_recorder_start(const char *filename) {
    ESP_RETURN_ON_FALSE(
        filename != NULL && filename[0] != '\0' && recorder_stats.ready == 1,
        ESP_ERR_INVALID_STATE,
        TAG,
        "recorder unavailable"
    );
    ESP_RETURN_ON_FALSE(
        recorder_task_handle == NULL && recorder_stats.recording == 0,
        ESP_ERR_INVALID_STATE,
        TAG,
        "recorder already active"
    );
    const char *base = strrchr(filename, '/');
    base = base == NULL ? filename : base + 1;
    ESP_RETURN_ON_FALSE(
        strchr(base, '/') == NULL && strstr(base, "..") == NULL,
        ESP_ERR_INVALID_ARG,
        TAG,
        "unsafe filename"
    );
    snprintf(recorder_final_path, sizeof(recorder_final_path), "%s/%s", SD_MOUNT_POINT, base);
    // Keep the temporary file inside FAT 8.3 limits. The deployed volume does
    // not enable long file names.
    snprintf(recorder_part_path, sizeof(recorder_part_path), "%s/REC.TMP", SD_MOUNT_POINT);
    recorder_stop_requested = false;
    xSemaphoreTake(recorder_lock, portMAX_DELAY);
    recorder_stats.bytes_written = 0;
    recorder_stats.rms = 0;
    recorder_stats.peak = 0;
    recorder_stats.last_error = ESP_OK;
    recorder_stats.recording = 1;
    xSemaphoreGive(recorder_lock);
    const BaseType_t created = xTaskCreatePinnedToCore(
        recorder_task,
        "audio_recorder",
        6144,
        NULL,
        8,
        &recorder_task_handle,
        1
    );
    if (created != pdPASS) {
        recorder_stats.recording = 0;
        recorder_task_handle = NULL;
        return ESP_ERR_NO_MEM;
    }
    ESP_LOGI(TAG, "RECORDER START file=%s", recorder_final_path);
    return ESP_OK;
}

int hmi_touch349_recorder_stop(void) {
    ESP_RETURN_ON_FALSE(
        recorder_stats.recording == 1,
        ESP_ERR_INVALID_STATE,
        TAG,
        "recorder not active"
    );
    recorder_stop_requested = true;
    return ESP_OK;
}

int hmi_touch349_recorder_stats(hmi_touch349_recorder_stats_t *stats) {
    ESP_RETURN_ON_FALSE(stats != NULL, ESP_ERR_INVALID_ARG, TAG, "recorder stats");
    if (recorder_lock == NULL) {
        *stats = recorder_stats;
        return ESP_OK;
    }
    xSemaphoreTake(recorder_lock, portMAX_DELAY);
    *stats = recorder_stats;
    xSemaphoreGive(recorder_lock);
    return ESP_OK;
}

int hmi_touch349_audio_read_levels(uint32_t *rms, uint32_t *peak) {
    ESP_RETURN_ON_FALSE(rms != NULL && peak != NULL, ESP_ERR_INVALID_ARG, TAG, "audio levels");
    ESP_RETURN_ON_FALSE(
        audio_record_device != NULL && recorder_stats.ready == 1 && recorder_stats.recording == 0,
        ESP_ERR_INVALID_STATE,
        TAG,
        "audio levels unavailable"
    );
    int16_t samples[1024];
    xSemaphoreTake(audio_read_lock, portMAX_DELAY);
    const int read_result = esp_codec_dev_read(audio_record_device, samples, sizeof(samples));
    xSemaphoreGive(audio_read_lock);
    ESP_RETURN_ON_FALSE(
        read_result == ESP_CODEC_DEV_OK,
        ESP_FAIL,
        TAG,
        "audio level read"
    );
    audio_levels(samples, sizeof(samples) / sizeof(samples[0]), rms, peak);
    return ESP_OK;
}

int hmi_touch349_audio_write(const int16_t *samples, size_t sample_count) {
    ESP_RETURN_ON_FALSE(
        samples != NULL && sample_count > 0 && sample_count <= AUDIO_PLAYBACK_SAMPLES &&
            audio_output_open && audio_playback_queue != NULL && recorder_stats.recording == 0,
        ESP_ERR_INVALID_STATE,
        TAG,
        "audio output unavailable"
    );
    audio_playback_block_t block = {.sample_count = sample_count};
    memcpy(block.samples, samples, sample_count * sizeof(int16_t));
    return xQueueSend(audio_playback_queue, &block, pdMS_TO_TICKS(1000)) == pdTRUE
        ? ESP_OK
        : ESP_ERR_TIMEOUT;
}

int hmi_touch349_audio_volume_set(uint8_t percent) {
    ESP_RETURN_ON_FALSE(audio_output_open, ESP_ERR_INVALID_STATE, TAG, "speaker unavailable");
    return esp_codec_dev_set_out_vol(audio_play_device, percent > 100 ? 100 : percent);
}

int hmi_touch349_audio_output_ready(void) {
    return audio_output_open && audio_playback_queue != NULL &&
            audio_playback_task_handle != NULL
        ? 1
        : 0;
}

int hmi_touch349_audio_pending(void) {
    if (audio_playback_queue == NULL) {
        return 0;
    }
    return (int)uxQueueMessagesWaiting(audio_playback_queue) + (audio_output_busy ? 1 : 0);
}

int hmi_touch349_audio_stop(uint32_t *dropped_samples) {
    ESP_RETURN_ON_FALSE(audio_playback_queue != NULL, ESP_ERR_INVALID_STATE, TAG, "speaker unavailable");
    uint32_t dropped = 0;
    audio_playback_block_t block;
    while (xQueueReceive(audio_playback_queue, &block, 0) == pdTRUE) {
        dropped += (uint32_t)block.sample_count;
    }
    if (dropped_samples != NULL) {
        *dropped_samples = dropped;
    }
    return ESP_OK;
}

int hmi_touch349_console_read(uint8_t *buffer, size_t capacity) {
    ESP_RETURN_ON_FALSE(buffer != NULL && capacity > 0, ESP_ERR_INVALID_ARG, TAG, "console buffer");
    return (int)usb_serial_jtag_ll_read_rxfifo(buffer, (uint32_t)capacity);
}

uint16_t *hmi_touch349_framebuffer(size_t *pixel_count) {
    if (pixel_count != NULL) {
        *pixel_count = framebuffer == NULL ? 0 : HMI_TOUCH349_PIXELS;
    }
    return framebuffer;
}

int hmi_touch349_flush_full(hmi_touch349_flush_stats_t *stats) {
    ESP_RETURN_ON_FALSE(
        initialized && framebuffer != NULL && dma_bands[0] != NULL &&
            dma_bands[1] != NULL && panel != NULL,
        ESP_ERR_INVALID_STATE,
        TAG,
        "display not initialized"
    );

    const int64_t started = esp_timer_get_time();
    uint32_t wait_us = 0;
    while (xSemaphoreTake(transfer_done, 0) == pdTRUE) {
        // Discard any completion token left by an earlier transfer.
    }
    for (uint16_t band = 0; band < HMI_TOUCH349_HEIGHT / HMI_TOUCH349_BAND_ROWS; ++band) {
        // Two DMA buffers let the CPU prepare the next RGB565 band while QSPI
        // sends the current band. Wait only when a buffer is about to be reused.
        if (band >= 2) {
            const int64_t wait_started = esp_timer_get_time();
            ESP_RETURN_ON_FALSE(
                xSemaphoreTake(transfer_done, pdMS_TO_TICKS(250)) == pdTRUE,
                ESP_ERR_TIMEOUT,
                TAG,
                "previous DMA transfer"
            );
            wait_us += (uint32_t)(esp_timer_get_time() - wait_started);
        }

        const size_t offset = band * HMI_TOUCH349_BAND_PIXELS;
        uint16_t *dma_band = dma_bands[band & 1];
        for (size_t index = 0; index < HMI_TOUCH349_BAND_PIXELS; ++index) {
            dma_band[index] = __builtin_bswap16(framebuffer[offset + index]);
        }
        const int y0 = band * HMI_TOUCH349_BAND_ROWS;
        ESP_RETURN_ON_ERROR(
            esp_lcd_panel_draw_bitmap(
                panel,
                0,
                y0,
                HMI_TOUCH349_WIDTH,
                y0 + HMI_TOUCH349_BAND_ROWS,
                dma_band
            ),
            TAG,
            "draw band"
        );
    }
    for (uint16_t pending = 0; pending < 2; ++pending) {
        const int64_t final_wait_started = esp_timer_get_time();
        ESP_RETURN_ON_FALSE(
            xSemaphoreTake(transfer_done, pdMS_TO_TICKS(250)) == pdTRUE,
            ESP_ERR_TIMEOUT,
            TAG,
            "final DMA transfer"
        );
        wait_us += (uint32_t)(esp_timer_get_time() - final_wait_started);
    }

    if (stats != NULL) {
        stats->flush_us = (uint32_t)(esp_timer_get_time() - started);
        stats->dma_wait_us = wait_us;
        stats->bands = HMI_TOUCH349_HEIGHT / HMI_TOUCH349_BAND_ROWS;
    }
    return ESP_OK;
}

int hmi_touch349_backlight_set(uint8_t duty, bool enabled) {
    ESP_RETURN_ON_FALSE(expander != NULL, ESP_ERR_INVALID_STATE, TAG, "expander missing");
    // A live device must never look dead. Active-low duty 204 is 20%
    // brightness. Only the explicit shutdown path may disable the backlight.
    if (enabled && duty > 204) {
        duty = 204;
    }
    const uint8_t applied_duty = enabled ? duty : 255;
    ESP_RETURN_ON_ERROR(
        ledc_set_duty(LEDC_LOW_SPEED_MODE, LEDC_CHANNEL_1, applied_duty),
        TAG,
        "backlight duty"
    );
    ESP_RETURN_ON_ERROR(
        ledc_update_duty(LEDC_LOW_SPEED_MODE, LEDC_CHANNEL_1),
        TAG,
        "backlight update"
    );
    ESP_RETURN_ON_ERROR(
        esp_io_expander_set_level(expander, EXIO_BACKLIGHT_ENABLE, enabled ? 1 : 0),
        TAG,
        "backlight enable"
    );
    ESP_LOGI(TAG, "BACKLIGHT enabled=%d active_low_duty=%u", enabled, applied_duty);
    return ESP_OK;
}

int hmi_touch349_sd_mount(hmi_touch349_sd_stats_t *stats) {
    if (stats != NULL) {
        memset(stats, 0, sizeof(*stats));
    }
    if (sd_card == NULL) {
        const esp_vfs_fat_sdmmc_mount_config_t mount_config = {
            .format_if_mount_failed = false,
            .max_files = 4,
            .allocation_unit_size = 16 * 1024,
        };
        sdmmc_host_t host = SDMMC_HOST_DEFAULT();
        host.max_freq_khz = SDMMC_FREQ_HIGHSPEED;

        sdmmc_slot_config_t slot = SDMMC_SLOT_CONFIG_DEFAULT();
        slot.width = 1;
        slot.clk = SDMMC_CLK;
        slot.cmd = SDMMC_CMD;
        slot.d0 = SDMMC_D0;
        slot.flags |= SDMMC_SLOT_FLAG_INTERNAL_PULLUP;

        const esp_err_t error = esp_vfs_fat_sdmmc_mount(
            SD_MOUNT_POINT,
            &host,
            &slot,
            &mount_config,
            &sd_card
        );
        if (error != ESP_OK) {
            sd_card = NULL;
            ESP_LOGW(TAG, "SD unavailable: %s", esp_err_to_name(error));
            return error;
        }
    }

    if (stats != NULL) {
        stats->capacity_bytes =
            (uint64_t)sd_card->csd.capacity * (uint64_t)sd_card->csd.sector_size;
        DWORD free_clusters = 0;
        FATFS *filesystem = NULL;
        if (f_getfree("0:", &free_clusters, &filesystem) == FR_OK && filesystem != NULL) {
            stats->free_bytes = (uint64_t)free_clusters *
                (uint64_t)filesystem->csize * (uint64_t)sd_card->csd.sector_size;
        }
        stats->sector_size = sd_card->csd.sector_size;
        stats->mounted = 1;
    }
    ESP_LOGI(
        TAG,
        "SD READY mount=%s capacity=%llu sector=%u",
        SD_MOUNT_POINT,
        (unsigned long long)sd_card->csd.capacity * sd_card->csd.sector_size,
        sd_card->csd.sector_size
    );
    return ESP_OK;
}

bool hmi_touch349_power_button_pressed(void) {
    // Waveshare's PWR input on GPIO16 is active-low.
    return gpio_get_level(POWER_BUTTON) == 0;
}

int hmi_touch349_touch_read(uint8_t response[32]) {
    if (!initialized || touch_device == NULL || response == NULL) {
        return ESP_ERR_INVALID_STATE;
    }
    static const uint8_t command[11] = {
        0xB5, 0xAB, 0xA5, 0x5A, 0x00, 0x00, 0x00, 0x0E, 0x00, 0x00, 0x00,
    };
    memset(response, 0, 32);
    return i2c_master_transmit_receive(
        touch_device,
        command,
        sizeof(command),
        response,
        32,
        pdMS_TO_TICKS(100)
    );
}

int hmi_touch349_network_start(const char *ssid, const char *password) {
    if (ssid == NULL || password == NULL || ssid[0] == '\0') {
        return ESP_ERR_INVALID_ARG;
    }
    esp_err_t error = nvs_flash_init();
    if (error == ESP_ERR_NVS_NO_FREE_PAGES || error == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        ESP_RETURN_ON_ERROR(nvs_flash_erase(), TAG, "erase NVS");
        error = nvs_flash_init();
    }
    ESP_RETURN_ON_ERROR(error, TAG, "NVS init");
    ESP_RETURN_ON_ERROR(esp_netif_init(), TAG, "network interface init");
    error = esp_event_loop_create_default();
    if (error != ESP_OK && error != ESP_ERR_INVALID_STATE) {
        return error;
    }
    ESP_RETURN_ON_FALSE(esp_netif_create_default_wifi_sta() != NULL, ESP_FAIL, TAG, "Wi-Fi STA");
    const wifi_init_config_t init_config = WIFI_INIT_CONFIG_DEFAULT();
    ESP_RETURN_ON_ERROR(esp_wifi_init(&init_config), TAG, "Wi-Fi init");
    // The device is deployed in India. Enable the legal 2.4 GHz channels 1-13
    // before scanning, including APs that use channel 12 or 13.
    ESP_RETURN_ON_ERROR(esp_wifi_set_country_code("IN", true), TAG, "Wi-Fi country");
    ESP_RETURN_ON_ERROR(
        esp_event_handler_register(WIFI_EVENT, ESP_EVENT_ANY_ID, network_event_handler, NULL),
        TAG,
        "Wi-Fi event handler"
    );
    ESP_RETURN_ON_ERROR(
        esp_event_handler_register(IP_EVENT, IP_EVENT_STA_GOT_IP, network_event_handler, NULL),
        TAG,
        "IP event handler"
    );
    wifi_config_t config = {0};
    strlcpy(wifi_target_ssid, ssid, sizeof(wifi_target_ssid));
    strlcpy((char *)config.sta.ssid, ssid, sizeof(config.sta.ssid));
    strlcpy((char *)config.sta.password, password, sizeof(config.sta.password));
    config.sta.scan_method = WIFI_ALL_CHANNEL_SCAN;
    config.sta.sort_method = WIFI_CONNECT_AP_BY_SIGNAL;
    config.sta.threshold.authmode = WIFI_AUTH_OPEN;
    config.sta.sae_pwe_h2e = WPA3_SAE_PWE_BOTH;
    ESP_RETURN_ON_ERROR(esp_wifi_set_mode(WIFI_MODE_STA), TAG, "Wi-Fi station mode");
    ESP_RETURN_ON_ERROR(esp_wifi_set_config(WIFI_IF_STA, &config), TAG, "Wi-Fi config");
    ESP_RETURN_ON_ERROR(esp_wifi_set_ps(WIFI_PS_NONE), TAG, "Wi-Fi power save");
    ESP_RETURN_ON_ERROR(esp_wifi_start(), TAG, "Wi-Fi start");
    const esp_err_t scan_error = scan_wifi_target();
    if (scan_error != ESP_OK) {
        ESP_LOGW(TAG, "WIFI SCAN failed: %s", esp_err_to_name(scan_error));
    }
    ESP_RETURN_ON_ERROR(esp_wifi_connect(), TAG, "Wi-Fi connect");
    ESP_LOGI(TAG, "WIFI START ssid=%s", ssid);
    return ESP_OK;
}

int hmi_touch349_network_scan(void) {
    wifi_manual_scan = true;
    const esp_err_t disconnect_error = esp_wifi_disconnect();
    if (disconnect_error != ESP_OK && disconnect_error != ESP_ERR_WIFI_NOT_CONNECT) {
        wifi_manual_scan = false;
        return disconnect_error;
    }
    vTaskDelay(pdMS_TO_TICKS(100));
    const esp_err_t scan_error = scan_wifi_target();
    wifi_manual_scan = false;
    const esp_err_t connect_error = esp_wifi_connect();
    return scan_error != ESP_OK ? scan_error : connect_error;
}

int hmi_touch349_network_stats(hmi_touch349_network_stats_t *stats) {
    ESP_RETURN_ON_FALSE(stats != NULL, ESP_ERR_INVALID_ARG, TAG, "network stats");
    memset(stats, 0, sizeof(*stats));
    stats->connected = wifi_connected ? 1 : 0;
    stats->ipv4 = station_ipv4;
    stats->reconnects = wifi_reconnects;
    stats->last_disconnect_reason = wifi_last_disconnect_reason;
    stats->target_visible = wifi_target_visible;
    stats->target_channel = wifi_target_channel;
    time_t now = 0;
    time(&now);
    stats->time_synced = now > 1700000000 ? 1 : 0;
    if (wifi_connected) {
        wifi_ap_record_t record = {0};
        if (esp_wifi_sta_get_ap_info(&record) == ESP_OK) {
            stats->rssi_dbm = record.rssi;
        }
    }
    return ESP_OK;
}

int hmi_touch349_battery_read(uint16_t *raw, uint32_t *millivolts) {
    ESP_RETURN_ON_FALSE(raw != NULL && millivolts != NULL, ESP_ERR_INVALID_ARG, TAG, "battery stats");
    int sample = 0;
    ESP_RETURN_ON_ERROR(adc_oneshot_read(battery_adc, ADC_CHANNEL_3, &sample), TAG, "battery ADC read");
    int adc_mv = 0;
    if (battery_calibrated) {
        ESP_RETURN_ON_ERROR(
            adc_cali_raw_to_voltage(battery_calibration, sample, &adc_mv),
            TAG,
            "battery calibration"
        );
    } else {
        adc_mv = sample * 3300 / 4095;
    }
    *raw = (uint16_t)sample;
    *millivolts = (uint32_t)adc_mv * 3;
    return ESP_OK;
}

uint32_t hmi_touch349_free_heap(void) {
    return heap_caps_get_free_size(MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT);
}

uint32_t hmi_touch349_free_psram(void) {
    return heap_caps_get_free_size(MALLOC_CAP_SPIRAM);
}

void hmi_touch349_power_off(void) {
    ESP_LOGW(TAG, "POWER OFF requested: display off, backlight off, SYS_EN low");
    if (panel != NULL) {
        const esp_err_t display_error = esp_lcd_panel_disp_on_off(panel, false);
        if (display_error != ESP_OK) {
            ESP_LOGW(TAG, "display-off command failed: %s", esp_err_to_name(display_error));
        }
    }
    const esp_err_t backlight_error = hmi_touch349_backlight_set(255, false);
    if (backlight_error != ESP_OK) {
        ESP_LOGW(TAG, "backlight-off failed: %s", esp_err_to_name(backlight_error));
    }
    vTaskDelay(pdMS_TO_TICKS(60));
    if (expander != NULL) {
        const esp_err_t latch_error =
            esp_io_expander_set_level(expander, EXIO_SYSTEM_ENABLE, 0);
        if (latch_error != ESP_OK) {
            ESP_LOGE(TAG, "SYS_EN release failed: %s", esp_err_to_name(latch_error));
        }
    }

    // Battery power should disappear when SYS_EN is released. USB can continue
    // powering the MCU, so deep sleep guarantees that the firmware stays off
    // instead of re-lighting the panel or continuing background work.
    vTaskDelay(pdMS_TO_TICKS(120));
    esp_deep_sleep_start();
}
