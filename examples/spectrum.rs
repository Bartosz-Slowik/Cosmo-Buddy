//! Keychain buddy: a music spectrum analyser on the OLED. 32 log-spaced bars from ~80 Hz to
//! 5 kHz with falling peak caps, auto-adjusting to the room's hiss and the music's volume.
//!
//! Wiring (Seeed XIAO ESP32S3):
//!
//!   XIAO               OLED 128x64 SSD1306, I2C address 0x3C
//!   3V3 -------------- VCC
//!   GND -------------- GND
//!   D7  (GPIO44) ----- SDA
//!   D8  (GPIO7)  ----- SCL
//!
//!   XIAO               Analog mic module (MAX9814 / MAX4466 style)
//!   3V3 -------------- VCC   (not 5 V: OUT must stay below 3.3 V for the ADC)
//!   GND -------------- GND
//!   D10 (GPIO9) ------ OUT   (ADC1 channel 8)
//!
//! Most OLED boards have I2C pull-ups on board; the ESP32's internal ones are enabled too.
//! SDA/SCL may be swapped and the address may be 0x3D instead: both are detected at boot.
use std::f32::consts::TAU;

use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use esp_idf_svc::hal::adc::attenuation::DB_12;
use esp_idf_svc::hal::adc::oneshot::config::AdcChannelConfig;
use esp_idf_svc::hal::adc::oneshot::{AdcChannelDriver, AdcDriver};
use esp_idf_svc::hal::i2c::{I2cConfig, I2cDriver};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::sys::esp_timer_get_time;
use log::info;
use microfft::Complex32;
use ssd1306::prelude::*;
use ssd1306::{I2CDisplayInterface, Ssd1306};

/// 256 samples at 10 kHz: 25.6 ms per frame, 39 Hz per FFT bin, up to 5 kHz
const SAMPLES: usize = 256;
const SAMPLE_US: i64 = 100;
const BARS: usize = 32;
/// Bin 2 is ~80 Hz; small mics hear little below that
const FIRST_BIN: usize = 2;
const HEIGHT: f32 = 64.0;
/// dB above the hiss a bar needs before it shows, so silence stays dark
const HISS_MARGIN: f32 = 6.0;
/// The loudest bar is drawn full height, but never for less than this many dB above the hiss
const MIN_SPAN: f32 = 24.0;

fn main() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    let p = Peripherals::take().unwrap();

    let i2c_config = I2cConfig::new().baudrate(Hertz(400_000));
    let (mut i2c0, mut d7, mut d8) = (p.i2c0, p.pins.gpio44, p.pins.gpio7);
    let mut found = None;
    'probe: for swapped in [false, true] {
        // Safety: the probe driver is dropped before the pins are used again
        let mut probe = unsafe {
            if swapped {
                I2cDriver::new(i2c0.reborrow(), d8.reborrow(), d7.reborrow(), &i2c_config)
            } else {
                I2cDriver::new(i2c0.reborrow(), d7.reborrow(), d8.reborrow(), &i2c_config)
            }
        }
        .unwrap();
        for address in [0x3C, 0x3D] {
            // 0x00 0xAE is the harmless "display off" command
            if probe.write(address, &[0x00, 0xAE], 10).is_ok() {
                found = Some((swapped, address));
                break 'probe;
            }
        }
    }
    let (swapped, address) =
        found.expect("No OLED answers on D7/D8 at 0x3C or 0x3D: check its VCC, GND and both data wires");
    info!("OLED at {address:#x}, SDA on {}", if swapped { "D8" } else { "D7" });

    let i2c = if swapped {
        I2cDriver::new(i2c0, d8, d7, &i2c_config)
    } else {
        I2cDriver::new(i2c0, d7, d8, &i2c_config)
    }
    .unwrap();
    let interface =
        if address == 0x3C { I2CDisplayInterface::new(i2c) } else { I2CDisplayInterface::new_alternate_address(i2c) };
    let mut display =
        Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate180).into_buffered_graphics_mode();
    display.init().unwrap();

    let adc = AdcDriver::new(p.adc1).unwrap();
    // 12 dB covers the mic's full 0-3.1 V swing
    let mic_config = AdcChannelConfig { attenuation: DB_12, ..Default::default() };
    let mut mic = AdcChannelDriver::new(&adc, p.pins.gpio9, &mic_config).unwrap();

    // Hann window: stops loud bars smearing into their neighbours
    let window: [f32; SAMPLES] =
        core::array::from_fn(|i| 0.5 - 0.5 * (TAU * i as f32 / (SAMPLES - 1) as f32).cos());
    let mut spectrum = Spectrum::new();
    let mut samples = [0f32; SAMPLES];
    let mut frame = 0u32;
    loop {
        // Evenly spaced samples, or the frequencies come out wrong
        let mut next = unsafe { esp_timer_get_time() };
        for s in &mut samples {
            while unsafe { esp_timer_get_time() } < next {}
            *s = mic.read_raw().unwrap() as f32;
            next += SAMPLE_US;
        }
        // The mic output sits at half supply; only the swing around it is sound
        let mean = samples.iter().sum::<f32>() / SAMPLES as f32;
        for (s, w) in samples.iter_mut().zip(&window) {
            *s = (*s - mean) * w;
        }
        spectrum.update(microfft::real::rfft_256(&mut samples));

        frame = frame.wrapping_add(1);
        if frame % 20 == 0 {
            info!("loudest bar {:.0} dB above the hiss", spectrum.span);
        }

        display.clear_buffer();
        spectrum.draw(&mut display).unwrap();
        display.flush().unwrap();
    }
}

