//! Sandevistan afterimages: frozen copies of the *real* cursor stamped along its path at a
//! fixed time step, each in the next colour of the preset palette, fading out slowly.

use std::time::{Duration, Instant};

use glam::Vec2;

use super::{Batch, Config, mix, palette_at};
use crate::input::cursor::CursorTracker;
use crate::settings::presets::Rgb;

const MAX_GHOSTS: usize = 420;

/// 0 (slow) ..= 1 (very fast flick) from a speed in px/s.
pub fn speed_factor(speed: f32) -> f32 {
    ((speed - 60.0) / 2600.0).clamp(0.0, 1.0)
}

#[derive(Clone, Copy)]
struct Ghost {
    /// Cursor hotspot position when stamped.
    pos: Vec2,
    born: Instant,
    life: f32,
    alpha: f32,
    color: Rgb,
    /// Direction of travel when stamped (drives the RGB-split glitch edge).
    dir: Vec2,
    /// Speed factor when stamped: fast frames glow a little more and split wider.
    speedf: f32,
}

pub struct Ghosts {
    list: Vec<Ghost>,
    last_stamp: Option<Instant>,
    last_pos: Option<Vec2>,
    counter: u32,
    hue: f32,
}

/// The classic arrow, hotspot at the tip (0,0), in 32px-cursor units (fallback shape).
const ARROW: [Vec2; 7] = [
    Vec2::new(0.0, 0.0),
    Vec2::new(0.0, 16.0),
    Vec2::new(4.0, 12.5),
    Vec2::new(7.0, 19.0),
    Vec2::new(9.6, 18.0),
    Vec2::new(6.6, 11.6),
    Vec2::new(11.6, 11.6),
];
/// Ear-clipped triangulation of the concave arrow polygon.
const ARROW_TRIS: [[usize; 3]; 5] = [[0, 1, 2], [0, 2, 5], [0, 5, 6], [2, 3, 4], [2, 4, 5]];

impl Ghosts {
    pub fn new() -> Self {
        Self {
            list: Vec::with_capacity(MAX_GHOSTS),
            last_stamp: None,
            last_pos: None,
            counter: 0,
            hue: 0.0,
        }
    }

    pub fn clear(&mut self) {
        self.list.clear();
        self.stop_spawning();
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn stop_spawning(&mut self) {
        self.last_stamp = None;
        self.last_pos = None;
    }

    fn push(&mut self, g: Ghost) {
        if self.list.len() >= MAX_GHOSTS {
            self.list.remove(0);
        }
        self.list.push(g);
    }

    fn next_color(&mut self, cfg: &Config) -> Rgb {
        self.next_color_step(cfg, 0.15)
    }

    /// `step` palette entries per frame: ~7 frames per colour, so a string of frames sweeps
    /// through the whole palette like the rainbow of afterimages in the show.
    fn next_color_step(&mut self, cfg: &Config, step: f32) -> Rgb {
        self.hue += step;
        self.counter = self.counter.wrapping_add(1);
        palette_at(&cfg.style.palette, self.hue)
    }

    /// Add a frame explicitly (used by the warp dash).
    pub fn add_colored(&mut self, pos: Vec2, born: Instant, life: f32, alpha: f32, cfg: &Config) {
        let color = self.next_color(cfg);
        self.push(Ghost {
            pos,
            born,
            life,
            alpha,
            color,
            dir: Vec2::X,
            speedf: 1.0,
        });
    }

    /// Stamp frames at fixed *time* steps along the real path: the faster you move, the
    /// further apart the frames are, independent of the frame rate.
    pub fn spawn_from_motion(
        &mut self,
        tracker: &CursorTracker,
        now: Instant,
        speed: f32,
        cfg: &Config,
    ) {
        // The faster you move, the more frames: one every 22 ms when slow, down to one every
        // 6 ms on a fast flick, and they linger longer.
        let speedf = speed_factor(speed);
        let interval = if cfg.performance_mode {
            Duration::from_millis(40)
        } else {
            Duration::from_secs_f32((22.0 - 16.0 * speedf) / 1000.0)
        };
        let min_dist = 6.0 * cfg.scale.max(0.75);
        let dir = tracker
            .direction(now, Duration::from_millis(60), 2.0)
            .unwrap_or(Vec2::X);
        if speed < 60.0 {
            return;
        }
        let life = cfg.ghost_lifetime() * (0.85 + 0.65 * speedf);
        // Colour sweep keeps roughly the same length on screen at any frame density.
        let hue_step = 0.15 * interval.as_secs_f32() / 0.02;
        let mut t = match self.last_stamp {
            Some(s) if now.saturating_duration_since(s) < interval * 8 => s + interval,
            _ => now,
        };
        while t <= now {
            if let Some(pos) = tracker.pos_at(t) {
                if self.last_pos.is_none_or(|p| p.distance(pos) >= min_dist) {
                    let color = self.next_color_step(cfg, hue_step);
                    self.push(Ghost {
                        pos,
                        born: t,
                        life,
                        alpha: 1.0,
                        color,
                        dir,
                        speedf,
                    });
                    self.last_pos = Some(pos);
                }
            }
            self.last_stamp = Some(t);
            t += interval;
        }
    }

    pub fn prune(&mut self, now: Instant) {
        self.list
            .retain(|g| now.saturating_duration_since(g.born).as_secs_f32() < g.life);
    }

    pub fn draw(&self, batch: &mut Batch, now: Instant, cfg: &Config) {
        let scale = cfg.scale.max(0.5);
        for g in &self.list {
            let age = now.saturating_duration_since(g.born).as_secs_f32();
            let k = (age / g.life).clamp(0.0, 1.0);
            // Hold strongly, then fade: frames should read as frozen snapshots.
            let fade = (1.0 - k * k).powf(1.4);
            let a = (cfg.ghost_opacity * 1.4 * g.alpha * fade * (0.5 + 0.5 * cfg.intensity))
                .clamp(0.0, 1.0);
            if a < 0.01 {
                continue;
            }
            // Freshly stamped frames flash brighter for a moment (kept below bloom level so the
            // frame stays a crisp cursor shape instead of a glowing blob).
            let fresh = (1.0 - age / 0.08).clamp(0.0, 1.0);
            let col = mix(g.color, [1.0, 1.0, 1.0], fresh * 0.45);
            match cfg.cursor {
                Some(cs) => {
                    // Frames shrink slightly towards the hotspot as they age, like they are
                    // falling behind in time.
                    let shrink = 1.0 - 0.18 * k;
                    let size = cs.size * shrink;
                    let tl = g.pos - cs.hotspot * shrink;
                    if !cfg.performance_mode {
                        // Thin neon rim that hugs the real cursor silhouette.
                        let margin = (1.6 + 1.2 * (1.0 - k) + 1.2 * g.speedf) * scale;
                        batch.sprite_glow(tl, size, margin, g.color, a * (0.4 + 0.2 * g.speedf));
                        // RGB-split edges along the direction of travel on fresh frames,
                        // wider on fast flicks.
                        let split = g.dir * (1.0 + (1.5 + 2.5 * g.speedf) * (1.0 - k)) * scale;
                        let sa = a * (0.22 + 0.13 * g.speedf) * (1.0 - k);
                        batch.sprite(tl - split, size, [1.0, 0.18, 0.62], sa, 1.0);
                        batch.sprite(tl + split, size, [0.12, 0.95, 1.0], sa, 1.0);
                    }
                    batch.sprite(tl, size, col, a, 0.0);
                }
                None => {
                    let at = |p: Vec2| g.pos + p * scale * 1.3;
                    batch.disc(
                        at(Vec2::new(4.5, 8.0)),
                        26.0 * scale,
                        g.color,
                        a * 0.35,
                        0.9,
                    );
                    let centroid = Vec2::new(4.0, 9.0);
                    for tri in ARROW_TRIS {
                        let p = tri.map(|i| at(centroid + (ARROW[i] - centroid) * 1.25));
                        batch.tri(p, mix(g.color, [1.0; 3], 0.5), a * 0.6, 0.5);
                    }
                    for tri in ARROW_TRIS {
                        batch.tri(tri.map(|i| at(ARROW[i])), col, a, 0.1);
                    }
                }
            }
        }
    }
}

impl Default for Ghosts {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::Effects;
    use crate::settings::config::Settings;

