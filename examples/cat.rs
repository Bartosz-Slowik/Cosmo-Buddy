//! Keychain buddy: a cat face on the OLED with moods driven by what the mic hears.
//! Run with `cargo run --release --example cat`
//!   idle      looks around, blinks, flicks an ear now and then
//!   happy     steady beats (music): ^ ^ eyes, bobs its head and sings, ears and whiskers twitch
//!   surprised a sudden bang in a quiet room: wide eyes, big pupils, ears up, "o" mouth
//!   annoyed   a few seconds of loud noise with no rhythm: slit pupils, frown, flat ears, hiss
//!   sleepy    ~10 s of quiet: lids droop, then close
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
use embedded_graphics::mono_font::{ascii::FONT_6X10, MonoTextStyle};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Circle, Ellipse, Line, PrimitiveStyle, Rectangle, Triangle};
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

/// One frame of sound is ~25 ms; with drawing the buddy runs at ~20 frames per second
const SAMPLES: usize = 200;
const SAMPLE_GAP_US: u32 = 100;
/// Loudness (ADC counts above the background hiss) treated as full volume in a quiet room, so the
/// auto-gain doesn't blow up noise. Raise it if the buddy dances in silence (see the logged `rms`)
const MIN_PEAK: f32 = 40.0;
/// ~10 s of quiet before the buddy gets sleepy, and ~5 s more until its eyes close
const SLEEP_AFTER: u32 = 200;
const DOZE_OFF: u32 = 100;

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

    let mut ears =
        Ears { floor: f32::INFINITY, peak: MIN_PEAK, average: 0.0, since_beat: 0, last_gap: 0, groove: 0 };
    let mut buddy = Buddy::new();
    let mut samples = [0u16; SAMPLES];
    loop {
        for s in &mut samples {
            *s = mic.read_raw().unwrap();
            Ets::delay_us(SAMPLE_GAP_US);
        }
        // The mic output sits at half supply; only the swing around it is sound
        let mean = samples.iter().map(|&s| s as f32).sum::<f32>() / SAMPLES as f32;
        let rms = (samples.iter().map(|&s| (s as f32 - mean).powi(2)).sum::<f32>() / SAMPLES as f32).sqrt();

        let hearing = ears.hear(rms);
        buddy.update(&hearing);
        if buddy.frame % 20 == 0 {
            info!(
                "rms {rms:.0}  hiss {:.0}  level {:.2}  groove {}  mood {:?}",
                ears.floor, hearing.level, hearing.groove, buddy.mood
            );
        }

        display.clear_buffer();
        buddy.draw(&mut display).unwrap();
        display.flush().unwrap();
    }
}

/// What the buddy heard in one frame
struct Hearing {
    /// 0..1 relative to recent loudest moment
    level: f32,
    /// Loudness above the hiss in units of `MIN_PEAK`, independent of the auto-gain
    loud: f32,
    beat: bool,
    /// A bang out of a quiet room
    startle: bool,
    /// Evenly spaced beats in a row; 3 or more means music
    groove: u32,
}

/// Turns loudness into a level, beats and rhythm, whatever the volume
struct Ears {
    /// Background hiss
    floor: f32,
    /// Recent loudest moment, for auto-gain
    peak: f32,
    /// Recent average loudness; a frame well above it is a beat
    average: f32,
    since_beat: u32,
    last_gap: u32,
    groove: u32,
}

