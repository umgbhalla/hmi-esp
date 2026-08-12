set(CODEC_ROOT "${CMAKE_CURRENT_LIST_DIR}/../../../../vendor/upstream/waveshareteam__ESP32-S3-Touch-LCD-3.49-V2/Arduino/examples/08_Audio_Test/src/esp_codec_dev")

set(CODEC_SOURCES
    "${CODEC_ROOT}/esp_codec_dev.c"
    "${CODEC_ROOT}/esp_codec_dev_vol.c"
    "${CODEC_ROOT}/esp_codec_dev_if.c"
    "${CODEC_ROOT}/audio_codec_sw_vol.c"
    "${CODEC_ROOT}/platform/audio_codec_gpio.c"
    "${CODEC_ROOT}/platform/audio_codec_ctrl_i2c.c"
    "${CODEC_ROOT}/platform/audio_codec_data_i2s.c"
    "${CODEC_ROOT}/platform/audio_codec_ctrl_spi.c"
    "${CODEC_ROOT}/platform/esp_codec_dev_os.c"
    "${CODEC_ROOT}/device/es7210/es7210.c"
)

idf_component_register(
    SRCS ${CODEC_SOURCES}
    INCLUDE_DIRS
        "${CODEC_ROOT}/include"
        "${CODEC_ROOT}/interface"
        "${CODEC_ROOT}/device/include"
    PRIV_INCLUDE_DIRS "${CODEC_ROOT}/device/priv_include"
    REQUIRES driver esp_driver_i2c esp_driver_i2s
    PRIV_REQUIRES freertos
)
