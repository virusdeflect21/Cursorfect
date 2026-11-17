//! Real-cursor tracking: a bounded, timestamped ring buffer of cursor positions
//! plus derived kinematics, and the pure geometry used by the warp dash.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use glam::Vec2;

const CAPACITY: usize = 768;

#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub pos: Vec2,
    pub t: Instant,
    /// True if the cursor teleported to this sample (dash / monitor jump):
    /// trails must not connect it to the previous sample.
    pub brk: bool,
}

#[derive(Default)]
pub struct CursorTracker {
    ring: VecDeque<Sample>,
    last_move: Option<Instant>,
}

impl CursorTracker {
    pub fn new() -> Self {
        Self {
            ring: VecDeque::with_capacity(CAPACITY),
            last_move: None,
        }
    }

    pub fn push(&mut self, pos: Vec2, t: Instant, brk: bool) {
        if let Some(last) = self.ring.back() {
            if t < last.t {
                return; // keep strictly monotonic
            }
            if last.pos == pos && !brk {
                return;
            }
            self.last_move = Some(t);
        } else {
            self.last_move = Some(t);
        }
        if self.ring.len() == CAPACITY {
            self.ring.pop_front();
        }
        self.ring.push_back(Sample { pos, t, brk });
    }

    pub fn clear(&mut self) {
        self.ring.clear();
        self.last_move = None;
    }

    pub fn samples(&self) -> impl DoubleEndedIterator<Item = &Sample> {
        self.ring.iter()
    }

    pub fn len(&self) -> usize {
        self.ring.len()
    }

    pub fn current(&self) -> Option<Vec2> {
        self.ring.back().map(|s| s.pos)
    }

    pub fn previous(&self) -> Option<Vec2> {
        let n = self.ring.len();
        if n >= 2 {
            Some(self.ring[n - 2].pos)
        } else {
            None
        }
    }

    pub fn delta(&self) -> Vec2 {
        match (self.current(), self.previous()) {
            (Some(c), Some(p)) => c - p,
            _ => Vec2::ZERO,
        }
    }

    pub fn time_since_move(&self, now: Instant) -> Option<Duration> {
        self.last_move.map(|t| now.saturating_duration_since(t))
    }

    /// Average velocity (px/s) over the last `window`, ending at `now`.
    pub fn velocity(&self, now: Instant, window: Duration) -> Vec2 {
        let Some(newest) = self.ring.back() else {
            return Vec2::ZERO;
        };
        if now.saturating_duration_since(newest.t) > window {
            return Vec2::ZERO; // cursor has been still for the whole window
        }
        let start_t = now.checked_sub(window).unwrap_or(now);
        // Oldest sample inside the window, stopping at teleports.
        let mut oldest = newest;
        for s in self.ring.iter().rev() {
            if s.t < start_t {
                break;
            }
            oldest = s;
            if s.brk {
                break;
            }
        }
        let dt = newest.t.saturating_duration_since(oldest.t).as_secs_f32();
        if dt < 1e-4 {
            return Vec2::ZERO;
        }
        (newest.pos - oldest.pos) / dt
    }

    pub fn speed(&self, now: Instant, window: Duration) -> f32 {
        self.velocity(now, window).length()
    }

    /// Change in velocity (px/s^2) between the two most recent windows.
    pub fn acceleration(&self, now: Instant, window: Duration) -> Vec2 {
        let v_now = self.velocity(now, window);
        let earlier = now.checked_sub(window).unwrap_or(now);
        let v_prev = self.velocity(earlier, window);
        (v_now - v_prev) / window.as_secs_f32().max(1e-3)
    }

    /// Unit direction of travel over the last `window`, if the cursor moved at
    /// least `min_dist` pixels (so tiny jitter yields `None`).
    pub fn direction(&self, now: Instant, window: Duration, min_dist: f32) -> Option<Vec2> {
        let newest = self.ring.back()?;
        let start_t = now.checked_sub(window).unwrap_or(now);
        let mut oldest = newest;
        for s in self.ring.iter().rev() {
            if s.t < start_t {
                break;
            }
            oldest = s;
            if s.brk {
                break;
            }
        }
        let d = newest.pos - oldest.pos;
        if d.length() >= min_dist {
            Some(d.normalize())
        } else {
            None
        }
    }

