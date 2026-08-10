#![no_std]

use embedded_graphics_core::{
    draw_target::DrawTarget,
    geometry::{OriginDimensions, Size},
    pixelcolor::BinaryColor,
    prelude::{Pixel, Point},
};
use embedded_hal::{delay::DelayNs, digital::OutputPin, spi::SpiDevice};

pub const WIDTH: u32 = 300;
pub const HEIGHT: u32 = 400;
pub const FRAME_BYTES: usize = 15_000;

#[derive(Clone)]
pub struct FrameBuffer {
    bytes: [u8; FRAME_BYTES],
}

impl Default for FrameBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameBuffer {
    pub const fn new() -> Self {
        Self {
            bytes: [0xff; FRAME_BYTES],
        }
    }

    pub fn bytes(&self) -> &[u8; FRAME_BYTES] {
        &self.bytes
    }

    pub fn clear_white(&mut self) {
        self.bytes.fill(0xff);
    }

    pub fn set_pixel(&mut self, point: Point, color: BinaryColor) {
        if point.x < 0 || point.y < 0 || point.x >= WIDTH as i32 || point.y >= HEIGHT as i32 {
            return;
        }
        let x = point.x as usize;
        let y = point.y as usize;
        let index = (y >> 1) * (WIDTH as usize >> 2) + (x >> 2);
        let bit = 7 - (((x & 3) << 1) | (y & 1));
        let mask = 1u8 << bit;
        match color {
            BinaryColor::On => self.bytes[index] |= mask, // white panel background
            BinaryColor::Off => self.bytes[index] &= !mask, // black ink
        }
    }
}

impl OriginDimensions for FrameBuffer {
    fn size(&self) -> Size {
        Size::new(WIDTH, HEIGHT)
    }
}

impl DrawTarget for FrameBuffer {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            self.set_pixel(point, color);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum Error<SpiError, PinError> {
    Spi(SpiError),
    Pin(PinError),
}

pub struct St7305<SPI, DC, RST> {
    spi: SPI,
    dc: DC,
    reset: RST,
}

impl<SPI, DC, RST, SpiError, PinError> St7305<SPI, DC, RST>
where
    SPI: SpiDevice<u8, Error = SpiError>,
    DC: OutputPin<Error = PinError>,
    RST: OutputPin<Error = PinError>,
{
    pub fn new(spi: SPI, dc: DC, reset: RST) -> Self {
        Self { spi, dc, reset }
    }

    pub fn init<D: DelayNs>(&mut self, delay: &mut D) -> Result<(), Error<SpiError, PinError>> {
        self.reset.set_high().map_err(Error::Pin)?;
        delay.delay_ms(50);
        self.reset.set_low().map_err(Error::Pin)?;
        delay.delay_ms(20);
        self.reset.set_high().map_err(Error::Pin)?;
        delay.delay_ms(50);

        // Hide undefined panel RAM while the controller is configured. The
        // caller writes a known-white frame before enabling the display.
        self.command(0x28)?;

        self.command_data(0xd6, &[0x17, 0x02])?;
        self.command_data(0xd1, &[0x01])?;
        self.command_data(0xc0, &[0x11, 0x04])?;
        self.command_data(0xc1, &[0x69, 0x69, 0x69, 0x69])?;
        self.command_data(0xc2, &[0x19, 0x19, 0x19, 0x19])?;
        self.command_data(0xc4, &[0x4b, 0x4b, 0x4b, 0x4b])?;
        self.command_data(0xc5, &[0x19, 0x19, 0x19, 0x19])?;
        self.command_data(0xd8, &[0x80, 0xe9])?;
        self.command_data(0xb2, &[0x02])?;
        self.command_data(
            0xb3,
            &[0xe5, 0xf6, 0x05, 0x46, 0x77, 0x77, 0x77, 0x77, 0x76, 0x45],
        )?;
        self.command_data(0xb4, &[0x05, 0x46, 0x77, 0x77, 0x77, 0x77, 0x76, 0x45])?;
        self.command_data(0x62, &[0x32, 0x03, 0x1f])?;
        self.command_data(0xb7, &[0x13])?;
        self.command_data(0xb0, &[0x64])?;
        self.command(0x11)?;
        delay.delay_ms(200);
        self.command_data(0xc9, &[0x00])?;
        self.command_data(0x36, &[0x48])?;
        self.command_data(0x3a, &[0x11])?;
        self.command_data(0xb9, &[0x20])?;
        self.command_data(0xb8, &[0x29])?;
        self.command(0x21)?;
        self.command_data(0x2a, &[0x12, 0x2a])?;
        self.command_data(0x2b, &[0x00, 0xc7])?;
        self.command_data(0x35, &[0x00])?;
        self.command_data(0xd0, &[0xff])?;
        self.command(0x38)?;
        Ok(())
    }

    pub fn display_on(&mut self) -> Result<(), Error<SpiError, PinError>> {
        self.command(0x29)
    }

    pub fn flush(&mut self, frame: &FrameBuffer) -> Result<(), Error<SpiError, PinError>> {
        self.command_data(0x2a, &[0x12, 0x2a])?;
        self.command_data(0x2b, &[0x00, 0xc7])?;
        self.command(0x2c)?;
        self.dc.set_high().map_err(Error::Pin)?;
        self.spi.write(frame.bytes()).map_err(Error::Spi)
    }

    fn command(&mut self, command: u8) -> Result<(), Error<SpiError, PinError>> {
        self.dc.set_low().map_err(Error::Pin)?;
        self.spi.write(&[command]).map_err(Error::Spi)
    }

    fn command_data(&mut self, command: u8, data: &[u8]) -> Result<(), Error<SpiError, PinError>> {
        self.command(command)?;
        self.dc.set_high().map_err(Error::Pin)?;
        self.spi.write(data).map_err(Error::Spi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framebuffer_is_exact_panel_size_and_white_by_default() {
        let frame = FrameBuffer::new();
        assert_eq!(frame.bytes().len(), 15_000);
        assert!(frame.bytes().iter().all(|byte| *byte == 0xff));
    }

    #[test]
    fn portrait_packing_matches_native_panel_mapping() {
        let mut frame = FrameBuffer::new();
        frame.set_pixel(Point::new(0, 0), BinaryColor::Off);
        assert_eq!(frame.bytes()[0], 0x7f);
        frame.set_pixel(Point::new(1, 0), BinaryColor::Off);
        assert_eq!(frame.bytes()[0], 0x5f);
        frame.set_pixel(Point::new(0, 1), BinaryColor::Off);
        assert_eq!(frame.bytes()[0], 0x1f);
        frame.set_pixel(Point::new(299, 399), BinaryColor::Off);
        assert_eq!(frame.bytes()[FRAME_BYTES - 1], 0xfe);
    }

    #[test]
    fn out_of_bounds_pixels_are_ignored() {
        let mut frame = FrameBuffer::new();
        frame.set_pixel(Point::new(-1, 0), BinaryColor::Off);
        frame.set_pixel(Point::new(300, 0), BinaryColor::Off);
        assert!(frame.bytes().iter().all(|byte| *byte == 0xff));
    }
}
