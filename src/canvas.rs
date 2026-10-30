//! 128x64 1-bit framebuffer: each frame is drawn here, then copied to the OLED and mirrored to
//! the phone.
use core::convert::Infallible;

use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;

pub const WIDTH: usize = 128;
pub const HEIGHT: usize = 64;

/// Rows top to bottom, 16 bytes each, leftmost pixel in the top bit
pub struct Canvas(pub [u8; WIDTH * HEIGHT / 8]);

impl Canvas {
    pub fn new() -> Self {
        Self([0; WIDTH * HEIGHT / 8])
    }

    pub fn blank(&mut self) {
        self.0.fill(0);
    }

    pub fn pixel(&self, x: usize, y: usize) -> bool {
        self.0[y * WIDTH / 8 + x / 8] & (0x80 >> (x % 8)) != 0
    }
}

impl OriginDimensions for Canvas {
    fn size(&self) -> Size {
        Size::new(WIDTH as u32, HEIGHT as u32)
    }
}

impl DrawTarget for Canvas {
    type Color = BinaryColor;
    type Error = Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<BinaryColor>>>(&mut self, pixels: I) -> Result<(), Infallible> {
        for Pixel(p, color) in pixels {
            if (0..WIDTH as i32).contains(&p.x) && (0..HEIGHT as i32).contains(&p.y) {
                let (x, y) = (p.x as usize, p.y as usize);
                let byte = &mut self.0[y * WIDTH / 8 + x / 8];
                let bit = 0x80 >> (x % 8);
                if color.is_on() {
                    *byte |= bit;
                } else {
                    *byte &= !bit;
                }
            }
        }
        Ok(())
    }
}
