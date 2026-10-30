//! Keychain buddy: a cat on the OLED that lives in your phone too. Over Bluetooth LE the phone
//! page (index.html; Chrome on Android, or the Bluefy browser on iPhone) shows a live copy of
//! the screen you can rub to pet the cat, and switches between modes:
//!   pet     the cat, with moods set from the phone
//!   song    the Spotify song playing on the phone's account, and what's next
//!   lyrics  that song's lyrics, line by line in time with the music
//!   bars    music visualizer from the phone's microphone
//!   focus   a focus timer; the cat naps until break time
//! A battery icon shows the charge when the battery divider below is fitted.
//!
//! Wiring (Seeed XIAO ESP32S3):
//!
//!   XIAO               OLED 128x64 SSD1306, I2C address 0x3C
//!   3V3 -------------- VCC
//!   GND -------------- GND
//!   D7  (GPIO44) ----- SDA
//!   D8  (GPIO7)  ----- SCL
//!
//!   Battery: LiPo on the BAT+/BAT- pads under the XIAO, and a divider that halves its voltage
//!   BAT+ ---[220k]--- D0 (GPIO1) ---[220k]--- GND      (optional 100 nF from D0 to GND steadies it)
//!
//! Most OLED boards have I2C pull-ups on board; the ESP32's internal ones are enabled too.
//! SDA/SCL may be swapped and the address may be 0x3D instead: both are detected at boot.
//!
//! The phone writes commands to one characteristic; the first byte says what follows:
//!   M<mood>     idle | happy | surprised | annoyed | sleepy (switches to the cat)
//!   P           one stroke of petting (switches to the cat)
//!   T<text>     message that scrolls across whatever is showing
//!   V<mode>     pet | song | lyrics | bars | focus
//!   S<song>     title \n artist \n next song \n progress ms \n duration ms \n playing 0/1,
//!               or nothing when nothing is playing
//!   L<line>     current lyrics line, empty between verses
//!   F<seconds>  start a focus timer; 0 stops it
//!   B<32 bytes> visualizer bar heights, 0..64
//! and is notified of the battery (percent or 255, then millivolts as u16 LE) and of the screen
//! (8 packets per frame: row block index, then 8 rows of 16 bytes, leftmost pixel in the top bit).
mod app;
mod ble;
mod canvas;
mod cat;

use std::thread;
use std::time::{Duration, Instant};

use esp_idf_svc::hal::adc::attenuation::DB_12;
use esp_idf_svc::hal::adc::oneshot::config::AdcChannelConfig;
use esp_idf_svc::hal::adc::oneshot::{AdcChannelDriver, AdcDriver};
use esp_idf_svc::hal::i2c::{I2cConfig, I2cDriver};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::units::Hertz;
use log::info;
use ssd1306::prelude::*;
use ssd1306::{I2CDisplayInterface, Ssd1306};

use app::App;
use ble::Ble;
use canvas::{Canvas, HEIGHT, WIDTH};

const FRAME: Duration = Duration::from_millis(50);
/// Every 4th frame (5 per second) goes to the phone
const MIRROR_EVERY: u32 = 4;
const BATTERY_EVERY: Duration = Duration::from_secs(5);

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
    let battery_config = AdcChannelConfig { attenuation: DB_12, ..Default::default() };
    let mut battery_pin = AdcChannelDriver::new(&adc, p.pins.gpio1, &battery_config).unwrap();

    let ble = Ble::start();
    info!("Advertising as \"Buddy\"");

    let mut app = App::new();
    let mut canvas = Canvas::new();
    let mut battery_at: Option<Instant> = None;
    let mut next_frame = Instant::now();
    loop {
        while let Ok(event) = ble.events.try_recv() {
            app.handle(event);
        }
        if battery_at.is_none_or(|at| at.elapsed() > BATTERY_EVERY) {
            battery_at = Some(Instant::now());
            // The divider halves the battery voltage
            let mv = (0..16).map(|_| battery_pin.read().unwrap() as u32).sum::<u32>() / 16 * 2;
            app.battery = battery_percent(mv);
            ble.set_battery(app.battery, mv as u16);
        }

        app.tick();
        canvas.blank();
        app.draw(&mut canvas).unwrap();
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                display.set_pixel(x as u32, y as u32, canvas.pixel(x, y));
            }
        }
        display.flush().unwrap();
        if app.frame % MIRROR_EVERY == 0 {
            ble.mirror(&canvas.0);
        }

        next_frame += FRAME;
        match next_frame.checked_duration_since(Instant::now()) {
            Some(wait) => thread::sleep(wait),
            None => next_frame = Instant::now(),
        }
    }
}

/// Rough charge of a resting LiPo from its voltage; `None` when no battery divider is fitted
fn battery_percent(mv: u32) -> Option<u8> {
    const CURVE: [(u32, u32); 8] =
        [(3300, 0), (3500, 5), (3600, 12), (3700, 30), (3800, 48), (3900, 64), (4000, 80), (4200, 100)];
    if mv < 2500 {
        return None;
    }
    let percent = match CURVE.iter().position(|&(v, _)| mv < v) {
        Some(0) => 0,
        Some(i) => {
            let ((v0, p0), (v1, p1)) = (CURVE[i - 1], CURVE[i]);
            p0 + (mv - v0) * (p1 - p0) / (v1 - v0)
        }
        None => 100,
    };
    Some(percent as u8)
}
