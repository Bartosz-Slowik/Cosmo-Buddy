//! What's on screen: the modes, and the phone's commands that drive them.
use std::time::{Duration, Instant};

use embedded_graphics::mono_font::ascii::{FONT_10X20, FONT_6X10, FONT_9X15_BOLD};
use embedded_graphics::mono_font::{MonoFont, MonoTextStyle};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle, RoundedRectangle};
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};
use log::{info, warn};

use crate::ble::Event;
use crate::cat::{Cat, Mood};

const BARS: usize = 32;
/// Visualizer data older than this means the phone stopped streaming
const BARS_TIMEOUT: Duration = Duration::from_secs(1);
/// With nothing from the phone for this long the cat dozes off
const BORED: Duration = Duration::from_secs(120);
const MESSAGE_PASSES: u32 = 2;
/// Pixels per frame
const MESSAGE_SPEED: u32 = 3;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Pet,
    Song,
    Lyrics,
    Bars,
    Focus,
}

struct Song {
    title: String,
    artist: String,
    next: String,
    progress_ms: u32,
    duration_ms: u32,
    playing: bool,
    received: Instant,
}

impl Song {
    fn parse(text: &str) -> Option<Self> {
        let mut fields = text.split('\n');
        Some(Self {
            title: fields.next()?.into(),
            artist: fields.next()?.into(),
            next: fields.next()?.into(),
            progress_ms: fields.next()?.parse().ok()?,
            duration_ms: fields.next()?.parse().ok()?,
            playing: fields.next()? == "1",
            received: Instant::now(),
        })
    }

    /// 0..1, moving on between the phone's updates while playing
    fn progress(&self) -> f32 {
        let played = if self.playing { self.received.elapsed().as_millis() as u32 } else { 0 };
        ((self.progress_ms + played) as f32 / self.duration_ms.max(1) as f32).min(1.0)
    }
}

struct Focus {
    started: Instant,
    length: Duration,
}

pub struct App {
    cat: Cat,
    mode: Mode,
    /// Text scrolling over the screen, and frames it has been showing
    message: Option<(String, u32)>,
    song: Option<Song>,
    /// Current lyrics line; empty between verses
    lyric: String,
    focus: Option<Focus>,
    bars: [u8; BARS],
    bars_at: Option<Instant>,
    /// Peak caps in bar units, falling slowly
    peaks: [f32; BARS],
    /// Recent visualizer loudness; a frame well above it is a beat for the cat
    average: f32,
    since_beat: u32,
    pub battery: Option<u8>,
    last_contact: Instant,
    pub frame: u32,
}

impl App {
    pub fn new() -> Self {
        Self {
            cat: Cat::new(),
            mode: Mode::Pet,
            message: None,
            song: None,
            lyric: String::new(),
            focus: None,
            bars: [0; BARS],
            bars_at: None,
            peaks: [0.0; BARS],
            average: 0.0,
            since_beat: 0,
            battery: None,
            last_contact: Instant::now(),
            frame: 0,
        }
    }

    pub fn handle(&mut self, event: Event) {
        self.last_contact = Instant::now();
        match event {
            Event::Connected => {
                info!("Phone connected");
                if self.focus.is_none() {
                    self.cat.set_mood(Mood::Idle);
                    self.cat.set_mood(Mood::Surprised);
                }
            }
            Event::Disconnected => info!("Phone disconnected"),
            Event::Command(data) => self.command(&data),
        }
    }

    fn command(&mut self, data: &[u8]) {
        let Some((&kind, payload)) = data.split_first() else { return };
        let text = String::from_utf8_lossy(payload);
        match kind {
            b'M' => match Mood::from_name(&text) {
                Some(mood) => {
                    self.cat.set_mood(mood);
                    self.mode = Mode::Pet;
                }
                None => warn!("Unknown mood {text:?}"),
            },
            b'P' => {
                self.cat.pet();
                self.mode = Mode::Pet;
            }
            b'T' => self.message = Some((text.into_owned(), 0)),
            b'V' => match &*text {
                "pet" => self.mode = Mode::Pet,
                "song" => self.mode = Mode::Song,
                "lyrics" => self.mode = Mode::Lyrics,
                "bars" => self.mode = Mode::Bars,
                "focus" => self.mode = Mode::Focus,
                _ => warn!("Unknown mode {text:?}"),
            },
            b'S' => self.song = Song::parse(&text),
            b'L' => self.lyric = text.into_owned(),
            b'F' => match text.parse::<u64>() {
                Ok(0) => self.focus = None,
                Ok(seconds) => {
                    self.focus = Some(Focus { started: Instant::now(), length: Duration::from_secs(seconds) });
                    self.mode = Mode::Focus;
                    // It naps while you work
                    self.cat.set_mood(Mood::Sleepy);
                }
                Err(_) => warn!("Bad focus length {text:?}"),
            },
            b'B' if payload.len() == BARS => {
                self.bars.copy_from_slice(payload);
                self.bars_at = Some(Instant::now());
            }
            _ => warn!("Unknown command {:?}", String::from_utf8_lossy(data)),
        }
    }

