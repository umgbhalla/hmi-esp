#include "hmi_touch349.h"

#include <string.h>
#include <stdlib.h>
#include <time.h>

#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "driver/ledc.h"
#include "driver/spi_master.h"
#include "esp_check.h"
#include "esp_heap_caps.h"
#include "esp_io_expander_tca9554.h"
#include "esp_lcd_axs15231b.h"
#include "esp_lcd_panel_io.h"
#include "esp_lcd_panel_ops.h"
#include "esp_log.h"
#include "esp_sntp.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "freertos/task.h"

#define LCD_HOST SPI3_HOST
#define LCD_CS GPIO_NUM_9
#define LCD_CLK GPIO_NUM_10
#define LCD_D0 GPIO_NUM_11
#define LCD_D1 GPIO_NUM_12
#define LCD_D2 GPIO_NUM_13
#define LCD_D3 GPIO_NUM_14
#define LCD_BACKLIGHT GPIO_NUM_42

#define SYSTEM_I2C_SDA GPIO_NUM_47
#define SYSTEM_I2C_SCL GPIO_NUM_48
#define TOUCH_I2C_SDA GPIO_NUM_17
#define TOUCH_I2C_SCL GPIO_NUM_18
#define TOUCH_I2C_ADDRESS 0x3B

#define EXIO_TOUCH_INT (1ULL << 0)
#define EXIO_BACKLIGHT_ENABLE (1ULL << 1)
#define EXIO_LCD_RESET (1ULL << 5)

static const char *TAG = "hmi_touch349";
static esp_lcd_panel_handle_t panel;
static esp_io_expander_handle_t expander;
static i2c_master_bus_handle_t touch_bus;
static i2c_master_dev_handle_t touch_device;
static SemaphoreHandle_t transfer_done;
static uint16_t *framebuffer;
static uint16_t *dma_band;
static uint16_t flush_failures;
static bool initialized;
static bool time_started;

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

static esp_err_t init_system_i2c(i2c_master_bus_handle_t *bus) {
    i2c_master_bus_config_t config = {
        .i2c_port = I2C_NUM_0,
        .sda_io_num = SYSTEM_I2C_SDA,
        .scl_io_num = SYSTEM_I2C_SCL,
        .clk_source = I2C_CLK_SRC_DEFAULT,
        .glitch_ignore_cnt = 7,
        .flags.enable_internal_pullup = true,
    };
    return i2c_new_master_bus(&config, bus);
}

static esp_err_t init_touch_i2c(void) {
    i2c_master_bus_config_t bus_config = {
        .i2c_port = I2C_NUM_1,
        .sda_io_num = TOUCH_I2C_SDA,
        .scl_io_num = TOUCH_I2C_SCL,
        .clk_source = I2C_CLK_SRC_DEFAULT,
        .glitch_ignore_cnt = 7,
        .flags.enable_internal_pullup = true,
    };
    ESP_RETURN_ON_ERROR(i2c_new_master_bus(&bus_config, &touch_bus), TAG, "touch bus");
    i2c_device_config_t device_config = {
        .dev_addr_length = I2C_ADDR_BIT_LEN_7,
        .device_address = TOUCH_I2C_ADDRESS,
        .scl_speed_hz = 300000,
    };
    return i2c_master_bus_add_device(touch_bus, &device_config, &touch_device);
}