    /// Position at time `t`, linearly interpolated between samples. Times
    /// before the oldest sample return the oldest, times after the newest the newest.
    pub fn pos_at(&self, t: Instant) -> Option<Vec2> {
        let newest = self.ring.back()?;
        if t >= newest.t {
            return Some(newest.pos);
        }
        let mut next = *newest;
        for s in self.ring.iter().rev().skip(1) {
            if s.t <= t {
                if next.brk {
                    return Some(next.pos);
                }
                let span = next.t.saturating_duration_since(s.t).as_secs_f32();
                if span < 1e-6 {
                    return Some(next.pos);
                }
                let f = t.saturating_duration_since(s.t).as_secs_f32() / span;
                return Some(s.pos.lerp(next.pos, f.clamp(0.0, 1.0)));
            }
            next = *s;
        }
        self.ring.front().map(|s| s.pos)
    }
}

/// Axis-aligned rectangle in virtual-desktop pixels (right/bottom exclusive).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Rect {
    pub fn contains(&self, p: Vec2) -> bool {
        p.x >= self.left && p.x < self.right && p.y >= self.top && p.y < self.bottom
    }

    pub fn clamp(&self, p: Vec2) -> Vec2 {
        Vec2::new(
            p.x.clamp(self.left, self.right - 1.0),
            p.y.clamp(self.top, self.bottom - 1.0),
        )
    }
}

pub fn union_rect(rects: &[Rect]) -> Option<Rect> {
    let first = rects.first()?;
    Some(rects.iter().fold(*first, |a, r| Rect {
        left: a.left.min(r.left),
        top: a.top.min(r.top),
        right: a.right.max(r.right),
        bottom: a.bottom.max(r.bottom),
    }))
}