    fn streaming(&self) -> bool {
        self.bars_at.is_some_and(|at| at.elapsed() < BARS_TIMEOUT)
    }

    pub fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        if let Some((text, frames)) = &mut self.message {
            *frames += 1;
            let pass = (128 + text.len() as u32 * 10) / MESSAGE_SPEED;
            if *frames > pass * MESSAGE_PASSES {
                self.message = None;
            }
        }
        if self.focus.as_ref().is_some_and(|f| f.started.elapsed() >= f.length) {
            self.focus = None;
            self.mode = Mode::Pet;
            self.cat.set_mood(Mood::Idle);
            self.cat.set_mood(Mood::Surprised);
            self.message = Some(("Break time!".into(), 0));
            self.last_contact = Instant::now();
        }
        if self.last_contact.elapsed() > BORED && self.cat.mood() != Mood::Sleepy {
            self.cat.set_mood(Mood::Sleepy);
        }

        let streaming = self.streaming();
        if !streaming {
            self.bars = [0; BARS];
        }
        for (peak, &bar) in self.peaks.iter_mut().zip(&self.bars) {
            *peak = (*peak - 1.0).max(bar as f32);
        }
        let music = streaming.then(|| {
            let level = self.bars.iter().map(|&b| b as f32).sum::<f32>() / (BARS as f32 * 64.0);
            self.since_beat += 1;
            let beat = level > self.average * 1.3 && level > 0.1 && self.since_beat > 5;
            if beat {
                self.since_beat = 0;
            }
            self.average += (level - self.average) * 0.1;
            (level, beat)
        });
        self.cat.update(music);
    }

    pub fn draw<D: DrawTarget<Color = BinaryColor>>(&self, d: &mut D) -> Result<(), D::Error> {
        match self.mode {
            Mode::Pet => self.cat.draw(d)?,
            Mode::Song => self.draw_song(d)?,
            Mode::Lyrics => self.draw_lyrics(d)?,
            Mode::Bars => self.draw_bars(d, 0, 64)?,
            Mode::Focus => self.draw_focus(d)?,
        }
        if let Some(percent) = self.battery {
            self.draw_battery(d, percent)?;
        }
        if let Some((text, frames)) = &self.message {
            // A banner over the middle of the screen with the text scrolling through it
            let banner = RoundedRectangle::with_equal_corners(
                Rectangle::new(Point::new(0, 18), Size::new(128, 28)),
                Size::new(6, 6),
            );
            banner.into_styled(PrimitiveStyle::with_fill(BinaryColor::Off)).draw(d)?;
            banner.into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1)).draw(d)?;
            let x = 128 - (frames * MESSAGE_SPEED) as i32 % (128 + text.len() as i32 * 10);
            let style = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
            let mut inside = d.clipped(&Rectangle::new(Point::new(2, 19), Size::new(124, 26)));
            Text::new(text, Point::new(x, 37), style).draw(&mut inside)?;
        }
        Ok(())
    }

    fn nothing_playing<D: DrawTarget<Color = BinaryColor>>(&self, d: &mut D) -> Result<(), D::Error> {
        centered(d, "Nothing playing", &FONT_9X15_BOLD, 14)?;
        centered(d, "log in to Spotify", &FONT_6X10, 36)?;
        centered(d, "on the phone page", &FONT_6X10, 47)
    }

    fn draw_song<D: DrawTarget<Color = BinaryColor>>(&self, d: &mut D) -> Result<(), D::Error> {
        let Some(song) = &self.song else { return self.nothing_playing(d) };
        scrolling_line(d, &song.title, &FONT_10X20, 0, self.frame)?;
        scrolling_line(d, &song.artist, &FONT_6X10, 23, self.frame)?;
        if !song.next.is_empty() {
            scrolling_line(d, &format!("Next: {}", song.next), &FONT_6X10, 38, self.frame)?;
        }
        progress_bar(d, 58, song.progress())
    }

    fn draw_lyrics<D: DrawTarget<Color = BinaryColor>>(&self, d: &mut D) -> Result<(), D::Error> {
        let Some(song) = &self.song else { return self.nothing_playing(d) };
        scrolling_line(d, &song.title, &FONT_6X10, 0, self.frame)?;
        Rectangle::new(Point::new(0, 11), Size::new(128, 1)).into_styled(PrimitiveStyle::with_fill(BinaryColor::On)).draw(d)?;
        if self.lyric.is_empty() {
            return centered(d, "~ ~ ~", &FONT_9X15_BOLD, 30);
        }
        // Big text when it fits in 3 lines, small otherwise
        let big = wrap(&self.lyric, 14);
        let (lines, font, step) =
            if big.len() <= 3 { (big, &FONT_9X15_BOLD, 16) } else { (wrap(&self.lyric, 21), &FONT_6X10, 10) };
        let shown = lines.len().min(52 / step as usize);
        let mut top = 13 + (51 - shown as i32 * step) / 2;
        for line in &lines[..shown] {
            centered(d, line, font, top)?;
            top += step;
        }
        Ok(())
    }

    fn draw_focus<D: DrawTarget<Color = BinaryColor>>(&self, d: &mut D) -> Result<(), D::Error> {
        let Some(focus) = &self.focus else {
            centered(d, "Focus timer", &FONT_9X15_BOLD, 16)?;
            return centered(d, "start one on the phone", &FONT_6X10, 38);
        };
        let left = focus.length.saturating_sub(focus.started.elapsed()).as_secs();
        centered(d, "FOCUS", &FONT_6X10, 4)?;
        centered(d, &format!("{:02}:{:02}", left / 60, left % 60), &FONT_10X20, 18)?;
        centered(d, "the cat is napping", &FONT_6X10, 42)?;
        let done = focus.started.elapsed().as_secs_f32() / focus.length.as_secs_f32().max(1.0);
        progress_bar(d, 58, done.min(1.0))
    }

    /// Bars with peak caps filling `height` pixels from `top` down
    fn draw_bars<D: DrawTarget<Color = BinaryColor>>(&self, d: &mut D, top: i32, height: i32) -> Result<(), D::Error> {
        let on = PrimitiveStyle::with_fill(BinaryColor::On);
        let width = 128 / BARS as i32;
        let scale = |v: f32| (v / 64.0 * height as f32).min(height as f32) as i32;
        for i in 0..BARS {
            let x = i as i32 * width;
            let h = scale(self.bars[i] as f32);
            if h > 0 {
                Rectangle::new(Point::new(x, top + height - h), Size::new(width as u32 - 1, h as u32))
                    .into_styled(on)
                    .draw(d)?;
            }
            let cap = top + height - 1 - scale(self.peaks[i]);
            Rectangle::new(Point::new(x, cap.max(top)), Size::new(width as u32 - 1, 1)).into_styled(on).draw(d)?;
        }
        Ok(())
    }

    /// Small battery icon in the top right corner; blinks when nearly empty
    fn draw_battery<D: DrawTarget<Color = BinaryColor>>(&self, d: &mut D, percent: u8) -> Result<(), D::Error> {
        if percent < 15 && self.frame % 20 >= 10 {
            return Ok(());
        }
        let on = PrimitiveStyle::with_fill(BinaryColor::On);
        Rectangle::new(Point::new(111, 0), Size::new(17, 8)).into_styled(PrimitiveStyle::with_fill(BinaryColor::Off)).draw(d)?;
        Rectangle::new(Point::new(112, 1), Size::new(13, 6)).into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1)).draw(d)?;
        Rectangle::new(Point::new(125, 3), Size::new(2, 2)).into_styled(on).draw(d)?;
        let fill = (percent as u32 * 9 + 50) / 100;
        if fill > 0 {
            Rectangle::new(Point::new(114, 3), Size::new(fill, 2)).into_styled(on).draw(d)?;
        }
        Ok(())
    }
}