static esp_err_t init_expander(i2c_master_bus_handle_t system_bus) {
    ESP_RETURN_ON_ERROR(
        esp_io_expander_new_i2c_tca9554(
            system_bus,
            ESP_IO_EXPANDER_I2C_TCA9554_ADDRESS_000,
            &expander
        ),
        TAG,
        "TCA9554"
    );
    ESP_RETURN_ON_ERROR(
        esp_io_expander_set_dir(expander, EXIO_TOUCH_INT, IO_EXPANDER_INPUT),
        TAG,
        "touch interrupt direction"
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
        "backlight off"
    );
    return esp_io_expander_set_level(expander, EXIO_LCD_RESET, 1);
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

static esp_err_t init_backlight(void) {
    ledc_timer_config_t timer = {
        .speed_mode = LEDC_LOW_SPEED_MODE,
        .duty_resolution = LEDC_TIMER_8_BIT,
        .timer_num = LEDC_TIMER_3,
        .freq_hz = 50000,
        .clk_cfg = LEDC_AUTO_CLK,
    };
    ESP_RETURN_ON_ERROR(ledc_timer_config(&timer), TAG, "backlight timer");
    ledc_channel_config_t channel = {
        .gpio_num = LCD_BACKLIGHT,
        .speed_mode = LEDC_LOW_SPEED_MODE,
        .channel = LEDC_CHANNEL_1,
        .intr_type = LEDC_INTR_DISABLE,
        .timer_sel = LEDC_TIMER_3,
        .duty = 0,
        .hpoint = 0,
    };
    return ledc_channel_config(&channel);
}

static esp_err_t init_panel(void) {
    spi_bus_config_t bus = {
        .sclk_io_num = LCD_CLK,
        .data0_io_num = LCD_D0,
        .data1_io_num = LCD_D1,
        .data2_io_num = LCD_D2,
        .data3_io_num = LCD_D3,
        .max_transfer_sz = HMI_TOUCH349_BAND_PIXELS * 2,
    };
    ESP_RETURN_ON_ERROR(spi_bus_initialize(LCD_HOST, &bus, SPI_DMA_CH_AUTO), TAG, "QSPI bus");

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
        // The short Waveshare {SLPOUT, DISPON} sequence leaves some AXS15231B
        // revisions displaying noise. NULL selects the component's full table.
        .init_cmds = NULL,
        .init_cmds_size = 0,
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
    transfer_done = xSemaphoreCreateBinary();
    if (transfer_done == NULL) {
        return ESP_ERR_NO_MEM;
    }
    framebuffer = heap_caps_calloc(HMI_TOUCH349_PIXELS, sizeof(uint16_t), MALLOC_CAP_SPIRAM);
    dma_band = heap_caps_malloc(HMI_TOUCH349_BAND_PIXELS * sizeof(uint16_t), MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL);
    if (framebuffer == NULL || dma_band == NULL) {
        return ESP_ERR_NO_MEM;
    }

    i2c_master_bus_handle_t system_bus = NULL;
    ESP_RETURN_ON_ERROR(init_backlight(), TAG, "backlight");
    ESP_RETURN_ON_ERROR(init_system_i2c(&system_bus), TAG, "system I2C");
    ESP_RETURN_ON_ERROR(init_expander(system_bus), TAG, "expander");
    ESP_RETURN_ON_ERROR(init_touch_i2c(), TAG, "touch I2C");
    ESP_RETURN_ON_ERROR(init_panel(), TAG, "panel");
    initialized = true;
    ESP_LOGI(TAG, "Touch349 V2 ready: %dx%d RGB565", HMI_TOUCH349_WIDTH, HMI_TOUCH349_HEIGHT);
    return ESP_OK;
}

uint16_t *hmi_touch349_framebuffer(size_t *pixel_count) {
    if (pixel_count != NULL) {
        *pixel_count = framebuffer == NULL ? 0 : HMI_TOUCH349_PIXELS;
    }
    return framebuffer;
}

int hmi_touch349_flush_full(hmi_touch349_flush_stats_t *stats) {
    if (!initialized || framebuffer == NULL || dma_band == NULL || panel == NULL) {
        return ESP_ERR_INVALID_STATE;
    }
    int64_t started = esp_timer_get_time();
    uint32_t wait_us = 0;
    for (uint16_t band = 0; band < HMI_TOUCH349_HEIGHT / HMI_TOUCH349_BAND_ROWS; ++band) {
        const size_t offset = band * HMI_TOUCH349_BAND_PIXELS;
        for (size_t index = 0; index < HMI_TOUCH349_BAND_PIXELS; ++index) {
            dma_band[index] = __builtin_bswap16(framebuffer[offset + index]);
        }
        int y0 = band * HMI_TOUCH349_BAND_ROWS;
        esp_err_t result = esp_lcd_panel_draw_bitmap(
            panel,
            0,
            y0,
            HMI_TOUCH349_WIDTH,
            y0 + HMI_TOUCH349_BAND_ROWS,
            dma_band
        );
        if (result != ESP_OK) {
            flush_failures++;
            return result;
        }
        int64_t wait_started = esp_timer_get_time();
        if (xSemaphoreTake(transfer_done, pdMS_TO_TICKS(250)) != pdTRUE) {
            flush_failures++;
            return ESP_ERR_TIMEOUT;
        }
        wait_us += (uint32_t)(esp_timer_get_time() - wait_started);
    }
    if (stats != NULL) {
        stats->flush_us = (uint32_t)(esp_timer_get_time() - started);
        stats->dma_wait_us = wait_us;
        stats->bands = HMI_TOUCH349_HEIGHT / HMI_TOUCH349_BAND_ROWS;
        stats->failures = flush_failures;
    }
    return ESP_OK;
}

int hmi_touch349_backlight_set(uint8_t duty, bool enabled) {
    if (expander == NULL) {
        return ESP_ERR_INVALID_STATE;
    }
    ESP_RETURN_ON_ERROR(
        esp_io_expander_set_level(expander, EXIO_BACKLIGHT_ENABLE, enabled ? 1 : 0),
        TAG,
        "backlight enable"
    );
    ESP_RETURN_ON_ERROR(
        ledc_set_duty(LEDC_LOW_SPEED_MODE, LEDC_CHANNEL_1, enabled ? duty : 0),
        TAG,
        "backlight duty"
    );
    return ledc_update_duty(LEDC_LOW_SPEED_MODE, LEDC_CHANNEL_1);
}

int hmi_touch349_touch_read(uint16_t *x, uint16_t *y, bool *pressed) {
    if (touch_device == NULL || x == NULL || y == NULL || pressed == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    const uint8_t command[11] = {0xB5, 0xAB, 0xA5, 0x5A, 0, 0, 0, 0x0E, 0, 0, 0};
    uint8_t response[32] = {0};
    ESP_RETURN_ON_ERROR(
        i2c_master_transmit_receive(
            touch_device,
            command,
            sizeof(command),
            response,
            sizeof(response),
            20
        ),
        TAG,
        "touch read"
    );
    *pressed = response[1] > 0 && response[1] < 5;
    if (!*pressed) {
        return ESP_OK;
    }
    uint16_t raw_x = ((uint16_t)(response[2] & 0x0F) << 8) | response[3];
    uint16_t raw_y = ((uint16_t)(response[4] & 0x0F) << 8) | response[5];
    raw_x = raw_x > HMI_TOUCH349_HEIGHT ? HMI_TOUCH349_HEIGHT : raw_x;
    raw_y = raw_y > HMI_TOUCH349_WIDTH ? HMI_TOUCH349_WIDTH : raw_y;
    *x = raw_y == HMI_TOUCH349_WIDTH ? HMI_TOUCH349_WIDTH - 1 : raw_y;
    *y = raw_x == 0 ? HMI_TOUCH349_HEIGHT - 1 : HMI_TOUCH349_HEIGHT - raw_x;
    return ESP_OK;
}

int hmi_touch349_time_init(const char *timezone) {
    if (timezone == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    if (setenv("TZ", timezone, 1) != 0) {
        return ESP_FAIL;
    }
    tzset();
    if (!time_started) {
        esp_sntp_setoperatingmode(ESP_SNTP_OPMODE_POLL);
        esp_sntp_setservername(0, "pool.ntp.org");
        esp_sntp_init();
        time_started = true;
    }
    return ESP_OK;
}

int hmi_touch349_time_read(int32_t *year, uint8_t *month, uint8_t *day,
                           uint8_t *weekday, uint8_t *hour, uint8_t *minute,
                           uint8_t *second) {
    if (year == NULL || month == NULL || day == NULL || weekday == NULL ||
        hour == NULL || minute == NULL || second == NULL) {
        return ESP_ERR_INVALID_ARG;
    }
    time_t now;
    time(&now);
    if (now < 1700000000) {
        return ESP_ERR_INVALID_STATE;
    }
    struct tm local;
    if (localtime_r(&now, &local) == NULL) {
        return ESP_FAIL;
    }
    *year = local.tm_year + 1900;
    *month = local.tm_mon + 1;
    *day = local.tm_mday;
    *weekday = local.tm_wday;
    *hour = local.tm_hour;
    *minute = local.tm_min;
    *second = local.tm_sec;
    return ESP_OK;
}