impl Ears {
    fn hear(&mut self, rms: f32) -> Hearing {
        // Drops instantly but rises very slowly, so it settles on the quietest moments
        self.floor = if rms < self.floor { rms } else { self.floor + (rms - self.floor) * 0.0005 };
        let sound = rms - self.floor;
        self.peak = (self.peak * 0.995).max(sound).max(MIN_PEAK);
        let startle = sound > MIN_PEAK * 1.5 && self.average < MIN_PEAK * 0.3;
        self.since_beat += 1;
        // At most ~3 beats a second
        let beat = sound > self.average * 1.5 && sound > MIN_PEAK / 2.0 && self.since_beat > 5;
        if beat {
            // Gaps within ~150 ms of the last one mean music rather than talking or clatter
            self.groove = if self.since_beat.abs_diff(self.last_gap) <= 3 {
                (self.groove + 1).min(8)
            } else {
                self.groove.saturating_sub(2)
            };
            self.last_gap = self.since_beat;
            self.since_beat = 0;
        } else if self.since_beat > 60 {
            self.groove = 0;
        }
        self.average += (sound - self.average) * 0.1;
        Hearing { level: sound / self.peak, loud: sound / MIN_PEAK, beat, startle, groove: self.groove }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mood {
    Idle,
    Happy,
    Surprised,
    Annoyed,
    Sleepy,
}

/// Face shape; the drawn face eases towards the shape the mood asks for
#[derive(Clone, Copy, Default)]
struct Face {
    eye_w: f32,
    eye_h: f32,
    /// Pupil width: a thin slit when cross, wide and round when excited
    pupil: f32,
    /// Gaze, -1..1
    look_x: f32,
    look_y: f32,
    /// Eyelid amounts, 0..1
    tired: f32,
    angry: f32,
    happy: f32,
    /// Ear tips move outwards and down to flatten, negative to perk up
    ear_out: f32,
    ear_down: f32,
    /// 0 is a closed :3 mouth
    mouth: f32,
}

impl Face {
    fn approach(&mut self, target: Face) {
        let ease = |v: &mut f32, t: f32| *v += (t - *v) * 0.3;
        ease(&mut self.eye_w, target.eye_w);
        ease(&mut self.eye_h, target.eye_h);
        ease(&mut self.pupil, target.pupil);
        ease(&mut self.look_x, target.look_x);
        ease(&mut self.look_y, target.look_y);
        ease(&mut self.angry, target.angry);
        ease(&mut self.happy, target.happy);
        ease(&mut self.ear_out, target.ear_out);
        ease(&mut self.ear_down, target.ear_down);
        ease(&mut self.mouth, target.mouth);
        // Eyelids fall slowly, like really getting tired
        self.tired += (target.tired - self.tired) * 0.06;
    }
}

struct Buddy {
    face: Face,
    mood: Mood,
    /// Frames since the mood last changed
    mood_age: u32,
    quiet: u32,
    /// Rises with loud noise that has no rhythm, falls otherwise
    noisy: u32,
    gaze: (f32, f32),
    next_glance: u32,
    /// Frames left of the current blink
    blink: u32,
    next_blink: u32,
    /// Ears and whiskers flick by this many pixels, then settle
    twitch: f32,
    bounce: f32,
    seed: u32,
    frame: u32,
}

impl Buddy {
    fn new() -> Self {
        Self {
            face: Face { eye_w: 22.0, eye_h: 24.0, pupil: 4.0, ..Default::default() },
            mood: Mood::Idle,
            mood_age: 0,
            quiet: 0,
            noisy: 0,
            gaze: (0.0, 0.0),
            next_glance: 0,
            blink: 0,
            next_blink: 40,
            twitch: 0.0,
            bounce: 0.0,
            seed: unsafe { esp_idf_svc::sys::esp_random() } | 1,
            frame: 0,
        }
    }

    /// xorshift: 0..n
    fn random(&mut self, n: u32) -> u32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        self.seed % n
    }

    fn update(&mut self, hearing: &Hearing) {
        self.frame = self.frame.wrapping_add(1);
        self.quiet = if hearing.loud < 0.25 { self.quiet + 1 } else { 0 };
        self.noisy = if hearing.loud > 1.0 && hearing.groove < 3 {
            self.noisy + 1
        } else {
            self.noisy.saturating_sub(1)
        };

        let mood = if hearing.startle || (self.mood == Mood::Surprised && self.mood_age < 25) {
            Mood::Surprised
        } else if hearing.groove >= 3 {
            Mood::Happy
        } else if self.noisy > 60 || (self.mood == Mood::Annoyed && self.noisy > 20) {
            Mood::Annoyed
        } else if self.quiet > SLEEP_AFTER {
            Mood::Sleepy
        } else {
            Mood::Idle
        };
        if mood == self.mood {
            self.mood_age += 1;
        } else {
            self.mood = mood;
            self.mood_age = 0;
        }

        if self.next_glance == 0 {
            self.gaze = ((self.random(21) as f32 - 10.0) / 10.0, (self.random(21) as f32 - 10.0) / 10.0);
            self.next_glance = 20 + self.random(60);
        } else {
            self.next_glance -= 1;
        }
        if self.blink > 0 {
            self.blink -= 1;
        } else if self.next_blink == 0 {
            self.blink = 3;
            self.next_blink = 40 + self.random(80);
        } else {
            self.next_blink -= 1;
        }
        if hearing.beat && self.mood == Mood::Happy {
            self.bounce = -5.0;
            self.twitch = 4.0;
        } else if self.mood == Mood::Idle && self.random(100) == 0 {
            self.twitch = 3.0;
        }
        self.bounce *= 0.7;
        self.twitch *= 0.6;

        let level = hearing.level;
        let mut target = Face {
            eye_w: 22.0,
            eye_h: 24.0,
            pupil: 4.0 + level * 6.0,
            look_x: self.gaze.0,
            look_y: self.gaze.1,
            ..Default::default()
        };
        match self.mood {
            Mood::Idle => {}
            Mood::Happy => {
                target.happy = 1.0;
                target.mouth = level;
            }
            Mood::Surprised => {
                (target.eye_w, target.eye_h, target.pupil) = (26.0, 28.0, 14.0);
                (target.look_x, target.look_y) = (0.0, 0.0);
                (target.ear_out, target.ear_down) = (-2.0, -1.0);
                target.mouth = 0.6;
            }
            Mood::Annoyed => {
                (target.eye_h, target.pupil) = (18.0, 2.0);
                target.angry = 1.0;
                (target.ear_out, target.ear_down) = (10.0, 9.0);
                target.mouth = 0.3;
            }
            Mood::Sleepy => {
                target.tired = if self.mood_age > DOZE_OFF { 1.0 } else { 0.5 };
                (target.pupil, target.look_x, target.look_y) = (6.0, 0.0, 0.0);
                (target.ear_out, target.ear_down) = (3.0, 4.0);
            }
        }
        self.face.approach(target);
    }

    fn draw<D: DrawTarget<Color = BinaryColor>>(&self, d: &mut D) -> Result<(), D::Error> {
        let on = PrimitiveStyle::with_fill(BinaryColor::On);
        let off = PrimitiveStyle::with_fill(BinaryColor::Off);
        let thin = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
        let thick = PrimitiveStyle::with_stroke(BinaryColor::On, 2);
        let pt = |x: f32, y: f32| Point::new(x as i32, y as i32);
        let f = self.face;
        let dy = self.bounce;

        for side in [-1.0, 1.0] {
            let x = |from_centre: f32| 64.0 + side * from_centre;
            let tip = pt(x(42.0 + f.ear_out + self.twitch), 1.0 + f.ear_down + dy);
            Triangle::new(pt(x(52.0), 18.0 + dy), tip, pt(x(24.0), 12.0 + dy)).into_styled(thick).draw(d)?;

            let (ex, ey, w, h) = (x(26.0), 32.0 + dy, f.eye_w, f.eye_h);
            let (left, right, top) = (ex - w / 2.0, ex + w / 2.0, ey - h / 2.0);
            if f.happy > 0.5 {
                Line::new(pt(left, ey + 3.0), pt(ex, ey - 4.0)).into_styled(thick).draw(d)?;
                Line::new(pt(ex, ey - 4.0), pt(right, ey + 3.0)).into_styled(thick).draw(d)?;
            } else if self.blink > 0 && self.mood != Mood::Sleepy {
                Line::new(pt(left, ey), pt(right, ey)).into_styled(thick).draw(d)?;
            } else {
                Ellipse::with_center(pt(ex, ey), Size::new(w as u32, h as u32)).into_styled(on).draw(d)?;
                let room = ((w - f.pupil) / 2.0 - 2.0).max(0.0);
                let (px, py) = (ex + f.look_x * room, ey + f.look_y * 2.0);
                let pupil = Size::new(f.pupil as u32, (h - 6.0).max(f.pupil) as u32);
                Ellipse::with_center(pt(px, py), pupil).into_styled(off).draw(d)?;
                if f.pupil >= 6.0 {
                    Circle::with_center(pt(px - f.pupil / 4.0, py - h / 5.0), 3).into_styled(on).draw(d)?;
                }
                if f.tired > 0.05 {
                    let lid = Size::new(w as u32 + 2, (h * f.tired) as u32 + 1);
                    Rectangle::new(pt(left - 1.0, top - 1.0), lid).into_styled(off).draw(d)?;
                    if f.tired > 0.9 {
                        let lash = ey + h / 2.0 - 1.0;
                        Line::new(pt(left, lash), pt(right, lash)).into_styled(thick).draw(d)?;
                    }
                }
                // Frowning lids, lowest at the inner corners
                if f.angry > 0.05 {
                    let (inner, outer) = if side < 0.0 { (right + 1.0, left - 1.0) } else { (left - 1.0, right + 1.0) };
                    let corner = pt(inner, top + h * 0.6 * f.angry);
                    Triangle::new(pt(inner, top - 1.0), pt(outer, top - 1.0), corner).into_styled(off).draw(d)?;
                }
            }

            for (i, tilt) in [-4.0, 0.0, 4.0].into_iter().enumerate() {
                let y = 48.0 + dy + i as f32 * 3.0;
                Line::new(pt(x(16.0), y), pt(x(46.0), y + tilt - self.twitch)).into_styled(thin).draw(d)?;
            }
        }

        let my = 44.0 + dy;
        if f.mouth > 0.15 {
            // Singing, gasping or hissing
            let open = Size::new((6.0 + f.mouth * 6.0) as u32, (2.0 + f.mouth * 10.0) as u32);
            Ellipse::new(pt(64.0 - open.width as f32 / 2.0, my), open).into_styled(thin).draw(d)?;
        } else {
            // :3 mouth, the bottom halves of two little circles
            for side in [-1.0, 1.0] {
                Circle::with_center(pt(64.0 + side * 3.0, my), 7).into_styled(thin).draw(d)?;
            }
            Rectangle::new(pt(58.0, my - 4.0), Size::new(13, 4)).into_styled(off).draw(d)?;
        }
        Triangle::new(pt(61.0, my - 4.0), pt(67.0, my - 4.0), pt(64.0, my - 1.0)).into_styled(on).draw(d)?;

        if self.mood == Mood::Sleepy && self.mood_age > DOZE_OFF {
            let style = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
            let drift = (self.frame / 8 % 4) as i32;
            Text::new("z", Point::new(70, 15 - drift), style).draw(d)?;
            Text::new("Z", Point::new(78, 8 - drift), style).draw(d)?;
        }
        Ok(())
    }
}