fn centered<D: DrawTarget<Color = BinaryColor>>(d: &mut D, text: &str, font: &MonoFont, top: i32) -> Result<(), D::Error> {
    let layout = TextStyleBuilder::new().alignment(Alignment::Center).baseline(Baseline::Top).build();
    Text::with_text_style(text, Point::new(64, top), MonoTextStyle::new(font, BinaryColor::On), layout).draw(d)?;
    Ok(())
}

/// One line of text from `top`; scrolls round when it doesn't fit
fn scrolling_line<D: DrawTarget<Color = BinaryColor>>(
    d: &mut D,
    text: &str,
    font: &MonoFont,
    top: i32,
    frame: u32,
) -> Result<(), D::Error> {
    let style = MonoTextStyle::new(font, BinaryColor::On);
    let layout = TextStyleBuilder::new().baseline(Baseline::Top).build();
    let width = text.chars().count() as i32 * font.character_size.width as i32;
    if width <= 128 {
        Text::with_text_style(text, Point::new(0, top), style, layout).draw(d)?;
        return Ok(());
    }
    let cycle = width + 30;
    let x = -(frame as i32 * 2 % cycle);
    Text::with_text_style(text, Point::new(x, top), style, layout).draw(d)?;
    Text::with_text_style(text, Point::new(x + cycle, top), style, layout).draw(d)?;
    Ok(())
}

fn progress_bar<D: DrawTarget<Color = BinaryColor>>(d: &mut D, top: i32, done: f32) -> Result<(), D::Error> {
    let bar = Rectangle::new(Point::new(0, top), Size::new(128, 5));
    bar.into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1)).draw(d)?;
    let filled = (done * 128.0) as u32;
    Rectangle::new(bar.top_left, Size::new(filled, 5)).into_styled(PrimitiveStyle::with_fill(BinaryColor::On)).draw(d)
}

/// Splits text into lines of at most `width` characters, breaking long words
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let chars: Vec<char> = word.chars().collect();
        for piece in chars.chunks(width) {
            if !line.is_empty() && line.chars().count() + 1 + piece.len() > width {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.extend(piece);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}