/// Dash destination: `start + dir * distance`, pulled back along the ray until it
/// lies on a real monitor (works with negative coordinates and irregular
/// multi-monitor layouts). Returns `None` if no valid destination farther than
/// 1 pixel exists.
pub fn dash_endpoint(start: Vec2, dir: Vec2, distance: f32, monitors: &[Rect]) -> Option<Vec2> {
    if monitors.is_empty() || dir.length_squared() < 1e-6 || distance <= 0.0 {
        return None;
    }
    let dir = dir.normalize();
    let step = 4.0_f32;
    let mut d = distance;
    while d > 1.0 {
        let p = start + dir * d;
        if let Some(r) = monitors.iter().find(|r| r.contains(p)) {
            return Some(r.clamp(p).round());
        }
        d -= step;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker_line(speed_px_s: f32, n: usize) -> (CursorTracker, Instant) {
        let t0 = Instant::now();
        let mut tr = CursorTracker::new();
        for i in 0..n {
            let t = t0 + Duration::from_millis(i as u64 * 10);
            tr.push(
                Vec2::new(100.0 + speed_px_s * 0.01 * i as f32, 200.0),
                t,
                false,
            );
        }
        let now = t0 + Duration::from_millis((n as u64 - 1) * 10);
        (tr, now)
    }

    #[test]
    fn velocity_and_direction() {
        let (tr, now) = tracker_line(1000.0, 20);
        let v = tr.velocity(now, Duration::from_millis(50));
        assert!((v.x - 1000.0).abs() < 5.0, "{v:?}");
        assert!(v.y.abs() < 1e-3);
        let d = tr.direction(now, Duration::from_millis(100), 4.0).unwrap();
        assert!((d - Vec2::X).length() < 1e-3);
        assert!(tr.delta().x > 0.0);
        assert_eq!(tr.current().unwrap().y, 200.0);
    }

    #[test]
    fn still_cursor_has_no_direction_or_speed() {
        let (tr, now) = tracker_line(1000.0, 20);
        let later = now + Duration::from_millis(500);
        assert_eq!(tr.velocity(later, Duration::from_millis(50)), Vec2::ZERO);
        assert!(
            tr.direction(later, Duration::from_millis(100), 4.0)
                .is_none()
        );
        assert!(tr.time_since_move(later).unwrap() >= Duration::from_millis(500));
    }

    #[test]
    fn ring_is_bounded() {
        let t0 = Instant::now();
        let mut tr = CursorTracker::new();
        for i in 0..(CAPACITY * 3) {
            tr.push(
                Vec2::new(i as f32, 0.0),
                t0 + Duration::from_micros(i as u64 * 100),
                false,
            );
        }
        assert_eq!(tr.len(), CAPACITY);
    }

    #[test]
    fn interpolation_between_samples() {
        let t0 = Instant::now();
        let mut tr = CursorTracker::new();
        tr.push(Vec2::new(0.0, 0.0), t0, false);
        tr.push(
            Vec2::new(100.0, 0.0),
            t0 + Duration::from_millis(100),
            false,
        );
        let p = tr.pos_at(t0 + Duration::from_millis(25)).unwrap();
        assert!((p.x - 25.0).abs() < 0.01);
        // Teleport samples are never interpolated across.
        tr.push(Vec2::new(900.0, 0.0), t0 + Duration::from_millis(200), true);
        let p = tr.pos_at(t0 + Duration::from_millis(150)).unwrap();
        assert_eq!(p.x, 900.0);
    }

    #[test]
    fn dash_endpoint_full_distance() {
        let screen = [Rect {
            left: 0.0,
            top: 0.0,
            right: 1920.0,
            bottom: 1080.0,
        }];
        let p = dash_endpoint(Vec2::new(500.0, 500.0), Vec2::X, 350.0, &screen).unwrap();
        assert_eq!(p, Vec2::new(850.0, 500.0));
    }

    #[test]
    fn dash_endpoint_clamps_to_screen_edge() {
        let screen = [Rect {
            left: 0.0,
            top: 0.0,
            right: 1920.0,
            bottom: 1080.0,
        }];
        let p = dash_endpoint(Vec2::new(1800.0, 500.0), Vec2::X, 350.0, &screen).unwrap();
        assert!(p.x < 1920.0 && p.x >= 1900.0, "{p:?}");
    }

    #[test]
    fn dash_endpoint_negative_coordinates_and_multi_monitor() {
        // Left monitor sits at negative X.
        let mons = [
            Rect {
                left: -1920.0,
                top: 0.0,
                right: 0.0,
                bottom: 1080.0,
            },
            Rect {
                left: 0.0,
                top: 0.0,
                right: 2560.0,
                bottom: 1440.0,
            },
        ];
        let p = dash_endpoint(Vec2::new(100.0, 300.0), -Vec2::X, 350.0, &mons).unwrap();
        assert_eq!(p, Vec2::new(-250.0, 300.0));
        // Down the gap below the shorter left monitor: pulled back onto a valid monitor.
        let q = dash_endpoint(Vec2::new(-100.0, 1000.0), Vec2::Y, 350.0, &mons).unwrap();
        assert!(mons.iter().any(|r| r.contains(q)));
    }

    #[test]
    fn dash_without_direction_does_nothing() {
        let screen = [Rect {
            left: 0.0,
            top: 0.0,
            right: 100.0,
            bottom: 100.0,
        }];
        assert!(dash_endpoint(Vec2::new(50.0, 50.0), Vec2::ZERO, 350.0, &screen).is_none());
        assert!(dash_endpoint(Vec2::new(50.0, 50.0), Vec2::X, 350.0, &[]).is_none());
    }

    #[test]
    fn rect_clamping() {
        let r = Rect {
            left: -100.0,
            top: -50.0,
            right: 100.0,
            bottom: 50.0,
        };
        assert_eq!(r.clamp(Vec2::new(500.0, -500.0)), Vec2::new(99.0, -50.0));
        let u = union_rect(&[
            r,
            Rect {
                left: 0.0,
                top: 0.0,
                right: 300.0,
                bottom: 10.0,
            },
        ])
        .unwrap();
        assert_eq!(u.right, 300.0);
        assert_eq!(u.left, -100.0);
    }
}
