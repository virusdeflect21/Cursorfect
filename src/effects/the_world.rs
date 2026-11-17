//! The World: time stop.
//!
//! Timeline (seconds are typical for the DIO dub):
//! ```text
//!  press �� Charging �� (voice "ZA WARUDO!") �� freeze �� Stopping �� Stopped �Ŀ
//!                                                 1.2 s anim            ticks   � press /
//!  Idle �� Resuming �� unfreeze �� ResumeCharging ("toki wa ugoki dasu") ����� release / timer
//!           0.8 s anim   (moves applied)
//! ```
//! While time is stopped the overlay shows a frozen capture of the desktop. Real windows can
//! be grabbed: their frozen image is dragged, the real window stays put, and all queued moves
//! are applied in one go when time resumes. This module is pure logic + drawing; the engine
//! performs the capture, input swallowing, sounds and window moves when it receives events.

use std::time::{Duration, Instant};

use glam::Vec2;

use super::rays::Rays;
use super::{Batch, Config, Look, RibbonPoint, Rng, World, mix};
use crate::overlay::renderer::Crop;
use crate::audio::dubs::Dub;
use crate::platform::wm::{WinInfo, WinRect};
use crate::settings::config::TimeStopMode;
use crate::settings::presets::Rgb;

const STOP_ANIM: f32 = 1.25;
const RESUME_ANIM: f32 = 0.85;
const GOLD: Rgb = [1.0, 0.80, 0.36];
const VIOLET: Rgb = [0.66, 0.55, 0.98];

pub fn palette(dub: Dub) -> (Rgb, Rgb) {
    match dub {
        Dub::OverHeaven => ([0.96, 0.98, 1.0], [0.68, 0.78, 0.92]),
        Dub::JotaroJp | Dub::JotaroEn => ([0.22, 0.58, 1.0], [0.12, 0.25, 0.72]),
        Dub::Diego => ([1.0, 0.64, 0.12], [0.72, 0.38, 0.06]),
        _ => (GOLD, VIOLET),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    /// Voice line playing; the world still moves.
    Charging,
    /// Time just stopped: inversion sphere, ripples.
    Stopping,
    Stopped,
    /// Resume voice line playing; still frozen, no more dragging.
    ResumeCharging,
    /// Colour and motion flood back.
    Resuming,
}

/// What the engine must do now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TwEvent {
    /// Capture the screen, show it frozen, start swallowing clicks.
    Freeze,
    /// Apply the queued window moves, show the live desktop again, release input.
    Unfreeze,
    /// One more frozen second has passed (for the clock tick).
    Tick(u32),
    /// Timed mode ran out (or the safety limit was hit): start resuming.
    AutoResume,
    /// Everything is over; the overlay may hide.
    Done,
}

/// Visual / behaviour options for one time stop.
#[derive(Clone, Copy, Debug)]
pub struct TwOptions {
    pub dub: Dub,
    pub custom_primary: Rgb,
    pub custom_secondary: Rgb,
    pub mode: TimeStopMode,
    pub duration: f32,
    pub max_secs: f32,
    pub flash: bool,
    pub inversion: bool,
    pub tint: bool,
    pub ripples: bool,
    pub streaks: bool,
    pub flare: bool,
    pub rays: bool,
    pub countdown: bool,
    pub move_windows: bool,
}

