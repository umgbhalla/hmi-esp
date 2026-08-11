#include "hmi_touch349.h"

#include <string.h>

#include "driver/i2c_master.h"
#include "driver/ledc.h"
#include "driver/sdmmc_host.h"
#include "driver/spi_master.h"
#include "esp_check.h"
#include "esp_heap_caps.h"
#include "esp_io_expander_tca9554.h"
#include "esp_lcd_axs15231b.h"
#include "esp_lcd_panel_io.h"
#include "esp_lcd_panel_ops.h"
#include "esp_log.h"
#include "esp_timer.h"
#include "esp_vfs_fat.h"
#include "freertos/FreeRTOS.h"
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

#define SYSTEM_I2C_SDA GPIO_NUM_47
#define SYSTEM_I2C_SCL GPIO_NUM_48

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
static SemaphoreHandle_t transfer_done;
static uint16_t *framebuffer;
static uint16_t *dma_band;
static sdmmc_card_t *sd_card;
static bool initialized;

// Exact external initialization sequence used by the pinned Waveshare V2
// ESP-IDF display, battery, audio, LVGL, and factory examples.
static const axs15231b_lcd_init_cmd_t panel_init_commands[] = {
    {0x11, NULL, 0, 100},
    {0x29, NULL, 0, 100},
};

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
    i2c_master_bus_handle_t bus = NULL;
    ESP_RETURN_ON_ERROR(i2c_new_master_bus(&bus_config, &bus), TAG, "system I2C");
    ESP_RETURN_ON_ERROR(
        esp_io_expander_new_i2c_tca9554(
            bus,
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

    transfer_done = xSemaphoreCreateBinary();
    ESP_RETURN_ON_FALSE(transfer_done != NULL, ESP_ERR_NO_MEM, TAG, "transfer semaphore");
    framebuffer = heap_caps_malloc(
        HMI_TOUCH349_FRAME_BYTES,
        MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT
    );
    dma_band = heap_caps_malloc(
        HMI_TOUCH349_BAND_PIXELS * sizeof(uint16_t),
        MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL
    );
    ESP_RETURN_ON_FALSE(
        framebuffer != NULL && dma_band != NULL,
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

uint16_t *hmi_touch349_framebuffer(size_t *pixel_count) {
    if (pixel_count != NULL) {
        *pixel_count = framebuffer == NULL ? 0 : HMI_TOUCH349_PIXELS;
    }
    return framebuffer;
}

int hmi_touch349_flush_full(hmi_touch349_flush_stats_t *stats) {
    ESP_RETURN_ON_FALSE(
        initialized && framebuffer != NULL && dma_band != NULL && panel != NULL,
        ESP_ERR_INVALID_STATE,
        TAG,
        "display not initialized"
    );

    const int64_t started = esp_timer_get_time();
    uint32_t wait_us = 0;
    xSemaphoreGive(transfer_done);
    for (uint16_t band = 0; band < HMI_TOUCH349_HEIGHT / HMI_TOUCH349_BAND_ROWS; ++band) {
        const int64_t wait_started = esp_timer_get_time();
        ESP_RETURN_ON_FALSE(
            xSemaphoreTake(transfer_done, pdMS_TO_TICKS(250)) == pdTRUE,
            ESP_ERR_TIMEOUT,
            TAG,
            "previous DMA transfer"
        );
        wait_us += (uint32_t)(esp_timer_get_time() - wait_started);

        const size_t offset = band * HMI_TOUCH349_BAND_PIXELS;
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
    const int64_t final_wait_started = esp_timer_get_time();
    ESP_RETURN_ON_FALSE(
        xSemaphoreTake(transfer_done, pdMS_TO_TICKS(250)) == pdTRUE,
        ESP_ERR_TIMEOUT,
        TAG,
        "final DMA transfer"
    );
    wait_us += (uint32_t)(esp_timer_get_time() - final_wait_started);

    if (stats != NULL) {
        stats->flush_us = (uint32_t)(esp_timer_get_time() - started);
        stats->dma_wait_us = wait_us;
        stats->bands = HMI_TOUCH349_HEIGHT / HMI_TOUCH349_BAND_ROWS;
    }
    return ESP_OK;
}

int hmi_touch349_backlight_set(uint8_t duty, bool enabled) {
    ESP_RETURN_ON_FALSE(expander != NULL, ESP_ERR_INVALID_STATE, TAG, "expander missing");
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