    fn cfg() -> Config {
        crate::app::effect_config(&Settings::default())
    }

    #[test]
    fn spawns_along_motion_in_changing_colors_and_expires() {
        let t0 = Instant::now();
        let mut tr = CursorTracker::new();
        for i in 0..60 {
            tr.push(
                Vec2::new(i as f32 * 12.0, 100.0),
                t0 + Duration::from_millis(i * 8),
                false,
            );
        }
        let now = t0 + Duration::from_millis(59 * 8);
        let mut g = Ghosts::new();
        let c = cfg();
        // Called once per ~16 ms frame, like the engine does.
        let mut f = t0;
        while f <= now {
            g.spawn_from_motion(&tr, f, 1500.0, &c);
            f += Duration::from_millis(16);
        }
        assert!(g.len() >= 8, "ghosts: {}", g.len());
        assert!(g.len() <= MAX_GHOSTS);
        let first = g.list.first().unwrap().color;
        let last = g.list.last().unwrap().color;
        assert_ne!(first, last, "frames must cycle through colours");
        g.prune(now + Duration::from_secs(2));
        assert!(g.is_empty(), "ghosts must expire after their lifetime");
    }

    #[test]
    fn faster_motion_leaves_more_frames() {
        let count = |px_per_ms: f32| {
            let t0 = Instant::now();
            let mut tr = CursorTracker::new();
            for i in 0..100 {
                tr.push(
                    Vec2::new(i as f32 * 4.0 * px_per_ms, 100.0),
                    t0 + Duration::from_millis(i * 4),
                    false,
                );
            }
            let speed = px_per_ms * 1000.0;
            let mut g = Ghosts::new();
            let mut f = t0;
            while f <= t0 + Duration::from_millis(396) {
                g.spawn_from_motion(&tr, f, speed, &cfg());
                f += Duration::from_millis(16);
            }
            g.len()
        };
        let slow = count(0.4);
        let fast = count(3.5);
        assert!(fast > slow * 2, "slow {slow}, fast {fast}");
    }

    #[test]
    fn no_ghosts_when_cursor_is_still() {
        let t0 = Instant::now();
        let mut tr = CursorTracker::new();
        tr.push(Vec2::new(10.0, 10.0), t0, false);
        let mut g = Ghosts::new();
        g.spawn_from_motion(&tr, t0 + Duration::from_millis(50), 0.0, &cfg());
        assert!(g.is_empty());
    }

    #[test]
    fn effects_draw_nothing_when_idle() {
        let mut fx = Effects::new();
        let tr = CursorTracker::new();
        let mut b = Batch::new(Vec2::ZERO, Vec2::new(100.0, 100.0));
        fx.draw(&mut b, &tr, Instant::now(), &cfg(), 1000.0);
        assert!(b.is_empty());
        assert!(!fx.is_animating(Instant::now(), &tr));
    }
}