impl Default for TwOptions {
    fn default() -> Self {
        Self {
            dub: Dub::DioJp,
            custom_primary: [0.91, 0.36, 1.0],
            custom_secondary: [0.38, 0.22, 0.78],
            mode: TimeStopMode::Toggle,
            duration: 5.0,
            max_secs: 300.0,
            flash: true,
            inversion: true,
            tint: true,
            ripples: true,
            streaks: true,
            flare: true,
            rays: true,
            countdown: true,
            move_windows: true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Move {
    win: usize,
    dx: f32,
    dy: f32,
}

#[derive(Clone, Debug)]
struct Drag {
    win: usize,
    /// Cursor position when grabbed.
    start: Vec2,
    /// Offset the window already had from an earlier queued move.
    base: Vec2,
    cur: Vec2,
    /// Recent cursor samples for the motion streaks.
    trail: Vec<(Vec2, Instant)>,
}

pub struct TimeStop {
    phase: Phase,
    phase_at: Instant,
    pub opts: TwOptions,
    center: Vec2,
    freeze_at: Instant,
    unfreeze_at: Instant,
    frozen_at: Option<Instant>,
    ticks: u32,
    windows: Vec<WinInfo>,
    moves: Vec<Move>,
    drag: Option<Drag>,
    hover: Option<usize>,
    /// Where the countdown goes (monitor of the time stop), virtual-desktop px.
    monitor: WinRect,
    rays: Rays,
    rng: Rng,
    auto_resume_sent: bool,
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl TimeStop {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            phase: Phase::Idle,
            phase_at: now,
            opts: TwOptions::default(),
            center: Vec2::ZERO,
            freeze_at: now,
            unfreeze_at: now,
            frozen_at: None,
            ticks: 0,
            windows: Vec::new(),
            moves: Vec::new(),
            drag: None,
            hover: None,
            monitor: WinRect {
                l: 0.0,
                t: 0.0,
                r: 1920.0,
                b: 1080.0,
            },
            rays: Rays::new(),
            rng: Rng::new(0x7E57_0D10),
            auto_resume_sent: false,
        }
    }

    fn colors(&self) -> (Rgb, Rgb) {
        if self.opts.dub == Dub::Custom { (self.opts.custom_primary, self.opts.custom_secondary) } else { palette(self.opts.dub) }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn is_idle(&self) -> bool {
        self.phase == Phase::Idle
    }

    /// Clicks must be swallowed (the world is frozen on screen).
    pub fn input_frozen(&self) -> bool {
        matches!(
            self.phase,
            Phase::Stopping | Phase::Stopped | Phase::ResumeCharging
        )
    }

    /// Started and not yet asked to resume (a press now means "resume").
    pub fn is_running(&self) -> bool {
        matches!(
            self.phase,
            Phase::Charging | Phase::Stopping | Phase::Stopped
        )
    }

    pub fn is_animating(&self) -> bool {
        self.phase != Phase::Idle || !self.rays.is_empty()
    }

    fn set_phase(&mut self, p: Phase, now: Instant) {
        self.phase = p;
        self.phase_at = now;
    }

    /// Start a time stop at `center`; time freezes `freeze_in` seconds from now.
    pub fn begin(&mut self, now: Instant, center: Vec2, freeze_in: f32, opts: TwOptions, monitor: WinRect) {
        self.opts = opts;
        self.center = center;
        self.monitor = monitor;
        self.freeze_at = now + Duration::from_secs_f32(freeze_in.max(0.0));
        self.frozen_at = None;
        self.ticks = 0;
        self.windows.clear();
        self.moves.clear();
        self.drag = None;
        self.hover = None;
        self.auto_resume_sent = false;
        self.set_phase(Phase::Charging, now);
    }

    /// The engine captured the screen: time is now stopped.
    pub fn frozen(&mut self, now: Instant, windows: Vec<WinInfo>, cfg: &Config) {
        self.windows = if self.opts.move_windows {
            windows
        } else {
            Vec::new()
        };
        self.frozen_at = Some(now);
        self.set_phase(Phase::Stopping, now);
        if self.opts.rays {
            let mut c = cfg.clone();
            let (primary, secondary) = self.colors();
            c.style.palette = [primary, secondary, mix(primary, [1.0; 3], 0.65), primary, secondary, primary];
            self.rays
                .spawn(self.center, now, 26, 520.0, 0.7, Vec2::ZERO, &c, &mut self.rng);
        }
    }

    /// Capture failed (secure desktop, ...): abort without ever swallowing input.
    pub fn abort(&mut self, now: Instant) {
        self.drag = None;
        self.moves.clear();
        self.set_phase(Phase::Idle, now);
    }

    /// Start resuming; time starts again `unfreeze_in` seconds from now. Pressing during the
    /// voice line (before anything froze) cancels instead. Returns false if nothing to do.
    pub fn request_resume(&mut self, now: Instant, unfreeze_in: f32) -> bool {
        match self.phase {
            Phase::Charging => {
                self.set_phase(Phase::Idle, now);
                true
            }
            Phase::Stopping | Phase::Stopped => {
                // An in-progress drag is committed where it is (consistent rule: release = drop).
                self.release();
                self.hover = None;
                self.unfreeze_at = now + Duration::from_secs_f32(unfreeze_in.max(0.0));
                self.set_phase(Phase::ResumeCharging, now);
                true
            }
            _ => false,
        }
    }

    /// The engine applied the moves and shows the live desktop again.
    pub fn unfrozen(&mut self, now: Instant, center: Vec2) {
        self.center = center;
        self.windows.clear();
        self.moves.clear();
        self.drag = None;
        self.set_phase(Phase::Resuming, now);
    }

    /// Queued moves as (hwnd, dx, dy).
    pub fn pending_moves(&self) -> Vec<(isize, f32, f32)> {
        self.moves
            .iter()
            .filter_map(|m| self.windows.get(m.win).map(|w| (w.hwnd, m.dx, m.dy)))
            .collect()
    }

    /// Seconds since time stopped.
    pub fn frozen_secs(&self, now: Instant) -> f32 {
        self.frozen_at
            .map(|t| now.saturating_duration_since(t).as_secs_f32())
            .unwrap_or(0.0)
    }

    /// The moment time will resume by itself (Timed mode or the safety limit).
    pub fn deadline(&self) -> Option<Instant> {
        let f = self.frozen_at?;
        let secs = match self.opts.mode {
            TimeStopMode::Timed => self.opts.duration.min(self.opts.max_secs),
            _ => self.opts.max_secs,
        };
        Some(f + Duration::from_secs_f32(secs))
    }

    pub fn update(&mut self, now: Instant) -> Vec<TwEvent> {
        let mut ev = Vec::new();
        let t = now.saturating_duration_since(self.phase_at).as_secs_f32();
        match self.phase {
            Phase::Idle => {}
            Phase::Charging => {
                if now >= self.freeze_at {
                    ev.push(TwEvent::Freeze);
                }
            }
            Phase::Stopping | Phase::Stopped => {
                if self.phase == Phase::Stopping && t >= STOP_ANIM {
                    self.set_phase(Phase::Stopped, now);
                }
                let secs = self.frozen_secs(now);
                while (self.ticks as f32 + 1.0) <= secs {
                    self.ticks += 1;
                    ev.push(TwEvent::Tick(self.ticks));
                }
                if !self.auto_resume_sent && self.deadline().is_some_and(|d| now >= d) {
                    self.auto_resume_sent = true;
                    ev.push(TwEvent::AutoResume);
                }
            }
            Phase::ResumeCharging => {
                if now >= self.unfreeze_at {
                    ev.push(TwEvent::Unfreeze);
                }
            }
            Phase::Resuming => {
                if t >= RESUME_ANIM {
                    self.set_phase(Phase::Idle, now);
                    ev.push(TwEvent::Done);
                }
            }
        }
        self.rays.prune(now);
        ev
    }

    // ------------------------------------------------------------------ dragging

    fn offset_of(&self, win: usize) -> Vec2 {
        self.moves
            .iter()
            .find(|m| m.win == win)
            .map(|m| Vec2::new(m.dx, m.dy))
            .unwrap_or(Vec2::ZERO)
    }

    fn can_drag(&self) -> bool {
        matches!(self.phase, Phase::Stopping | Phase::Stopped) && self.opts.move_windows
    }

    /// The window under `p` as seen in the frozen world: moved windows (drawn on top) first,
    /// then the rest in their original stacking order.
    pub fn hit(&self, p: Vec2) -> Option<usize> {
        for m in self.moves.iter().rev() {
            if let Some(w) = self.windows.get(m.win) {
                if w.rect.offset(m.dx, m.dy).contains(p.x, p.y) {
                    return Some(m.win);
                }
            }
        }
        self.windows.iter().enumerate().find_map(|(i, w)| {
            let moved = self.moves.iter().any(|m| m.win == i);
            (!moved && w.rect.contains(p.x, p.y)).then_some(i)
        })
    }

    pub fn grab(&mut self, p: Vec2, now: Instant) -> bool {
        if !self.can_drag() {
            return false;
        }
        let Some(win) = self.hit(p) else {
            return false;
        };
        let base = self.offset_of(win);
        self.drag = Some(Drag {
            win,
            start: p,
            base,
            cur: p,
            trail: vec![(p, now)],
        });
        true
    }

    pub fn drag_to(&mut self, p: Vec2, now: Instant) {
        if self.drag.is_none() {
            self.hover = if self.can_drag() { self.hit(p) } else { None };
        }
        if let Some(d) = self.drag.as_mut() {
            d.cur = p;
            d.trail.push((p, now));
            d.trail
                .retain(|(_, t)| now.saturating_duration_since(*t) < Duration::from_millis(140));
        }
    }

    /// Drop the dragged window: queue its new position. Returns true if a move was queued.
    pub fn release(&mut self) -> bool {
        let Some(d) = self.drag.take() else {
            return false;
        };
        let off = d.base + (d.cur - d.start);
        self.moves.retain(|m| m.win != d.win);
        if off.length() >= 2.0 {
            self.moves.push(Move {
                win: d.win,
                dx: off.x,
                dy: off.y,
            });
            true
        } else {
            false
        }
    }

    /// Esc: put the dragged window back where it was before this drag.
    pub fn cancel_drag(&mut self) {
        self.drag = None;
    }

    /// Backspace: forget the last queued move.
    pub fn undo(&mut self) -> bool {
        if self.drag.is_some() {
            self.drag = None;
            return true;
        }
        self.moves.pop().is_some()
    }

    // ------------------------------------------------------------------ visuals

    /// World-pass parameters (None: the live desktop shows untouched).
    pub fn world(&self, now: Instant, max_radius: f32) -> Option<World> {
        let t = now.saturating_duration_since(self.phase_at).as_secs_f32();
        let feather = 160.0;
        let full = max_radius + feather;
        let tint = if self.opts.tint { 1.0 } else { 0.0 };
        let chroma = if self.opts.inversion { 1.0 } else { 0.0 };
        let charge_total = self
            .freeze_at
            .saturating_duration_since(self.phase_at)
            .as_secs_f32();
        let base = |amount: f32, radius: f32, invert: bool, inversion: f32, flash: f32, ripple: (Vec2, f32, f32)| World {
            center: self.center,
            radius,
            feather,
            amount,
            invert,
            contrast: 1.12,
            front: 0.0,
            glitch: 0.0,
            ripple,
            look: Look::TimeStop {
                inversion,
                flash,
                tint,
                chroma,
                pinch: 0.0,
                primary: self.colors().0,
                secondary: self.colors().1,
            },
        };
        let no_ripple = (self.center, 0.0, 0.0);
        match self.phase {
            Phase::Idle => None,
            // Instant mode: nothing to build up.
            Phase::Charging if charge_total < 0.2 => None,
            Phase::Charging => {
                // While the voice line plays, time starts to give way: the frozen grade creeps
                // in from the screen edges, pressure waves rush inwards to the stand, and in
                // the last instant space is pinched towards it.
                let k = (t / charge_total).clamp(0.0, 1.0);
                let radius = full * (1.0 - 0.6 * k.powf(1.5));
                let amount = 0.6 * smooth(0.05, 1.0, k);
                let ripple = if self.opts.ripples {
                    // Negative radius = continuous inward wave train (see the shader).
                    (self.center, -1.0, (0.1 + 0.7 * smooth(0.0, 1.0, k)) * smooth(0.0, 0.25, t))
                } else {
                    no_ripple
                };
                let mut w = base(amount, radius, true, 0.0, 0.0, ripple);
                w.look = Look::TimeStop {
                    inversion: 0.0,
                    flash: 0.0,
                    tint,
                    chroma,
                    primary: self.colors().0,
                    secondary: self.colors().1,
                    pinch: if self.opts.flare {
                        smooth(0.78, 1.0, k)
                    } else {
                        0.0
                    },
                };
                Some(w)
            }
            Phase::Stopping => {
                // The sphere of inverted colour bursts out across the screen, then collapses
                // back into the stand, leaving the frozen, graded world behind.
                let inversion = if self.opts.inversion {
                    let reach = max_radius * 1.05;
                    if t < 0.42 {
                        ease_out(t / 0.42) * reach
                    } else {
                        let k = ((t - 0.42) / 0.5).clamp(0.0, 1.0);
                        (1.0 - k * k) * reach
                    }
                } else {
                    0.0
                };
                let flash = if self.opts.flash {
                    0.42 * (1.0 - t / 0.2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let ripple = if self.opts.ripples && t < 1.3 {
                    let k = t / 1.3;
                    (self.center, 30.0 + 1500.0 * ease_out(k), (1.0 - k).powf(1.3) * 1.1)
                } else {
                    no_ripple
                };
                Some(base(smooth(0.0, 0.55, t), full, false, inversion, flash, ripple))
            }
            Phase::Stopped | Phase::ResumeCharging => Some(base(1.0, full, false, 0.0, 0.0, no_ripple)),
            Phase::Resuming => {
                // Colour floods back from the cursor with a white-gold pulse and a ripple.
                let k = (t / 0.6).clamp(0.0, 1.0);
                let flash = if self.opts.flash {
                    0.28 * (1.0 - t / 0.22).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let ripple = if self.opts.ripples {
                    (self.center, 30.0 + 1300.0 * ease_out(t / RESUME_ANIM), (1.0 - t / RESUME_ANIM).max(0.0) * 0.8)
                } else {
                    no_ripple
                };
                if k >= 1.0 && ripple.2 <= 0.01 && flash <= 0.0 {
                    return None;
                }
                Some(base(1.0, ease_out(k) * full, true, 0.0, flash, ripple))
            }
        }
    }

    fn countdown_text(&self, now: Instant) -> String {
        let secs = self.frozen_secs(now);
        match self.opts.mode {
            TimeStopMode::Timed => format!("{:.1}", (self.opts.duration - secs).max(0.0)),
            // DIO counts the seconds that have passed in stopped time.
            _ => format!("{}", secs.floor() as u32),
        }
    }

    const DIGIT_H: f32 = 56.0;

    /// Width of the countdown text (matches `draw_digits`).
    fn text_width(text: &str, h: f32) -> f32 {
        let unit = h / 2.0;
        text.chars()
            .map(|c| if c == '.' { unit * 0.5 } else { unit * 1.55 })
            .sum()
    }

    // Only copy pixels belonging to this window: higher windows cover parts of the
    // desktop capture and must never be carried along with a lower window.
    fn visible_pieces(&self, index: usize) -> Vec<WinRect> {
        let mut pieces = vec![self.windows[index].rect];
        for cover in self.windows.iter().take(index).map(|w| w.rect) {
            let mut next = Vec::new();
            for r in pieces {
                let l = r.l.max(cover.l); let t = r.t.max(cover.t);
                let right = r.r.min(cover.r); let bottom = r.b.min(cover.b);
                if l >= right || t >= bottom { next.push(r); continue; }
                for part in [WinRect { l: r.l, t: r.t, r: r.r, b: t },
                             WinRect { l: r.l, t: bottom, r: r.r, b: r.b },
                             WinRect { l: r.l, t, r: l, b: bottom },
                             WinRect { l: right, t, r: r.r, b: bottom }] {
                    if part.w() > 0.0 && part.h() > 0.0 { next.push(part); }
                }
            }
            pieces = next;
        }
        pieces
    }

    /// Frozen-image pieces: vacated spots and the windows at their new places.
    pub fn crops(&self, now: Instant) -> Vec<Crop> {
        if !matches!(
            self.phase,
            Phase::Stopping | Phase::Stopped | Phase::ResumeCharging
        ) {
            return Vec::new();
        }
        let mut out = Vec::new();
        if self.opts.countdown {
            // Dark backing so the counter reads on any background.
            let w = Self::text_width(&self.countdown_text(now), Self::DIGIT_H);
            let (r, t) = (self.monitor.r - 48.0, self.monitor.t + 40.0);
            let a = smooth(0.3, 0.9, self.frozen_secs(now));
            out.push(Crop {
                src: None,
                dst: [r - w - 22.0, t - 18.0, r + 14.0, t + Self::DIGIT_H + 18.0],
                alpha: a,
                fill: [0.03, 0.02, 0.06, 0.62],
            });
        }
        let dragging = self.drag.as_ref().map(|d| d.win);
        let r4 = |r: WinRect| [r.l, r.t, r.r, r.b];
        // Where moved windows used to be: the frozen image can't know what was behind them,
        // so the spot is shown as an empty, darkened imprint.
        for i in self.moves.iter().map(|m| m.win).chain(dragging) {
            if let Some(w) = self.windows.get(i) {
                out.push(Crop {
                    src: None,
                    dst: r4(w.rect),
                    alpha: 1.0,
                    fill: [0.02, 0.01, 0.05, 0.55],
                });
            }
        }
        for m in &self.moves {
            if Some(m.win) == dragging {
                continue;
            }
            if self.windows.get(m.win).is_some() {
                for piece in self.visible_pieces(m.win) {
                    out.push(Crop { src: Some(r4(piece)), dst: r4(piece.offset(m.dx, m.dy)), alpha: 1.0, fill: [0.0; 4] });
                }
            }
        }
        if let Some(d) = self.drag.as_ref() {
            if self.windows.get(d.win).is_some() {
                let off = d.base + (d.cur - d.start);
                for piece in self.visible_pieces(d.win) {
                    out.push(Crop { src: Some(r4(piece)), dst: r4(piece.offset(off.x, off.y)), alpha: 0.95, fill: [0.0; 4] });
                }
            }
        }
        out
    }

    fn outline(batch: &mut Batch, r: WinRect, c: Rgb, a: f32, width: f32, cfg: &Config) {
        let pts = [
            Vec2::new(r.l, r.t),
            Vec2::new(r.r, r.t),
            Vec2::new(r.r, r.b),
            Vec2::new(r.l, r.b),
        ];
        for i in 0..4 {
            let (p0, p1) = (pts[i], pts[(i + 1) % 4]);
            let seg = |hw: f32, al: f32| {
                [
                    RibbonPoint {
                        pos: p0,
                        half_width: hw,
                        alpha: al,
                    },
                    RibbonPoint {
                        pos: p1,
                        half_width: hw,
                        alpha: al,
                    },
                ]
            };
            batch.ribbon(&seg(width * 4.0, a * 0.22), c, 1.0);
            batch.ribbon(&seg(width, a), cfg.hot(c, 1.5), 0.8);
        }
    }

    /// Character-specific display lettering drawn directly into the overlay.
    fn draw_digits(batch: &mut Batch, text: &str, right: Vec2, h: f32, c: Rgb, a: f32, dub: Dub) {
        // Segments: a b c d e f g as (x0,y0,x1,y1) in a 0..1 x 0..2 box.
        const SEG: [(f32, f32, f32, f32); 7] = [
            (0.0, 0.0, 1.0, 0.0),
            (1.0, 0.0, 1.0, 1.0),
            (1.0, 1.0, 1.0, 2.0),
            (0.0, 2.0, 1.0, 2.0),
            (0.0, 1.0, 0.0, 2.0),
            (0.0, 0.0, 0.0, 1.0),
            (0.0, 1.0, 1.0, 1.0),
        ];
        fn mask(ch: char) -> u8 {
            match ch {
                '0' => 0b0111111,
                '1' => 0b0000110,
                '2' => 0b1011011,
                '3' => 0b1001111,
                '4' => 0b1100110,
                '5' => 0b1101101,
                '6' => 0b1111101,
                '7' => 0b0000111,
                '8' => 0b1111111,
                '9' => 0b1101111,
                _ => 0,
            }
        }
        let unit = h / 2.0;
        let advance = unit * 1.55;
        let width: f32 = text
            .chars()
            .map(|ch| if ch == '.' { unit * 0.5 } else { advance })
            .sum();
        let mut x = right.x - width;
        let y = right.y;
        let hw = (h * match dub { Dub::OverHeaven => 0.035, Dub::Diego => 0.085, Dub::JotaroJp | Dub::JotaroEn => 0.065, _ => 0.055 }).max(1.5);
        for ch in text.chars() {
            if ch == '.' {
                batch.disc_sharp(Vec2::new(x + unit * 0.1, y + h), hw * 2.2, c, a, 0.8, 6.0);
                x += unit * 0.5;
                continue;
            }
            let m = mask(ch);
            for (i, s) in SEG.iter().enumerate() {
                if m & (1 << i) == 0 {
                    continue;
                }
                let slant = match dub { Dub::JotaroJp | Dub::JotaroEn => 0.17, Dub::Diego => -0.12, _ => 0.0 };
                let p0 = Vec2::new(x + (s.0 + slant * (2.0 - s.1)) * unit, y + s.1 * unit);
                let p1 = Vec2::new(x + (s.2 + slant * (2.0 - s.3)) * unit, y + s.3 * unit);
                let inset = (p1 - p0).normalize_or_zero() * hw * 0.9;
                let line = |w: f32, al: f32| {
                    [
                        RibbonPoint {
                            pos: p0 + inset,
                            half_width: w,
                            alpha: al,
                        },
                        RibbonPoint {
                            pos: p1 - inset,
                            half_width: w,
                            alpha: al,
                        },
                    ]
                };
                batch.ribbon(&line(hw * 3.5, a * 0.25), c, 1.0);
                batch.ribbon(&line(hw, a), c, 0.6);
            }
            x += advance;
        }
    }

    /// Energy being drawn into the stand: thin golden streaks fly inwards from every
    /// direction, more and faster as the voice line builds, then an anamorphic lens flare
    /// flashes across the cursor the instant before time stops.
    fn draw_gathering(batch: &mut Batch, p: Vec2, t: f32, k: f32, cfg: &Config, streaks: bool, flare: bool, primary: Rgb, secondary: Rgb) {
        let hash = |i: u32, salt: u32| -> f32 {
            let mut x = i.wrapping_mul(0x9E37_79B9) ^ salt.wrapping_mul(0x85EB_CA6B);
            x ^= x >> 15;
            x = x.wrapping_mul(0x2C1B_3C6D);
            x ^= x >> 12;
            (x & 0xFFFF) as f32 / 65535.0
        };
        let count = if streaks { (10.0 + 30.0 * k) as u32 } else { 0 };
        for i in 0..count {
            let ang = hash(i, 1) * std::f32::consts::TAU;
            let dir = Vec2::from_angle(ang);
            let speed = 0.9 + 1.1 * hash(i, 2) + 1.2 * k;
            let ph = (t * speed + hash(i, 3)).fract();
            let r0 = 260.0 + 520.0 * hash(i, 4);
            let r = 16.0 + r0 * (1.0 - ph).powf(1.6);
            let len = (26.0 + 90.0 * k) * (1.0 - ph * 0.6);
            let a = (0.25 + 0.75 * k) * smooth(0.0, 0.18, ph) * smooth(1.0, 0.86, ph);
            if a < 0.01 {
                continue;
            }
            let head = p + dir * r;
            let tail = p + dir * (r + len);
            let w = 0.8 + 0.9 * hash(i, 5);
            let col = if hash(i, 6) > 0.7 {
                secondary
            } else {
                mix(primary, [1.0, 1.0, 1.0], 0.35 * hash(i, 7))
            };
            let pts = [
                RibbonPoint {
                    pos: tail,
                    half_width: w * 0.3,
                    alpha: 0.0,
                },
                RibbonPoint {
                    pos: head,
                    half_width: w,
                    alpha: a,
                },
            ];
            batch.ribbon(&pts, cfg.hot(col, 1.8), 1.0);
        }
        // Anamorphic flare: a long, thin horizontal streak + a tiny sharp glint.
        let f = if flare { smooth(0.72, 1.0, k) } else { 0.0 };
        if f > 0.01 {
            let span = 120.0 + 900.0 * f;
            for (hw, al, heat) in [(9.0, 0.10, 1.0), (2.2, 0.55, 2.2)] {
                let pts = [
                    RibbonPoint {
                        pos: p - Vec2::X * span,
                        half_width: hw * 0.2,
                        alpha: 0.0,
                    },
                    RibbonPoint {
                        pos: p,
                        half_width: hw,
                        alpha: al * f,
                    },
                    RibbonPoint {
                        pos: p + Vec2::X * span,
                        half_width: hw * 0.2,
                        alpha: 0.0,
                    },
                ];
                batch.ribbon(&pts, cfg.hot(mix(primary, [1.0; 3], 0.25), heat), 1.0);
            }
            batch.disc_sharp(p, 9.0 + 6.0 * f, cfg.hot(mix(primary, [1.0; 3], 0.65), 2.5), f, 1.0, 14.0);
        }
    }

    pub fn draw(&mut self, batch: &mut Batch, now: Instant, cfg: &Config, cursor: Option<Vec2>) {
        let (primary, secondary) = self.colors();
        let t = now.saturating_duration_since(self.phase_at).as_secs_f32();
        match self.phase {
            Phase::Charging => {
                // The stand gathers power at the cursor while the voice line plays.
                let total = self
                    .freeze_at
                    .saturating_duration_since(self.phase_at)
                    .as_secs_f32()
                    .max(0.01);
                let k = (t / total).clamp(0.0, 1.0);
                let p = cursor.unwrap_or(self.center);
                self.center = p;
                Self::draw_gathering(batch, p, t, k, cfg, self.opts.streaks, self.opts.flare, primary, secondary);
            }
            Phase::Stopping | Phase::Stopped | Phase::ResumeCharging => {
                let fade_in = smooth(0.3, 0.9, self.frozen_secs(now));
                // Hover / drag outlines.
                if let Some(d) = self.drag.as_ref() {
                    if let Some(w) = self.windows.get(d.win) {
                        let off = d.base + (d.cur - d.start);
                        let r = w.rect.offset(off.x, off.y);
                        Self::outline(batch, r, primary, 0.95, match self.opts.dub { Dub::OverHeaven => 0.9, Dub::Diego => 2.5, Dub::JotaroJp | Dub::JotaroEn => 1.9, _ => 1.6 }, cfg);
                        if self.opts.dub == Dub::OverHeaven {
                            Self::outline(batch, r.offset(5.0, -5.0), secondary, 0.32, 0.7, cfg);
                        }
                        // Motion streaks trailing behind the corners.
                        if let (Some(first), Some(last)) = (d.trail.first(), d.trail.last()) {
                            let v = last.0 - first.0;
                            if v.length() > 8.0 && self.opts.dub != Dub::OverHeaven {
                                for c in [
                                    Vec2::new(r.l, r.t),
                                    Vec2::new(r.r, r.t),
                                    Vec2::new(r.r, r.b),
                                    Vec2::new(r.l, r.b),
                                    Vec2::new((r.l + r.r) * 0.5, r.t),
                                    Vec2::new((r.l + r.r) * 0.5, r.b),
                                ] {
                                    let pts = [
                                        RibbonPoint {
                                            pos: c - v * 1.2,
                                            half_width: 0.4,
                                            alpha: 0.0,
                                        },
                                        RibbonPoint {
                                            pos: c,
                                            half_width: 1.6,
                                            alpha: 0.7,
                                        },
                                    ];
                                    batch.ribbon(&pts, cfg.hot(if self.opts.dub == Dub::Diego { secondary } else { primary }, 1.4), 1.0);
                                }
                            }
                        }
                    }
                } else if let Some(h) = self.hover {
                    if let Some(w) = self.windows.get(h) {
                        let off = self.offset_of(h);
                        Self::outline(batch, w.rect.offset(off.x, off.y), primary, 0.45 * fade_in, 1.0, cfg);
                    }
                }
                for m in &self.moves {
                    if self.drag.as_ref().is_some_and(|d| d.win == m.win) {
                        continue;
                    }
                    if let Some(w) = self.windows.get(m.win) {
                        Self::outline(batch, w.rect.offset(m.dx, m.dy), secondary, 0.5, 1.0, cfg);
                    }
                }
                if self.opts.countdown {
                    let text = self.countdown_text(now);
                    let a = fade_in * if self.phase == Phase::ResumeCharging { 0.6 } else { 1.0 };
                    let top_right = Vec2::new(self.monitor.r - 48.0, self.monitor.t + 40.0);
                    let col = mix(primary, [1.0, 1.0, 1.0], 0.15);
                    Self::draw_digits(batch, &text, top_right, Self::DIGIT_H, cfg.hot(col, 1.6), a, self.opts.dub);
                }
            }
            Phase::Resuming | Phase::Idle => {}
        }
        self.rays.draw(batch, now, cfg);
    }
}

impl Default for TimeStop {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        crate::app::effect_config(&crate::settings::config::Settings::default())
    }

    fn win(hwnd: isize, l: f32, t: f32, r: f32, b: f32) -> WinInfo {
        WinInfo {
            hwnd,
            rect: WinRect { l, t, r, b },
            class: "Test".into(),
        }
    }

    fn stopped(now: Instant, mode: TimeStopMode) -> TimeStop {
        let mut ts = TimeStop::new();
        let opts = TwOptions {
            mode,
            duration: 3.0,
            ..TwOptions::default()
        };
        ts.begin(now, Vec2::new(500.0, 500.0), 0.0, opts, WinRect { l: 0.0, t: 0.0, r: 1920.0, b: 1080.0 });
        assert_eq!(ts.update(now), vec![TwEvent::Freeze]);
        ts.frozen(
            now,
            vec![win(1, 0.0, 0.0, 400.0, 300.0), win(2, 200.0, 100.0, 900.0, 700.0)],
            &cfg(),
        );
        ts
    }

    #[test]
    fn variant_colours_reach_the_world_pass() {
        let now = Instant::now();
        let mut ts = stopped(now, TimeStopMode::Toggle);
        for dub in Dub::ALL {
            ts.opts.dub = dub;
            let world = ts.world(now, 2000.0).unwrap();
            match world.look {
                Look::TimeStop { primary, secondary, .. } => {
                    assert_eq!((primary, secondary), ts.colors());
                }
                _ => panic!("expected time-stop grade"),
            }
        }
        assert_ne!(palette(Dub::Diego), palette(Dub::DioJp));
        assert_eq!(palette(Dub::JotaroJp), palette(Dub::JotaroEn));
    }

    #[test]
    fn dragged_lower_window_excludes_occluding_pixels() {
        let now = Instant::now();
        let ts = stopped(now, TimeStopMode::Toggle);
        let pieces = ts.visible_pieces(1);
        assert!(pieces.iter().all(|r| !r.contains(300.0, 200.0)));
        assert!(pieces.iter().any(|r| r.contains(500.0, 400.0)));
    }

    #[test]
    fn full_cycle_toggle() {
        let t0 = Instant::now();
        let mut ts = stopped(t0, TimeStopMode::Toggle);
        assert!(ts.input_frozen());
        let t1 = t0 + Duration::from_millis(2500);
        let ev = ts.update(t1);
        assert_eq!(ev, vec![TwEvent::Tick(1), TwEvent::Tick(2)]);
        assert_eq!(ts.phase(), Phase::Stopped);
        // Toggle mode has no 5 s limit: still stopped after a minute.
        let t2 = t0 + Duration::from_secs(60);
        assert!(!ts.update(t2).contains(&TwEvent::AutoResume));
        assert!(ts.request_resume(t2, 1.0));
        assert!(ts.input_frozen(), "still frozen during the resume voice line");
        assert!(ts.update(t2 + Duration::from_millis(500)).is_empty());
        assert_eq!(ts.update(t2 + Duration::from_millis(1000)), vec![TwEvent::Unfreeze]);
        ts.unfrozen(t2 + Duration::from_millis(1000), Vec2::ZERO);
        assert!(!ts.input_frozen());
        assert_eq!(ts.update(t2 + Duration::from_secs(3)), vec![TwEvent::Done]);
        assert!(ts.is_idle());
    }

    #[test]
    fn timed_mode_resumes_by_itself() {
        let t0 = Instant::now();
        let mut ts = stopped(t0, TimeStopMode::Timed);
        let ev = ts.update(t0 + Duration::from_millis(3010));
        assert!(ev.contains(&TwEvent::AutoResume), "{ev:?}");
        // Only once.
        assert!(!ts.update(t0 + Duration::from_millis(3100)).contains(&TwEvent::AutoResume));
    }

    #[test]
    fn safety_limit_applies_to_toggle() {
        let t0 = Instant::now();
        let mut ts = stopped(t0, TimeStopMode::Toggle);
        ts.opts.max_secs = 10.0;
        assert!(ts.update(t0 + Duration::from_secs(11)).contains(&TwEvent::AutoResume));
    }

    #[test]
    fn pressing_during_the_voice_cancels() {
        let t0 = Instant::now();
        let mut ts = TimeStop::new();
        ts.begin(t0, Vec2::ZERO, 2.0, TwOptions::default(), WinRect { l: 0.0, t: 0.0, r: 10.0, b: 10.0 });
        assert!(ts.update(t0 + Duration::from_millis(500)).is_empty());
        assert!(ts.request_resume(t0 + Duration::from_millis(600), 1.0));
        assert!(ts.is_idle());
        assert!(!ts.input_frozen());
    }

    #[test]
    fn drag_queues_moves_without_touching_real_windows() {
        let t0 = Instant::now();
        let mut ts = stopped(t0, TimeStopMode::Toggle);
        // Window 1 is on top where both overlap.
        assert_eq!(ts.hit(Vec2::new(300.0, 200.0)), Some(0));
        assert!(ts.grab(Vec2::new(300.0, 200.0), t0));
        ts.drag_to(Vec2::new(800.0, 900.0), t0);
        assert!(ts.release());
        assert_eq!(ts.pending_moves(), vec![(1, 500.0, 700.0)]);
        // Dragging the same window again accumulates into one move.
        assert!(ts.grab(Vec2::new(810.0, 910.0), t0));
        ts.drag_to(Vec2::new(710.0, 910.0), t0);
        ts.release();
        assert_eq!(ts.pending_moves(), vec![(1, 400.0, 700.0)]);
        // Where window 1 used to be, window 2 can now be grabbed.
        assert_eq!(ts.hit(Vec2::new(300.0, 200.0)), Some(1));
        assert!(ts.grab(Vec2::new(300.0, 200.0), t0));
        ts.drag_to(Vec2::new(310.0, 250.0), t0);
        ts.release();
        assert_eq!(ts.pending_moves().len(), 2);
        assert!(ts.undo());
        assert_eq!(ts.pending_moves(), vec![(1, 400.0, 700.0)]);
        // Esc cancels a drag in progress (window 1 now spans x 400..800, y 700..1000).
        assert!(ts.grab(Vec2::new(700.0, 900.0), t0));
        ts.drag_to(Vec2::new(0.0, 0.0), t0);
        ts.cancel_drag();
        assert_eq!(ts.pending_moves(), vec![(1, 400.0, 700.0)]);
    }

    #[test]
    fn resume_commits_a_drag_in_progress_and_blocks_new_ones() {
        let t0 = Instant::now();
        let mut ts = stopped(t0, TimeStopMode::Toggle);
        assert!(ts.grab(Vec2::new(10.0, 10.0), t0));
        ts.drag_to(Vec2::new(110.0, 10.0), t0);
        ts.request_resume(t0, 1.0);
        assert_eq!(ts.pending_moves(), vec![(1, 100.0, 0.0)]);
        assert!(!ts.grab(Vec2::new(150.0, 10.0), t0), "no dragging while resuming");
    }

    #[test]
    fn clicking_empty_space_grabs_nothing() {
        let t0 = Instant::now();
        let mut ts = stopped(t0, TimeStopMode::Toggle);
        assert!(!ts.grab(Vec2::new(1500.0, 1000.0), t0));
        assert!(ts.pending_moves().is_empty());
    }

    #[test]
    fn world_is_frozen_grade_while_stopped() {
        let t0 = Instant::now();
        let mut ts = stopped(t0, TimeStopMode::Toggle);
        ts.update(t0 + Duration::from_secs(2));
        let w = ts.world(t0 + Duration::from_secs(2), 2000.0).unwrap();
        assert_eq!(w.amount, 1.0);
        assert!(matches!(w.look, Look::TimeStop { inversion, .. } if inversion == 0.0));
    }
}

