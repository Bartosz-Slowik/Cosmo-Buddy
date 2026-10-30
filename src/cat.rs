//! The cat face: moods are set from the phone, the little animations (glancing, blinking, ear
//! flicks, dancing, purring when petted) run on their own.
use embedded_graphics::mono_font::{ascii::FONT_6X10, MonoTextStyle};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Circle, Ellipse, Line, PrimitiveStyle, Rectangle, Triangle};
use embedded_graphics::text::Text;

/// Frames (20 per second) a surprise lasts before the cat goes back to its previous mood
const SURPRISE: u32 = 40;
/// Frames of sleepiness before the eyes close
const DOZE_OFF: u32 = 100;
/// Frames between dance steps when happy without music
const DANCE_STEP: u32 = 10;
/// Frames of purring after each stroke
const PURR: u32 = 30;
const MAX_HEARTS: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mood {
    Idle,
    Happy,
    Surprised,
    Annoyed,
    Sleepy,
}

impl Mood {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "idle" => Self::Idle,
            "happy" => Self::Happy,
            "surprised" => Self::Surprised,
            "annoyed" => Self::Annoyed,
            "sleepy" => Self::Sleepy,
            _ => return None,
        })
    }
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

pub struct Cat {
    face: Face,
    mood: Mood,
    /// Mood to return to after a surprise
    resting: Mood,
    /// Frames since the mood last changed
    mood_age: u32,
    gaze: (f32, f32),
    next_glance: u32,
    /// Frames left of the current blink
    blink: u32,
    next_blink: u32,
    /// Ears and whiskers flick by this many pixels, then settle
    twitch: f32,
    bounce: f32,
    /// Frames of purring left
    purr: u32,
    /// Floating up: x, y
    hearts: Vec<(f32, f32)>,
    seed: u32,
    frame: u32,
}

impl Cat {
    pub fn new() -> Self {
        Self {
            face: Face { eye_w: 22.0, eye_h: 24.0, pupil: 4.0, ..Default::default() },
            mood: Mood::Idle,
            resting: Mood::Idle,
            mood_age: 0,
            gaze: (0.0, 0.0),
            next_glance: 0,
            blink: 0,
            next_blink: 40,
            twitch: 0.0,
            bounce: 0.0,
            purr: 0,
            hearts: Vec::new(),
            seed: unsafe { esp_idf_svc::sys::esp_random() } | 1,
            frame: 0,
        }
    }

    pub fn mood(&self) -> Mood {
        self.mood
    }

    pub fn set_mood(&mut self, mood: Mood) {
        if mood != Mood::Surprised {
            self.resting = mood;
        }
        if mood != self.mood {
            self.mood = mood;
            self.mood_age = 0;
        }
    }

    /// One stroke: purrs, sends up a heart, and calms a cross or sleepy cat
    pub fn pet(&mut self) {
        self.purr = PURR;
        if matches!(self.mood, Mood::Annoyed | Mood::Sleepy) {
            self.set_mood(Mood::Idle);
        }
        if self.hearts.len() < MAX_HEARTS {
            let x = 16.0 + self.random(96) as f32;
            self.hearts.push((x, 60.0));
        }
    }

    /// xorshift: 0..n
    fn random(&mut self, n: u32) -> u32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        self.seed % n
    }

    /// `music` is the loudness 0..1 and whether this frame is a beat, while the phone streams it
    pub fn update(&mut self, music: Option<(f32, bool)>) {
        self.frame = self.frame.wrapping_add(1);
        self.mood_age += 1;
        if self.mood == Mood::Surprised && self.mood_age > SURPRISE {
            self.set_mood(self.resting);
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

        // Without music a happy cat dances to its own beat
        let (level, beat) = music.unwrap_or_else(|| {
            let step = self.frame % DANCE_STEP;
            (if step < DANCE_STEP / 2 { 0.6 } else { 0.2 }, step == 0)
        });
        if beat && matches!(self.mood, Mood::Happy | Mood::Idle) {
            if self.mood == Mood::Happy {
                self.bounce = -5.0;
            }
            if self.mood == Mood::Happy || music.is_some() {
                self.twitch = 4.0;
            }
        } else if self.mood == Mood::Idle && self.random(100) == 0 {
            self.twitch = 3.0;
        }
        self.bounce *= 0.7;
        self.twitch *= 0.6;
        self.purr = self.purr.saturating_sub(1);
        for heart in &mut self.hearts {
            heart.1 -= 1.5;
        }
        self.hearts.retain(|&(_, y)| y > -8.0);

        let mut target = Face {
            eye_w: 22.0,
            eye_h: 24.0,
            pupil: 4.0,
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
        if self.purr > 0 {
            // Eyes closed in bliss, ears relaxed
            (target.happy, target.mouth, target.tired) = (1.0, 0.0, 0.0);
            (target.ear_out, target.ear_down) = (4.0, 3.0);
        }
        self.face.approach(target);
    }

    pub fn draw<D: DrawTarget<Color = BinaryColor>>(&self, d: &mut D) -> Result<(), D::Error> {
        let on = PrimitiveStyle::with_fill(BinaryColor::On);
        let off = PrimitiveStyle::with_fill(BinaryColor::Off);
        let thin = PrimitiveStyle::with_stroke(BinaryColor::On, 1);
        let thick = PrimitiveStyle::with_stroke(BinaryColor::On, 2);
        let pt = |x: f32, y: f32| Point::new(x as i32, y as i32);
        let f = self.face;
        // Purring is a gentle shiver
        let dy = self.bounce + if self.purr > 0 && self.frame % 2 == 0 { 1.0 } else { 0.0 };

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
        for &(x, y) in &self.hearts {
            for side in [-2.0, 2.0] {
                Circle::with_center(pt(x + side, y), 5).into_styled(on).draw(d)?;
            }
            Triangle::new(pt(x - 4.0, y + 1.0), pt(x + 4.0, y + 1.0), pt(x, y + 6.0)).into_styled(on).draw(d)?;
        }
        Ok(())
    }
}
