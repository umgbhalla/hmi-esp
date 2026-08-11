idf_component_register(
    SRCS "hmi_touch349.c"
    INCLUDE_DIRS "include"
    REQUIRES esp_lcd_axs15231b esp_io_expander_tca9554 esp_driver_i2c esp_driver_spi esp_driver_ledc esp_timer lwip
)