struct Spectrum {
    /// FFT bins of bar `i` are `edges[i]..edges[i + 1]`
    edges: [usize; BARS + 1],
    /// Per-bar background hiss in dB
    hiss: [f32; BARS],
    heights: [f32; BARS],
    peaks: [f32; BARS],
    /// Frames a peak cap waits before it starts falling
    hold: [u32; BARS],
    /// dB above the hiss drawn as full height; follows the music's volume
    span: f32,
}

impl Spectrum {
    fn new() -> Self {
        let last = SAMPLES / 2;
        let mut edges: [usize; BARS + 1] = core::array::from_fn(|i| {
            let ratio = (last as f32 / FIRST_BIN as f32).powf(i as f32 / BARS as f32);
            (FIRST_BIN as f32 * ratio).round() as usize
        });
        // Low bars would share bins; give each at least one of its own
        for i in 1..=BARS {
            edges[i] = edges[i].max(edges[i - 1] + 1);
        }
        Self {
            edges,
            hiss: [f32::INFINITY; BARS],
            heights: [0.0; BARS],
            peaks: [0.0; BARS],
            hold: [0; BARS],
            span: MIN_SPAN,
        }
    }

    fn update(&mut self, bins: &[Complex32; SAMPLES / 2]) {
        let mut loudest = 0f32;
        let mut levels = [0f32; BARS];
        for (i, level) in levels.iter_mut().enumerate() {
            // Summed rather than averaged, so wide treble bars keep up with the bass in typical music
            let power: f32 =
                bins[self.edges[i]..self.edges[i + 1]].iter().map(|c| c.re * c.re + c.im * c.im).sum();
            let db = 10.0 * (power + 1e-6).log10();
            // Drops instantly but rises very slowly, so it settles on the quietest moments
            self.hiss[i] = if db < self.hiss[i] { db } else { self.hiss[i] + 0.005 };
            *level = (db - self.hiss[i] - HISS_MARGIN).max(0.0);
            loudest = loudest.max(*level);
        }
        self.span = (self.span - 0.2).max(loudest).max(MIN_SPAN);

        for i in 0..BARS {
            let target = (levels[i] / self.span * HEIGHT).min(HEIGHT);
            // Jump up, fall back smoothly
            self.heights[i] = target.max(self.heights[i] - 3.0);
            if self.heights[i] >= self.peaks[i] {
                self.peaks[i] = self.heights[i];
                self.hold[i] = 15;
            } else if self.hold[i] > 0 {
                self.hold[i] -= 1;
            } else {
                self.peaks[i] = (self.peaks[i] - 1.0).max(self.heights[i]);
            }
        }
    }

    fn draw<D: DrawTarget<Color = BinaryColor>>(&self, d: &mut D) -> Result<(), D::Error> {
        let on = PrimitiveStyle::with_fill(BinaryColor::On);
        let width = 128 / BARS as i32;
        for i in 0..BARS {
            let x = i as i32 * width;
            let h = self.heights[i] as u32;
            if h > 0 {
                Rectangle::new(Point::new(x, 64 - h as i32), Size::new(width as u32 - 1, h)).into_styled(on).draw(d)?;
            }
            let cap = (63 - self.peaks[i] as i32).max(0);
            Rectangle::new(Point::new(x, cap), Size::new(width as u32 - 1, 1)).into_styled(on).draw(d)?;
        }
        Ok(())
    }
}
