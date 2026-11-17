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
}
