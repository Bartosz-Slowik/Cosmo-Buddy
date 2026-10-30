//! Checks the microphone: `cargo run --release --example mic_test`
//! Shows the average mV on every analog pin, marks the one that looks like the mic (resting nearest
//! 1650 mV, half of 3.3 V) with `>` and draws its live waveform. Clap or talk and it should wiggle.
use embedded_graphics::mono_font::{ascii::FONT_5X8, MonoTextStyle};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Line, PrimitiveStyle};
use embedded_graphics::text::Text;
use esp_idf_svc::hal::adc::attenuation::DB_12;
use esp_idf_svc::hal::adc::oneshot::config::AdcChannelConfig;
use esp_idf_svc::hal::adc::oneshot::{AdcChannelDriver, AdcDriver};
use esp_idf_svc::hal::delay::Ets;
use esp_idf_svc::hal::i2c::{I2cConfig, I2cDriver};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::units::Hertz;
use log::info;
use ssd1306::prelude::*;
use ssd1306::{I2CDisplayInterface, Ssd1306};

/// The waveform fills the screen below the text, 0 V at the bottom and 3.3 V at the top
const SCOPE_TOP: i32 = 33;
const SCOPE_MV: f32 = 3300.0;

fn main() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    let p = Peripherals::take().unwrap();

    let i2c_config = I2cConfig::new().baudrate(Hertz(400_000));
    let i2c = I2cDriver::new(p.i2c0, p.pins.gpio44, p.pins.gpio7, &i2c_config).unwrap();
    let mut display = Ssd1306::new(I2CDisplayInterface::new(i2c), DisplaySize128x64, DisplayRotation::Rotate180)
        .into_buffered_graphics_mode();
    display.init().expect("OLED not answering on SDA=D7, SCL=D8");
    let text = MonoTextStyle::new(&FONT_5X8, BinaryColor::On);
    let ink = PrimitiveStyle::with_stroke(BinaryColor::On, 1);

    let adc = AdcDriver::new(p.adc1).unwrap();
    let config = AdcChannelConfig { attenuation: DB_12, ..Default::default() };
    // D6-D8 aren't analog or drive the OLED
    let mut d0 = AdcChannelDriver::new(&adc, p.pins.gpio1, &config).unwrap();
    let mut d1 = AdcChannelDriver::new(&adc, p.pins.gpio2, &config).unwrap();
    let mut d2 = AdcChannelDriver::new(&adc, p.pins.gpio3, &config).unwrap();
    let mut d3 = AdcChannelDriver::new(&adc, p.pins.gpio4, &config).unwrap();
    let mut d4 = AdcChannelDriver::new(&adc, p.pins.gpio5, &config).unwrap();
    let mut d5 = AdcChannelDriver::new(&adc, p.pins.gpio6, &config).unwrap();
    let mut d9 = AdcChannelDriver::new(&adc, p.pins.gpio8, &config).unwrap();
    let mut d10 = AdcChannelDriver::new(&adc, p.pins.gpio9, &config).unwrap();
    // 12.8 ms of waveform, enough for a few cycles of a voice or a whistle
    macro_rules! trace {
        ($pin:expr) => {{
            let mut trace = [0u16; 128];
            for t in &mut trace {
                *t = $pin.read().unwrap();
                Ets::delay_us(100);
            }
            trace
        }};
    }

    loop {
        let pins = [
            ("D0", trace!(d0)),
            ("D1", trace!(d1)),
            ("D2", trace!(d2)),
            ("D3", trace!(d3)),
            ("D4", trace!(d4)),
            ("D5", trace!(d5)),
            ("D9", trace!(d9)),
            ("D10", trace!(d10)),
        ];
        let average = |t: &[u16; 128]| t.iter().map(|&v| v as u32).sum::<u32>() / 128;
        let swing = |t: &[u16; 128]| t.iter().max().unwrap() - t.iter().min().unwrap();
        // The mic is the pin resting nearest half supply
        let (mic, trace) = pins.iter().min_by_key(|(_, t)| average(t).abs_diff(1650)).unwrap();
        let summary: Vec<_> = pins.iter().map(|(name, t)| (*name, average(t), swing(t))).collect();
        info!("(pin, average mV, swing mV): {summary:?}  mic looks like {mic}");

        display.clear_buffer();
        for (i, row) in summary.chunks(2).enumerate() {
            let line: String = row
                .iter()
                .map(|(name, avg, _)| format!("{}{name:<3} {avg:<5}", if name == mic { '>' } else { ' ' }))
                .collect();
            Text::new(&line, Point::new(0, 7 + i as i32 * 8), text).draw(&mut display).unwrap();
        }
        let y = |mv: u16| 63 - ((mv as f32 / SCOPE_MV).min(1.0) * (63 - SCOPE_TOP) as f32) as i32;
        for (x, pair) in trace.windows(2).enumerate() {
            let (a, b) = (Point::new(x as i32, y(pair[0])), Point::new(x as i32 + 1, y(pair[1])));
            Line::new(a, b).into_styled(ink).draw(&mut display).unwrap();
        }
        display.flush().unwrap();
    }
}
