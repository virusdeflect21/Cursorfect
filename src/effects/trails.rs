//! Smooth luminous motion trail built from the cursor history each frame.

use std::time::Instant;

use glam::Vec2;

use super::{Batch, Config, RibbonPoint, TRAIL_LIFETIME, mix};
use crate::input::cursor::CursorTracker;

/// Collect trail points newest -> oldest as `(position, age_seconds)`.
/// Samples after `cutoff` (deactivation time) are ignored; the path stops at teleports.
pub fn collect(
    tracker: &CursorTracker,
    now: Instant,
    cutoff: Option<Instant>,
    min_spacing: f32,
    max_points: usize,
) -> Vec<(Vec2, f32)> {
    let mut pts: Vec<(Vec2, f32)> = Vec::with_capacity(64);
    for s in tracker.samples().rev() {
        if cutoff.is_some_and(|c| s.t > c) {
            continue;
        }
        let age = now.saturating_duration_since(s.t).as_secs_f32();
        if age > TRAIL_LIFETIME {
            break;
        }
        let keep = pts
            .last()
            .is_none_or(|(p, _)| p.distance(s.pos) >= min_spacing);
        if keep {
            pts.push((s.pos, age));
            if pts.len() >= max_points {
                break;
            }
        }
        if s.brk {
            break; // do not connect across a teleport
        }
    }
    pts
}

/// MIH's progressively longer trail reuses both the cursor history and its own
/// geometry scratch buffers. No new history queue or per-frame allocations.
pub struct HeavenTrail {
    samples: Vec<(Vec2, f32)>,
    curve: Vec<(Vec2, f32)>,
    ribbon: Vec<RibbonPoint>,
}

impl Default for HeavenTrail {
    fn default() -> Self {
        Self {
            samples: Vec::with_capacity(160),
            curve: Vec::with_capacity(320),
            ribbon: Vec::with_capacity(320),
        }
    }
}

impl HeavenTrail {
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        batch: &mut Batch,
        tracker: &CursorTracker,
        now: Instant,
        since: Option<Instant>,
        cutoff: Option<Instant>,
        progress: f32,
        opacity: f32,
        cfg: &Config,
    ) {
        self.samples.clear();
        self.curve.clear();
        let life = 0.12 + 0.36 * progress.powf(1.3);
        let spacing = if cfg.performance_mode { 7.0 } else { 3.0 } * cfg.scale.max(0.75);
        let cap = if cfg.performance_mode { 80 } else { 160 };
        for sample in tracker.samples().rev() {
            if cutoff.is_some_and(|t| sample.t > t) {
                continue;
            }
            let age = now.saturating_duration_since(sample.t).as_secs_f32();
            if age > life || since.is_some_and(|t| sample.t < t) {
                break;
            }
            if self
                .samples
                .last()
                .is_none_or(|(p, _)| p.distance(sample.pos) >= spacing)
            {
                self.samples.push((sample.pos, age));
                if self.samples.len() >= cap {
                    break;
                }
            }
            if sample.brk {
                break;
            }
        }
        if self.samples.len() < 2 {
            return;
        }
        // Hermite interpolation keeps the head and tail exactly on the real path.
        // Limited tangents prevent overshoot on sharp flicks and direction reversals.
        for i in 0..self.samples.len() - 1 {
            let (p0, a0) = self.samples[i];
            let (p1, a1) = self.samples[i + 1];
            let prev = self.samples[i.saturating_sub(1)].0;
            let next = self.samples[(i + 2).min(self.samples.len() - 1)].0;
            let limit = p0.distance(p1);
            let m0 = ((p1 - prev) * 0.5).clamp_length_max(limit);
            let m1 = ((next - p0) * 0.5).clamp_length_max(limit);
            for u in [0.0f32, 0.5] {
                let u2 = u * u;
                let u3 = u2 * u;
                let p = p0 * (2.0 * u3 - 3.0 * u2 + 1.0)
                    + m0 * (u3 - 2.0 * u2 + u)
                    + p1 * (-2.0 * u3 + 3.0 * u2)
                    + m1 * (u3 - u2);
                self.curve.push((p, a0 + (a1 - a0) * u));
            }
        }
        if let Some(last) = self.samples.last() {
            self.curve.push(*last);
        }
        let speed =
            (tracker.speed(now, std::time::Duration::from_millis(60)) / 2600.0).clamp(0.0, 1.0);
        let width = (1.4 + 2.6 * progress + 1.5 * speed) * cfg.scale.max(0.75);
        let bright = (0.18 + 0.72 * progress.powf(1.2)) * opacity * cfg.intensity;
        for (w, a, color, additive) in [
            (3.5, 0.26, [0.18, 0.82, 1.0], 1.0),
            (1.3, 0.64, [0.36, 0.94, 1.0], 0.85),
            (0.34, 0.82, cfg.hot([0.97, 1.0, 1.0], 1.8), 0.70),
        ] {
            if cfg.performance_mode && w > 3.0 {
                continue;
            }
            self.ribbon.clear();
            for (pos, age) in &self.curve {
                let fade = (1.0 - age / life).clamp(0.0, 1.0);
                self.ribbon.push(RibbonPoint {
                    pos: *pos,
                    half_width: width * w * fade.powf(0.8) + 0.15,
                    alpha: bright * a * fade.powf(1.3),
                });
            }
            batch.ribbon(&self.ribbon, color, additive);
        }
    }
}

