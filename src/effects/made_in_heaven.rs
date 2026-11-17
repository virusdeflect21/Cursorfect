//! Made in Heaven: elapsed-time acceleration, three visual phases and Universe Reset.
//! The controller is pure/testable; the visuals emit into Cursorfect's existing GPU
//! batch and reuse the cursor history, ghost pool and particle pool.

use std::time::{Duration, Instant};

use glam::Vec2;

use super::ghosts::Ghosts;
use super::particles::Particles;
use super::trails::HeavenTrail;
use super::{Batch, Config, RibbonPoint, Rng, text};
use crate::input::cursor::{CursorTracker, Rect};
use crate::settings::config::{MadeInHeavenSettings, TimeStopMode};

const WHITE: [f32; 3] = [0.96, 1.0, 1.0];
const CYAN: [f32; 3] = [0.20, 0.88, 1.0];

fn smooth(lo: f32, hi: f32, value: f32) -> f32 {
    let x = ((value - lo) / (hi - lo).max(0.0001)).clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// ~1.33x / 1.99x / 2.89x at 25/50/75%, with the default 4x maximum.
pub fn acceleration_curve(progress: f32) -> f32 {
    progress.clamp(0.0, 1.0).powf(1.6)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MihPhase {
    Inactive,
    Awakening,
    TimeAcceleration,
    HeavenAscended,
    UniverseReset,
    Decelerating,
}

impl MihPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Inactive => "READY",
            Self::Awakening => "AWAKENING",
            Self::TimeAcceleration => "TIME ACCELERATION",
            Self::HeavenAscended => "HEAVEN ASCENDED",
            Self::UniverseReset => "UNIVERSE RESET",
            Self::Decelerating => "DECELERATING",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MihSnapshot {
    pub phase: MihPhase,
    pub progress: f32,
    pub multiplier: f32,
    pub opacity: f32,
    pub reset_progress: f32,
    pub reset_elapsed: f32,
}

impl Default for MihSnapshot {
    fn default() -> Self {
        Self {
            phase: MihPhase::Inactive,
            progress: 0.0,
            multiplier: 1.0,
            opacity: 0.0,
            reset_progress: 0.0,
            reset_elapsed: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MihEvent {
    Activated,
    Acceleration,
    Maximum,
    Reset,
    Deactivated,
    Finished,
}

/// One update can finish a reset and begin one repeat. No per-frame Vec allocation.
#[derive(Clone, Copy, Debug, Default)]
pub struct MihEvents {
    events: [Option<MihEvent>; 6],
    len: usize,
}

impl MihEvents {
    fn push(&mut self, event: MihEvent) {
        if self.len < self.events.len() {
            self.events[self.len] = Some(event);
            self.len += 1;
        }
    }
}

impl IntoIterator for MihEvents {
    type Item = MihEvent;
    type IntoIter = std::iter::Flatten<std::array::IntoIter<Option<MihEvent>, 6>>;
    fn into_iter(self) -> Self::IntoIter {
        self.events.into_iter().flatten()
    }
}

#[derive(Clone, Copy, Debug)]
enum State {
    Idle,
    Accelerating {
        at: Instant,
        from_progress: f32,
        cycle_at: Instant,
    },
    Resetting {
        at: Instant,
        from_multiplier: f32,
    },
    Decelerating {
        at: Instant,
        duration: f32,
        from: MihSnapshot,
    },
}

pub struct MihController {
    state: State,
    settings: MadeInHeavenSettings,
    activation_at: Option<Instant>,
    held: bool,
    acceleration_announced: bool,
    /// Smooth out live edits of the maximum without a sudden pointer-speed jump.
    retune: Option<(Instant, f32)>,
}

impl Default for MihController {
    fn default() -> Self {
        Self {
            state: State::Idle,
            settings: MadeInHeavenSettings::default(),
            activation_at: None,
            held: false,
            acceleration_announced: false,
            retune: None,
        }
    }
}

impl MihController {
    pub fn is_idle(&self) -> bool {
        matches!(self.state, State::Idle)
    }

    pub fn snapshot(&self, now: Instant) -> MihSnapshot {
        match self.state {
            State::Idle => MihSnapshot::default(),
            State::Accelerating {
                at,
                from_progress,
                cycle_at,
            } => {
                let progress = (from_progress
                    + now.saturating_duration_since(at).as_secs_f32() / self.settings.ramp_up_secs)
                    .clamp(0.0, 1.0);
                let phase = if progress < 0.35 {
                    MihPhase::Awakening
                } else if progress < 0.75 {
                    MihPhase::TimeAcceleration
                } else {
                    MihPhase::HeavenAscended
                };
                let offset = self
                    .retune
                    .map(|(at, delta)| {
                        delta
                            * (1.0
                                - smooth(0.0, 0.5, now.saturating_duration_since(at).as_secs_f32()))
                    })
                    .unwrap_or(0.0);
                MihSnapshot {
                    phase,
                    progress,
                    multiplier: (1.0
                        + (self.settings.max_multiplier - 1.0) * acceleration_curve(progress)
                        + offset)
                        .clamp(1.0, 6.0),
                    opacity: smooth(
                        0.0,
                        0.35,
                        now.saturating_duration_since(cycle_at).as_secs_f32(),
                    ),
                    ..MihSnapshot::default()
                }
            }
            State::Resetting {
                at,
                from_multiplier,
            } => {
                let elapsed = now.saturating_duration_since(at).as_secs_f32();
                let progress = (elapsed / self.settings.reset_secs).clamp(0.0, 1.0);
                MihSnapshot {
                    phase: MihPhase::UniverseReset,
                    progress: 1.0,
                    multiplier: 1.0 + (from_multiplier - 1.0) * (1.0 - smooth(0.0, 0.90, progress)),
                    opacity: 1.0 - smooth(0.60, 1.0, progress),
                    reset_progress: progress,
                    reset_elapsed: elapsed,
                }
            }
            State::Decelerating { at, duration, from } => {
                let k = smooth(
                    0.0,
                    duration,
                    now.saturating_duration_since(at).as_secs_f32(),
                );
                MihSnapshot {
                    phase: MihPhase::Decelerating,
                    progress: from.progress,
                    multiplier: 1.0 + (from.multiplier - 1.0) * (1.0 - k),
                    opacity: from.opacity * (1.0 - k),
                    ..MihSnapshot::default()
                }
            }
        }
    }

    fn begin_cycle(&mut self, now: Instant) {
        self.state = State::Accelerating {
            at: now,
            from_progress: 0.0,
            cycle_at: now,
        };
        self.acceleration_announced = false;
        self.retune = None;
    }

    fn begin(&mut self, now: Instant, settings: &MadeInHeavenSettings) -> MihEvents {
        let mut events = MihEvents::default();
        if !settings.enabled || !self.is_idle() {
            return events;
        }
        self.settings = settings.clone();
        self.settings.sanitize();
        self.activation_at = Some(now);
        self.held = true;
        self.begin_cycle(now);
        events.push(MihEvent::Activated);
        events
    }

    pub fn press(&mut self, now: Instant, settings: &MadeInHeavenSettings) -> MihEvents {
        if self.is_idle() {
            self.begin(now, settings)
        } else if self.settings.mode != TimeStopMode::Hold {
            self.cancel(now)
        } else {
            MihEvents::default()
        }
    }

    /// The existing tray's Activate/Deactivate item is a toggle in every mode.
    pub fn toggle(&mut self, now: Instant, settings: &MadeInHeavenSettings) -> MihEvents {
        if self.is_idle() {
            self.begin(now, settings)
        } else {
            self.cancel(now)
        }
    }

    pub fn release(&mut self, now: Instant) -> MihEvents {
        self.held = false;
        if self.settings.mode == TimeStopMode::Hold {
            self.cancel(now)
        } else {
            MihEvents::default()
        }
    }

    pub fn cancel(&mut self, now: Instant) -> MihEvents {
        let mut events = MihEvents::default();
        if matches!(self.state, State::Idle | State::Decelerating { .. }) {
            return events;
        }
        let from = self.snapshot(now);
        self.state = State::Decelerating {
            at: now,
            duration: self.settings.deceleration_secs,
            from,
        };
        self.retune = None;
        events.push(MihEvent::Deactivated);
        events
    }

    /// Master off / effect switch / emergency / exit: input is normal immediately.
    pub fn force_stop(&mut self) {
        self.state = State::Idle;
        self.activation_at = None;
        self.held = false;
        self.retune = None;
    }

    pub fn reconfigure(&mut self, settings: &MadeInHeavenSettings, now: Instant) {
        let before = self.snapshot(now);
        let timing_changed = self.settings.ramp_up_secs != settings.ramp_up_secs;
        let maximum_changed = self.settings.max_multiplier != settings.max_multiplier;
        let decel_changed = self.settings.deceleration_secs != settings.deceleration_secs;
        let reset_changed = self.settings.reset_secs != settings.reset_secs;
        let old_reset = self.settings.reset_secs;
        self.settings = settings.clone();
        self.settings.sanitize();
        match self.state {
            State::Accelerating { cycle_at, .. } => {
                if timing_changed {
                    self.state = State::Accelerating {
                        at: now,
                        from_progress: before.progress,
                        cycle_at,
                    };
                }
                if maximum_changed {
                    let target = 1.0
                        + (self.settings.max_multiplier - 1.0)
                            * acceleration_curve(before.progress);
                    self.retune = Some((now, before.multiplier - target));
                }
            }
            State::Decelerating { at, duration, .. } if decel_changed => {
                let remaining = (1.0 - now.saturating_duration_since(at).as_secs_f32() / duration)
                    .clamp(0.0, 1.0);
                self.state = State::Decelerating {
                    at: now,
                    duration: (self.settings.deceleration_secs * remaining).max(0.001),
                    from: before,
                };
            }
            State::Resetting {
                at,
                from_multiplier,
            } if reset_changed => {
                let k =
                    (now.saturating_duration_since(at).as_secs_f32() / old_reset).clamp(0.0, 1.0);
                let at = now
                    .checked_sub(Duration::from_secs_f32(k * self.settings.reset_secs))
                    .unwrap_or(now);
                self.state = State::Resetting {
                    at,
                    from_multiplier,
                };
            }
            _ => {}
        }
    }

    fn timed_deadline(&self) -> Option<Instant> {
        (self.settings.mode == TimeStopMode::Timed)
            .then(|| {
                self.activation_at
                    .map(|at| at + Duration::from_secs_f32(self.settings.duration_secs))
            })
            .flatten()
    }

    pub fn update(&mut self, now: Instant) -> MihEvents {
        let mut events = MihEvents::default();
        if let State::Accelerating {
            at, from_progress, ..
        } = self.state
        {
            let end =
                at + Duration::from_secs_f32((1.0 - from_progress) * self.settings.ramp_up_secs);
            let timed = self.timed_deadline();
            if let Some(deadline) = timed.filter(|t| *t < end && now >= *t) {
                for event in self.cancel(deadline) {
                    events.push(event);
                }
            } else {
                let snapshot = self.snapshot(now);
                if snapshot.progress >= 0.35 && !self.acceleration_announced {
                    self.acceleration_announced = true;
                    events.push(MihEvent::Acceleration);
                }
                if now >= end {
                    let maximum = self.snapshot(end).multiplier;
                    self.state = State::Resetting {
                        at: end,
                        from_multiplier: maximum,
                    };
                    self.retune = None;
                    events.push(MihEvent::Maximum);
                    events.push(MihEvent::Reset);
                }
            }
        }
        // A timed activation can expire partway through a reset too. Stop the
        // cycle, disable repeating, and finish a bounded smooth cleanup. If the
        // timer and maximum coincide, preserve the normal reset animation.
        if let State::Resetting { at, .. } = self.state
            && let Some(deadline) = self.timed_deadline()
            && deadline > at
            && now >= deadline
            && deadline < at + Duration::from_secs_f32(self.settings.reset_secs)
        {
            let remaining = (at + Duration::from_secs_f32(self.settings.reset_secs))
                .saturating_duration_since(deadline)
                .as_secs_f32();
            for event in self.cancel(deadline) {
                events.push(event);
            }
            if let State::Decelerating {
                ref mut duration, ..
            } = self.state
            {
                *duration = duration.min(remaining).max(0.001);
            }
        }
        match self.state {
            State::Resetting { at, .. }
                if now >= at + Duration::from_secs_f32(self.settings.reset_secs) =>
            {
                let repeat = self.settings.repeat_after_reset
                    && (self.settings.mode != TimeStopMode::Hold || self.held)
                    && self.timed_deadline().is_none_or(|deadline| now < deadline);
                self.state = State::Idle;
                events.push(MihEvent::Finished);
                if repeat {
                    // Never replay missed cycles after a stall. Begin ONE fresh cycle now.
                    self.begin_cycle(now);
                    events.push(MihEvent::Activated);
                } else {
                    self.activation_at = None;
                }
            }
            State::Decelerating { at, duration, .. }
                if now >= at + Duration::from_secs_f32(duration) =>
            {
                self.force_stop();
                events.push(MihEvent::Finished);
            }
            _ => {}
        }
        events
    }
}

#[derive(Clone, Copy)]
struct Tail {
    at: Instant,
    from: MihSnapshot,
}

pub struct MadeInHeaven {
    pub controller: MihController,
    ghosts: Ghosts,
    particles: Particles,
    trail: HeavenTrail,
    rng: Rng,
    since: Option<Instant>,
    cutoff: Option<Instant>,
    tail: Option<Tail>,
    reset_center: Vec2,
    head: Option<Vec2>,
    last_update: Option<Instant>,
    last_multiplier: f32,
    visual_time: f32,
}

impl Default for MadeInHeaven {
    fn default() -> Self {
        Self {
            controller: MihController::default(),
            ghosts: Ghosts::new(),
            particles: Particles::new(),
            trail: HeavenTrail::default(),
            rng: Rng::new(0x4D49_4811),
            since: None,
            cutoff: None,
            tail: None,
            reset_center: Vec2::ZERO,
            head: None,
            last_update: None,
            last_multiplier: 1.0,
            visual_time: 0.0,
        }
    }
}

impl MadeInHeaven {
    pub fn is_busy(&self) -> bool {
        !self.controller.is_idle()
    }

    pub fn snapshot(&self, now: Instant) -> MihSnapshot {
        if self.is_busy() {
            return self.controller.snapshot(now);
        }
        if let Some(tail) = self.tail {
            let opacity = tail.from.opacity
                * (1.0
                    - smooth(
                        0.0,
                        0.35,
                        now.saturating_duration_since(tail.at).as_secs_f32(),
                    ));
            return MihSnapshot {
                phase: MihPhase::Decelerating,
                multiplier: 1.0,
                opacity,
                ..tail.from
            };
        }
        MihSnapshot::default()
    }

    /// React to controller events once, never on every rendered frame.
    pub fn react(&mut self, events: MihEvents, now: Instant, pos: Vec2, cfg: &Config) {
        for event in events {
            match event {
                MihEvent::Activated => {
                    self.ghosts.clear();
                    self.particles.clear();
                    self.since = Some(now);
                    self.cutoff = None;
                    self.tail = None;
                    self.last_update = Some(now);
                    self.last_multiplier = 1.0;
                    self.visual_time = 0.0;
                    self.head = Some(pos);
                }
                MihEvent::Reset => {
                    self.reset_center = pos;
                    self.cutoff = Some(now);
                    if cfg.toggles.particles {
                        self.particles
                            .burst(pos, 65, 820.0 * cfg.scale, cfg, &mut self.rng);
                    }
                }
                MihEvent::Deactivated => {
                    self.cutoff = Some(now);
                    self.ghosts.stop_spawning();
                }
                MihEvent::Finished => {
                    self.ghosts.clear();
                    self.particles.clear();
                    self.since = None;
                    self.cutoff = None;
                    self.tail = None;
                }
                MihEvent::Acceleration | MihEvent::Maximum => {}
            }
        }
    }

    pub fn force_stop(&mut self, now: Instant) {
        let from = self.snapshot(now);
        self.controller.force_stop();
        self.tail = (from.opacity > 0.0).then_some(Tail { at: now, from });
        self.cutoff = Some(now);
        self.ghosts.stop_spawning();
    }

    pub fn clear(&mut self) {
        self.controller.force_stop();
        self.ghosts.clear();
        self.particles.clear();
        self.since = None;
        self.cutoff = None;
        self.tail = None;
        self.head = None;
    }

    pub fn update(&mut self, now: Instant, tracker: &CursorTracker, cfg: &Config) -> MihEvents {
        let events = self.controller.update(now);
        let pos = tracker.current().or(self.head).unwrap_or(Vec2::ZERO);
        self.react(events, now, pos, cfg);
        let snapshot = self.snapshot(now);
        let dt = self
            .last_update
            .map(|at| now.saturating_duration_since(at).as_secs_f32())
            .unwrap_or(0.0);
        self.last_update = Some(now);
        let accelerated_dt = dt * (self.last_multiplier + snapshot.multiplier) * 0.5;
        self.last_multiplier = snapshot.multiplier;
        self.visual_time = (self.visual_time + accelerated_dt) % 1000.0;
        self.head = tracker.current().or(self.head);
        self.particles
            .update(accelerated_dt, Particles::cap(cfg).min(180));
        self.ghosts.prune(now);
        let accelerating = matches!(
            snapshot.phase,
            MihPhase::Awakening | MihPhase::TimeAcceleration | MihPhase::HeavenAscended
        );
        if accelerating {
            let speed = tracker.speed(now, Duration::from_millis(60));
            if cfg.toggles.ghosts && cfg.intensity > 0.0 {
                self.ghosts
                    .spawn_heaven_from_motion(tracker, now, speed, snapshot.progress, cfg);
            }
            if cfg.toggles.particles {
                let dir = tracker
                    .direction(now, Duration::from_millis(80), 2.0)
                    .unwrap_or(Vec2::X);
                self.particles.emit_heaven(
                    pos,
                    dir,
                    speed,
                    accelerated_dt,
                    snapshot.progress,
                    cfg,
                    &mut self.rng,
                );
            }
        }
        if !cfg.toggles.ghosts || cfg.intensity <= 0.0 {
            self.ghosts.clear();
        }
        if !cfg.toggles.particles {
            self.particles.clear();
        }
        if self.tail.is_some() && snapshot.opacity <= 0.001 {
            self.tail = None;
            self.since = None;
            self.cutoff = None;
            self.ghosts.clear();
            self.particles.clear();
        }
        events
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        let settings = &self.controller.settings;
        let snapshot = self.snapshot(now);
        let visible = settings.indicator
            || (settings.intensity > 0
                && (settings.afterimages
                    || settings.motion_trails
                    || settings.speed_particles
                    || settings.heaven_aura
                    || snapshot.phase == MihPhase::UniverseReset));
        visible
            && (self.is_busy()
                || snapshot.opacity > 0.001
                || !self.ghosts.is_empty()
                || !self.particles.is_empty())
    }

    pub fn draw(
        &mut self,
        batch: &mut Batch,
        tracker: &CursorTracker,
        now: Instant,
        cfg: &Config,
        monitor: Rect,
    ) {
        let snapshot = self.snapshot(now);
        if snapshot.opacity <= 0.001 {
            return;
        }
        let p = snapshot.progress;
        let reset = snapshot.phase == MihPhase::UniverseReset;
        let dissolve = if reset {
            (-snapshot.reset_progress * 13.0).exp()
        } else {
            1.0
        };
        let opacity = snapshot.opacity;
        if cfg.toggles.trails {
            self.trail.draw(
                batch,
                tracker,
                now,
                self.since,
                self.cutoff,
                p,
                opacity * dissolve,
                cfg,
            );
        }
        if cfg.toggles.ghosts {
            self.ghosts.draw_heaven(
                batch,
                now,
                cfg,
                opacity * dissolve * cfg.intensity * (0.18 + 0.82 * p),
            );
        }
        if cfg.toggles.particles {
            self.particles.draw_heaven(batch, cfg, opacity);
        }
        if let Some(head) = self.head {
            if cfg.toggles.glow {
                self.draw_aura(batch, head, snapshot, cfg);
            }
            if cfg.toggles.trails && !reset {
                self.draw_streaks(batch, tracker, now, head, snapshot, cfg);
            }
        }
        if reset {
            self.draw_reset(batch, snapshot, cfg);
        }
        if self.controller.settings.indicator {
            self.draw_indicator(batch, snapshot, monitor, cfg);
        }
    }

    fn draw_aura(&self, batch: &mut Batch, pos: Vec2, snapshot: MihSnapshot, cfg: &Config) {
        let p = snapshot.progress;
        let energy = (0.14 + 0.70 * acceleration_curve(p)) * snapshot.opacity * cfg.intensity;
        let breathe = 1.0 + 0.045 * (self.visual_time * 2.1).sin();
        let r = (24.0 + 30.0 * p) * cfg.scale.max(0.75) * breathe;
        let center = pos + Vec2::new(5.0, 8.0) * cfg.scale;
        batch.disc_sharp(center, r * 1.5, CYAN, energy * 0.16, 1.0, 3.0);
        batch.disc(center, r, WHITE, energy * 0.19, 1.0);
        if let Some(cursor) = cfg.cursor {
            batch.sprite_glow(
                pos - cursor.hotspot,
                cursor.size,
                (3.0 + 4.0 * p) * cfg.scale,
                CYAN,
                energy * 0.65,
            );
            batch.sprite_glow(
                pos - cursor.hotspot,
                cursor.size,
                1.8 * cfg.scale,
                cfg.hot(WHITE, 1.6),
                energy * 0.55,
            );
        }
        // Subtle pressure rings become faster as the accelerated simulation clock advances.
        let pulse_level = smooth(0.28, 0.65, p) * energy;
        if pulse_level > 0.005 && !cfg.performance_mode {
            for i in 0..2 {
                let t = (self.visual_time * 0.35 + i as f32 * 0.5).fract();
                let radius = (20.0 + 105.0 * t) * cfg.scale;
                let a = pulse_level * (1.0 - t).powi(2) * smooth(0.0, 0.12, t) * 0.14;
                batch.ring(
                    center,
                    radius + 4.0,
                    radius / (radius + 4.0),
                    1.2 / (radius + 4.0),
                    CYAN,
                    a,
                    1.0,
                );
            }
        }
    }

    fn draw_streaks(
        &self,
        batch: &mut Batch,
        tracker: &CursorTracker,
        now: Instant,
        pos: Vec2,
        snapshot: MihSnapshot,
        cfg: &Config,
    ) {
        let speed = (tracker.speed(now, Duration::from_millis(60)) / 2200.0).clamp(0.0, 1.0);
        let phase = smooth(0.30, 0.85, snapshot.progress);
        let a = speed * phase * snapshot.opacity * cfg.intensity * 0.26;
        if a <= 0.003 {
            return;
        }
        let Some(dir) = tracker.direction(now, Duration::from_millis(60), 3.0) else {
            return;
        };
        let perp = Vec2::new(-dir.y, dir.x);
        let count = if cfg.performance_mode { 3 } else { 7 };
        for i in 0..count {
            let seed = i as f32 * 2.39996;
            let travel = (self.visual_time * (0.8 + i as f32 * 0.035) + seed).fract();
            let side = (i as f32 - (count - 1) as f32 * 0.5) * 9.0 * cfg.scale;
            let head = pos - dir * (12.0 + travel * 90.0 * phase) * cfg.scale + perp * side;
            let length = (24.0 + 100.0 * phase * speed) * cfg.scale;
            let fade = (std::f32::consts::PI * travel).sin().max(0.0);
            let points = [
                RibbonPoint {
                    pos: head - dir * length,
                    half_width: 0.2,
                    alpha: 0.0,
                },
                RibbonPoint {
                    pos: head - dir * length * 0.3,
                    half_width: 1.2 * cfg.scale,
                    alpha: a * fade,
                },
                RibbonPoint {
                    pos: head,
                    half_width: 0.3,
                    alpha: 0.0,
                },
            ];
            batch.ribbon(&points, if i % 3 == 0 { WHITE } else { CYAN }, 1.0);
        }
    }

    fn draw_reset(&self, batch: &mut Batch, snapshot: MihSnapshot, cfg: &Config) {
        let k = snapshot.reset_progress;
        // One short, translucent flash. Never turns the whole desktop opaque.
        if self.controller.settings.reset_flash {
            let flash = smooth(0.035, 0.10, snapshot.reset_elapsed)
                * (1.0 - smooth(0.13, 0.32, snapshot.reset_elapsed));
            batch.fill_all(
                [0.83, 0.97, 1.0],
                flash * 0.32 * cfg.intensity * snapshot.opacity,
            );
        }
        let travel = smooth(0.055, 0.84, k);
        let reach = batch.size.length();
        let radius = 12.0 + reach * travel;
        let fade = (1.0 - smooth(0.40, 1.0, k)) * smooth(0.0, 0.07, k) * cfg.intensity;
        let width = 10.0 + 24.0 * (1.0 - travel);
        let quad = radius + width * 4.0;
        batch.ring(
            self.reset_center,
            quad,
            radius / quad,
            width / quad,
            CYAN,
            fade * 0.34,
            1.0,
        );
        batch.ring(
            self.reset_center,
            quad,
            radius / quad,
            3.0 / quad,
            cfg.hot(WHITE, 2.0),
            fade * 0.62,
            0.85,
        );
        if !cfg.performance_mode {
            let trailing = radius * 0.90;
            batch.ring(
                self.reset_center,
                quad,
                trailing / quad,
                5.0 / quad,
                CYAN,
                fade * 0.17,
                1.0,
            );
        }
        batch.disc(
            self.reset_center,
            (65.0 + 100.0 * k) * cfg.scale,
            WHITE,
            fade * (-k * 7.0).exp() * 0.40,
            1.0,
        );
    }

    fn draw_indicator(
        &self,
        batch: &mut Batch,
        snapshot: MihSnapshot,
        monitor: Rect,
        cfg: &Config,
    ) {
        let scale = cfg.scale.clamp(0.85, 1.5);
        let size = Vec2::new(166.0, 72.0) * scale;
        let margin = 24.0 * scale;
        let mut at = Vec2::new(monitor.right - size.x - margin, monitor.top + margin);
        if self
            .head
            .is_some_and(|p| p.x > at.x - 20.0 && p.y < at.y + size.y + 20.0)
        {
            at.x = monitor.left + margin; // never draw over the real cursor
        }
        at = Vec2::new(at.x.max(monitor.left), at.y.max(monitor.top));
        let a = snapshot.opacity;
        batch.rect(at, size, [0.018, 0.035, 0.052], a * 0.68);
        batch.rect(at, Vec2::new(2.0 * scale, size.y), CYAN, a * 0.75);
        let content = at + Vec2::new(12.0, 10.0) * scale;
        text::draw(
            batch,
            "MADE IN HEAVEN",
            content,
            1.3 * scale,
            WHITE,
            a * 0.92,
        );
        let tenth = (snapshot.multiplier * 10.0).round().clamp(10.0, 60.0) as u8;
        let digits = [b'0' + tenth / 10, b'.', b'0' + tenth % 10, 0xc3, 0x97];
        let multiplier = std::str::from_utf8(&digits).unwrap_or("1.0x");
        text::draw(
            batch,
            multiplier,
            content + Vec2::new(0.0, 17.0) * scale,
            2.8 * scale,
            WHITE,
            a,
        );
        text::draw(
            batch,
            snapshot.phase.label(),
            content + Vec2::new(0.0, 43.0) * scale,
            1.0 * scale,
            CYAN,
            a * 0.9,
        );
        batch.rect(
            at + Vec2::new(12.0, 63.0) * scale,
            Vec2::new(142.0, 2.0) * scale,
            CYAN,
            a * 0.12,
        );
        batch.rect(
            at + Vec2::new(12.0, 63.0) * scale,
            Vec2::new(142.0 * snapshot.progress, 2.0) * scale,
            CYAN,
            a * 0.7,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(events: MihEvents) -> Vec<MihEvent> {
        events.into_iter().collect()
    }
    fn at(start: Instant, seconds: f32) -> Instant {
        start + Duration::from_secs_f32(seconds)
    }

    #[test]
    fn default_ramp_is_gradual_nonlinear_and_independent_of_frame_rate() {
        let start = Instant::now();
        let cfg = MadeInHeavenSettings::default();
        let mut c = MihController::default();
        assert_eq!(events(c.press(start, &cfg)), vec![MihEvent::Activated]);
        for (seconds, expected) in [
            (0.0, 1.0),
            (5.0, 1.33),
            (10.0, 1.99),
            (15.0, 2.89),
            (20.0, 4.0),
        ] {
            assert!((c.snapshot(at(start, seconds)).multiplier - expected).abs() < 0.025);
        }
        let sparse = c.snapshot(at(start, 12.5)).multiplier;
        for i in 0..1000 {
            c.update(at(start, i as f32 * 0.0125));
        }
        assert_eq!(c.snapshot(at(start, 12.5)).multiplier, sparse);
    }

    #[test]
    fn phases_and_sounds_cross_boundaries_once() {
        let start = Instant::now();
        let mut c = MihController::default();
        c.press(start, &MadeInHeavenSettings::default());
        assert_eq!(c.snapshot(at(start, 6.99)).phase, MihPhase::Awakening);
        assert_eq!(c.snapshot(at(start, 7.0)).phase, MihPhase::TimeAcceleration);
        assert_eq!(
            events(c.update(at(start, 7.0))),
            vec![MihEvent::Acceleration]
        );
        assert!(events(c.update(at(start, 8.0))).is_empty());
        assert_eq!(c.snapshot(at(start, 15.0)).phase, MihPhase::HeavenAscended);
        assert_eq!(
            events(c.update(at(start, 20.0))),
            vec![MihEvent::Maximum, MihEvent::Reset]
        );
        assert!(events(c.update(at(start, 20.1))).is_empty());
        assert_eq!(c.snapshot(at(start, 20.1)).phase, MihPhase::UniverseReset);
    }

    #[test]
    fn toggle_cancels_with_smooth_deceleration_and_no_repeat() {
        let start = Instant::now();
        let mut c = MihController::default();
        let cfg = MadeInHeavenSettings {
            repeat_after_reset: true,
            ..Default::default()
        };
        c.press(start, &cfg);
        let t = at(start, 10.0);
        let speed = c.snapshot(t).multiplier;
        assert_eq!(events(c.press(t, &cfg)), vec![MihEvent::Deactivated]);
        assert_eq!(c.snapshot(t).multiplier, speed);
        assert!(c.snapshot(at(t, 0.75)).multiplier < speed);
        assert!(c.snapshot(at(t, 0.75)).multiplier > 1.0);
        assert_eq!(events(c.update(at(t, 1.5))), vec![MihEvent::Finished]);
        assert!(c.is_idle());
        assert_eq!(c.snapshot(at(t, 1.5)).multiplier, 1.0);
        assert!(events(c.update(at(t, 120.0))).is_empty());
    }

    #[test]
    fn hold_release_in_ramp_or_reset_never_repeats() {
        for seconds in [8.0, 20.3] {
            let start = Instant::now();
            let mut c = MihController::default();
            let cfg = MadeInHeavenSettings {
                mode: TimeStopMode::Hold,
                repeat_after_reset: true,
                ..Default::default()
            };
            c.press(start, &cfg);
            c.update(at(start, seconds));
            assert_eq!(
                events(c.release(at(start, seconds))),
                vec![MihEvent::Deactivated]
            );
            c.update(at(start, seconds + 1.5));
            assert!(c.is_idle());
            assert_eq!(c.snapshot(at(start, seconds + 2.0)).multiplier, 1.0);
        }
    }

    #[test]
    fn timed_expiry_preempts_a_later_reset_and_limits_looping() {
        let start = Instant::now();
        let mut c = MihController::default();
        let cfg = MadeInHeavenSettings {
            mode: TimeStopMode::Timed,
            duration_secs: 8.0,
            repeat_after_reset: true,
            ..Default::default()
        };
        c.press(start, &cfg);
        assert_eq!(
            events(c.update(at(start, 8.0))),
            vec![MihEvent::Deactivated]
        );
        c.update(at(start, 9.5));
        assert!(c.is_idle());
        let cfg = MadeInHeavenSettings {
            duration_secs: 20.0,
            ..cfg
        };
        c.press(start, &cfg);
        assert!(events(c.update(at(start, 20.0))).contains(&MihEvent::Reset));
        assert_eq!(events(c.update(at(start, 21.5))), vec![MihEvent::Finished]);
        assert!(c.is_idle());
    }

    #[test]
    fn timed_expiry_during_reset_never_restarts_a_cycle() {
        let start = Instant::now();
        let mut controller = MihController::default();
        let settings = MadeInHeavenSettings {
            mode: TimeStopMode::Timed,
            duration_secs: 20.5,
            reset_secs: 5.0,
            repeat_after_reset: true,
            ..Default::default()
        };
        controller.press(start, &settings);
        controller.update(at(start, 20.0));
        assert_eq!(
            controller.snapshot(at(start, 20.0)).phase,
            MihPhase::UniverseReset
        );
        assert_eq!(
            events(controller.update(at(start, 20.5))),
            vec![MihEvent::Deactivated]
        );
        assert_eq!(
            events(controller.update(at(start, 22.0))),
            vec![MihEvent::Finished]
        );
        assert!(controller.is_idle());
        assert_eq!(controller.snapshot(at(start, 22.0)).multiplier, 1.0);
    }

    #[test]
    fn visual_free_mode_keeps_the_clock_but_needs_no_gpu_frames() {
        let start = Instant::now();
        let mut effect = MadeInHeaven::default();
        let settings = MadeInHeavenSettings {
            intensity: 0,
            indicator: false,
            ..Default::default()
        };
        effect.controller.press(start, &settings);
        assert!(effect.is_busy());
        assert!(!effect.is_animating(start));
        assert!(effect.snapshot(at(start, 10.0)).multiplier > 1.0);
    }

    #[test]
    fn universe_reset_restores_one_x_then_goes_inactive() {
        let start = Instant::now();
        let mut c = MihController::default();
        c.press(start, &MadeInHeavenSettings::default());
        c.update(at(start, 20.0));
        assert_eq!(c.snapshot(at(start, 20.0)).multiplier, 4.0);
        let halfway = c.snapshot(at(start, 20.75)).multiplier;
        assert!(halfway > 1.0 && halfway < 4.0);
        assert_eq!(events(c.update(at(start, 21.5))), vec![MihEvent::Finished]);
        assert!(c.is_idle());
        assert_eq!(c.snapshot(at(start, 21.5)).multiplier, 1.0);
    }

    #[test]
    fn loop_begins_exactly_one_new_cycle_and_manual_reset_cancel_stops_it() {
        let start = Instant::now();
        let cfg = MadeInHeavenSettings {
            repeat_after_reset: true,
            ..Default::default()
        };
        let mut c = MihController::default();
        c.press(start, &cfg);
        c.update(at(start, 20.0));
        assert_eq!(
            events(c.update(at(start, 21.5))),
            vec![MihEvent::Finished, MihEvent::Activated]
        );
        assert_eq!(c.snapshot(at(start, 21.5)).multiplier, 1.0);
        assert_eq!(c.snapshot(at(start, 21.5)).progress, 0.0);
        c.update(at(start, 41.5));
        c.cancel(at(start, 41.7));
        c.update(at(start, 44.0));
        assert!(c.is_idle());
    }

    #[test]
    fn forced_stop_disabled_effect_and_duplicate_press_are_safe() {
        let start = Instant::now();
        let mut c = MihController::default();
        let cfg = MadeInHeavenSettings {
            enabled: false,
            ..Default::default()
        };
        assert!(events(c.press(start, &cfg)).is_empty());
        let cfg = MadeInHeavenSettings {
            enabled: true,
            mode: TimeStopMode::Hold,
            ..cfg
        };
        c.press(start, &cfg);
        assert!(events(c.press(at(start, 1.0), &cfg)).is_empty());
        for seconds in [2.0, 12.0, 20.5] {
            c.force_stop();
            c.press(start, &cfg);
            c.update(at(start, seconds));
            c.force_stop();
            assert!(c.is_idle());
            assert_eq!(c.snapshot(at(start, seconds)).multiplier, 1.0);
        }
    }

    #[test]
    fn settings_apply_without_a_sudden_multiplier_or_progress_jump() {
        let start = Instant::now();
        let mut c = MihController::default();
        let mut cfg = MadeInHeavenSettings::default();
        c.press(start, &cfg);
        let t = at(start, 10.0);
        let before = c.snapshot(t);
        cfg.max_multiplier = 6.0;
        cfg.ramp_up_secs = 40.0;
        c.reconfigure(&cfg, t);
        assert!((c.snapshot(t).multiplier - before.multiplier).abs() < 0.0001);
        assert_eq!(c.snapshot(t).progress, before.progress);
        assert!((c.snapshot(at(t, 10.0)).progress - 0.75).abs() < 0.0001);
        cfg.max_multiplier = 1.0;
        let t = at(t, 10.0);
        let before = c.snapshot(t).multiplier;
        c.reconfigure(&cfg, t);
        assert!((c.snapshot(t).multiplier - before).abs() < 0.0001);
        assert_eq!(c.snapshot(at(t, 0.5)).multiplier, 1.0);
    }

    #[test]
    fn delayed_update_finishes_without_replaying_missed_cycles() {
        let start = Instant::now();
        let mut c = MihController::default();
        let cfg = MadeInHeavenSettings {
            repeat_after_reset: true,
            ..Default::default()
        };
        c.press(start, &cfg);
        let ev = events(c.update(at(start, 250.0)));
        assert_eq!(ev.iter().filter(|e| **e == MihEvent::Activated).count(), 1);
        assert_eq!(c.snapshot(at(start, 250.0)).multiplier, 1.0);
    }

    #[test]
    fn visuals_fade_and_gpu_geometry_stays_finite_and_bounded() {
        let start = Instant::now();
        let mut fx = MadeInHeaven::default();
        let settings = crate::settings::config::Settings::default();
        let mut cfg = crate::app::effect_config(&settings);
        cfg.style.palette = [CYAN, WHITE, CYAN, WHITE, CYAN, WHITE];
        cfg.max_particles = 80;
        let ev = fx.controller.press(start, &settings.made_in_heaven);
        fx.react(ev, start, Vec2::splat(200.0), &cfg);
        let mut tr = CursorTracker::new();
        let mut batch = Batch::new(Vec2::new(-1920.0, 0.0), Vec2::new(3840.0, 1080.0));
        let mon = Rect {
            left: 0.0,
            top: 0.0,
            right: 1920.0,
            bottom: 1080.0,
        };
        for i in 0..1350 {
            let t = i as f32 / 60.0;
            let now = at(start, t);
            tr.push(
                Vec2::new(
                    700.0 + (t * 2.0).sin() * 350.0,
                    500.0 + (t * 3.0).cos() * 200.0,
                ),
                now,
                false,
            );
            fx.update(now, &tr, &cfg);
            batch.reset(Vec2::new(-1920.0, 0.0), Vec2::new(3840.0, 1080.0));
            fx.draw(&mut batch, &tr, now, &cfg, mon);
            assert!(fx.ghosts.len() <= 12);
            assert!(fx.particles.len() <= 80);
            assert!(batch.verts.len() <= Batch::MAX_VERTS);
            assert!(batch.verts.iter().all(|v| {
                v.pos
                    .iter()
                    .chain(v.color.iter())
                    .chain(v.params.iter())
                    .all(|n| n.is_finite())
            }));
        }
        assert!(!fx.is_busy());
        assert!(!fx.is_animating(at(start, 23.0)));
        assert!(batch.is_empty());
    }
}
