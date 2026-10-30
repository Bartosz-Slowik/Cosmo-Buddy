//! Finds where the OLED is wired: `cargo run --release --example i2c_scan`
//! Lists the XIAO pins held high by the OLED's pull-up resistors, then looks for it on every pin pair.
use std::thread;
use std::time::Duration;

use esp_idf_svc::hal::delay::Ets;
use esp_idf_svc::hal::gpio::{AnyIOPin, PinDriver, Pull};
use esp_idf_svc::hal::i2c::{I2cConfig, I2cDriver};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::units::Hertz;
use log::{info, warn};

const PINS: [(&str, u8); 11] = [
    ("D0", 1),
    ("D1", 2),
    ("D2", 3),
    ("D3", 4),
    ("D4", 5),
    ("D5", 6),
    ("D6", 43),
    ("D7", 44),
    ("D8", 7),
    ("D9", 8),
    ("D10", 9),
];

fn main() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    let mut i2c0 = Peripherals::take().unwrap().i2c0;
    let config = I2cConfig::new().baudrate(Hertz(100_000));

    loop {
        // Against the weak internal pull-down, only lines with the OLED's pull-ups read high
        let mut pulled_up = Vec::new();
        for (name, gpio) in PINS {
            // Safety: only one driver uses each pin at a time
            let pin = PinDriver::input(unsafe { AnyIOPin::steal(gpio) }, Pull::Down).unwrap();
            Ets::delay_us(100);
            if pin.is_high() {
                pulled_up.push((name, gpio));
            }
        }
        info!("Pulled-up pins: {:?}", pulled_up.iter().map(|p| p.0).collect::<Vec<_>>());
        if pulled_up.is_empty() {
            warn!("No pull-ups: the OLED is unpowered (check VCC/GND) or not on any D pin");
        }

        // Every pair, in case the OLED lacks its own pull-ups (the internal ones are enabled)
        let mut found = false;
        for (sda_name, sda) in PINS {
            for (scl_name, scl) in PINS {
                if sda == scl {
                    continue;
                }
                // Safety: the driver is dropped before the next pair is tried
                let mut i2c = unsafe {
                    I2cDriver::new(i2c0.reborrow(), AnyIOPin::steal(sda), AnyIOPin::steal(scl), &config)
                }
                .unwrap();
                for address in [0x3C, 0x3D] {
                    if i2c.write(address, &[0x00], 10).is_ok() {
                        info!("Device at {address:#04x} with SDA on {sda_name}, SCL on {scl_name}");
                        found = true;
                    }
                }
            }
        }
        if !found {
            warn!("No I2C device answered");
        }
        thread::sleep(Duration::from_secs(5));
    }
}