pub fn draw(
    batch: &mut Batch,
    tracker: &CursorTracker,
    now: Instant,
    cutoff: Option<Instant>,
    speedf: f32,
    cfg: &Config,
) {
    let spacing = if cfg.performance_mode { 6.0 } else { 3.0 };
    let pts = collect(tracker, now, cutoff, spacing, 200);
    if pts.len() < 2 {
        return;
    }
    let st = &cfg.style;
    let base = (3.2 + 4.0 * speedf) * st.trail_width * cfg.scale.max(0.75);
    let bright = (0.40 + 0.60 * speedf) * (0.30 + 0.70 * cfg.intensity);
    let core_col = mix(st.echo_young, [1.0, 1.0, 1.0], 0.55);

    let shape = |wmul: f32, amul: f32| -> Vec<RibbonPoint> {
        pts.iter()
            .map(|(p, age)| {
                let k = (age / TRAIL_LIFETIME).clamp(0.0, 1.0);
                RibbonPoint {
                    pos: *p,
                    // Thinner as it ages.
                    half_width: base * wmul * (1.0 - k).powf(0.85) + 0.25,
                    alpha: bright * amul * (1.0 - k).powf(1.3),
                }
            })
            .collect()
    };
    // Additive-style layers: wide halo, body, hot core.
    if !cfg.performance_mode {
        batch.ribbon(&shape(3.4, 0.38), st.echo_old, 0.9);
    }
    batch.ribbon(&shape(1.5, 0.85), st.echo_young, 0.55);
    batch.ribbon(&shape(0.55, 1.0), cfg.hot(core_col, 1.8), 0.5);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn collects_only_recent_points_and_respects_cutoff() {
        let t0 = Instant::now();
        let mut tr = CursorTracker::new();
        for i in 0..100 {
            tr.push(
                Vec2::new(i as f32 * 5.0, 0.0),
                t0 + Duration::from_millis(i * 5),
                false,
            );
        }
        let now = t0 + Duration::from_millis(500);
        let pts = collect(&tr, now, None, 3.0, 200);
        assert!(!pts.is_empty());
        assert!(pts.iter().all(|(_, age)| *age <= TRAIL_LIFETIME));
        // Newest first, ages ascending.
        assert!(pts.windows(2).all(|w| w[0].1 <= w[1].1));
        // With a cutoff in the past, newer samples are ignored.
        let cut = collect(&tr, now, Some(t0 + Duration::from_millis(300)), 3.0, 200);
        assert!(cut.first().map(|p| p.1).unwrap_or(0.0) >= 0.19);
    }

    #[test]
    fn trail_stops_at_teleport() {
        let t0 = Instant::now();
        let mut tr = CursorTracker::new();
        tr.push(Vec2::new(0.0, 0.0), t0, false);
        tr.push(Vec2::new(10.0, 0.0), t0 + Duration::from_millis(10), false);
        tr.push(Vec2::new(900.0, 0.0), t0 + Duration::from_millis(20), true);
        tr.push(Vec2::new(910.0, 0.0), t0 + Duration::from_millis(30), false);
        let pts = collect(&tr, t0 + Duration::from_millis(40), None, 1.0, 50);
        assert!(pts.iter().all(|(p, _)| p.x >= 900.0), "{pts:?}");
    }

    #[test]
    fn heaven_trail_never_connects_teleports_or_pre_activation_history() {
        let start = Instant::now();
        let mut tracker = CursorTracker::new();
        tracker.push(Vec2::new(0.0, 50.0), start, false);
        tracker.push(
            Vec2::new(10.0, 50.0),
            start + Duration::from_millis(10),
            false,
        );
        tracker.push(
            Vec2::new(900.0, 50.0),
            start + Duration::from_millis(20),
            true,
        );
        tracker.push(
            Vec2::new(920.0, 50.0),
            start + Duration::from_millis(30),
            false,
        );
        let mut trail = HeavenTrail::default();
        let cfg = crate::app::heaven_config(&crate::settings::config::Settings::default());
        let mut batch = Batch::new(Vec2::ZERO, Vec2::splat(1000.0));
        trail.draw(
            &mut batch,
            &tracker,
            start + Duration::from_millis(40),
            Some(start),
            None,
            1.0,
            1.0,
            &cfg,
        );
        assert!(trail.samples.iter().all(|(p, _)| p.x >= 900.0));
        batch.reset(Vec2::ZERO, Vec2::splat(1000.0));
        trail.draw(
            &mut batch,
            &tracker,
            start + Duration::from_millis(40),
            Some(start + Duration::from_millis(35)),
            None,
            1.0,
            1.0,
            &cfg,
        );
        assert!(batch.is_empty());
    }
}
