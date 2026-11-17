//! Activation shockwave. With the live desktop available it is an *air ripple*: the screen
//! itself refracts in expanding concentric waves (done in the world pass, see
//! [`Shockwaves::ripple`]). Without desktop capture a neon ring is drawn instead.

use std::time::Instant;

use glam::Vec2;

use super::{Batch, Config, mix, palette_at};

const LIFE: f32 = 0.55;
/// The air ripple travels further and lasts longer than the drawn ring.
const RIPPLE_LIFE: f32 = 1.15;
const RIPPLE_REACH: f32 = 900.0;

struct Shock {
    center: Vec2,
    born: Instant,
    max_radius: f32,
}

pub struct Shockwaves {
    list: Vec<Shock>,
}

impl Shockwaves {
    pub fn new() -> Self {
        Self {
            list: Vec::with_capacity(4),
        }
    }

    pub fn clear(&mut self) {
        self.list.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn spawn(&mut self, center: Vec2, now: Instant, cfg: &Config) {
        self.spawn_scaled(center, now, cfg, 1.0);
    }

    /// A shockwave `size` times the normal radius (speed bursts use a smaller one).
    pub fn spawn_scaled(&mut self, center: Vec2, now: Instant, cfg: &Config, size: f32) {
        if self.list.len() >= 4 {
            self.list.remove(0);
        }
        let max_radius = 150.0
            * size
            * cfg.style.shock_scale
            * cfg.scale.max(0.75)
            * (0.6 + 0.4 * cfg.intensity);
        self.list.push(Shock {
            center,
            born: now,
            max_radius,
        });
    }

    pub fn prune(&mut self, now: Instant) {
        self.list
            .retain(|s| now.saturating_duration_since(s.born).as_secs_f32() < RIPPLE_LIFE);
    }

    /// Newest air ripple at `now`: (centre, front radius px, amplitude 0..1).
    pub fn ripple(&self, now: Instant, cfg: &Config) -> Option<(Vec2, f32, f32)> {
        let s = self.list.last()?;
        let k = now.saturating_duration_since(s.born).as_secs_f32() / RIPPLE_LIFE;
        if k >= 1.0 {
            return None;
        }
        let ease = 1.0 - (1.0 - k).powf(2.2);
        let reach = RIPPLE_REACH * cfg.style.shock_scale * cfg.scale.max(0.75);
        let amp = (1.0 - k).powf(1.4) * (0.5 + 0.5 * cfg.intensity);
        Some((s.center, 20.0 + reach * ease, amp))
    }

    pub fn draw(&self, batch: &mut Batch, now: Instant, cfg: &Config) {
        let st = &cfg.style;
        let (c0, c1) = (palette_at(&st.palette, 0.0), palette_at(&st.palette, 4.0));
        for s in &self.list {
            let k = now.saturating_duration_since(s.born).as_secs_f32() / LIFE;
            if k >= 1.0 {
                continue;
            }
            let ease = 1.0 - (1.0 - k).powi(3);
            let r = (s.max_radius * ease).max(4.0);
            let fade = (1.0 - k).powf(1.5);
            let a = fade * st.shock_alpha * (0.4 + 0.6 * cfg.intensity);
            // Bright pulse at the cursor, decaying in the first ~40% of the wave.
            let pulse = (1.0 - k * 2.5).max(0.0);
            batch.disc(
                s.center,
                36.0 * cfg.scale.max(0.75) * (0.6 + ease),
                cfg.hot(mix(c0, [1.0; 3], 0.4), 2.2),
                pulse * 0.9 * st.shock_alpha,
                0.95,
            );
            // The ring: outer glow + crisp inner line. The quad is slightly larger than the ring.
            let quad_r = r * 1.25 + 6.0;
            let mid = r / quad_r;
            batch.ring(
                s.center,
                quad_r,
                mid,
                0.10 + 0.05 * (1.0 - k),
                c0,
                a * 0.8,
                0.8,
            );
            batch.ring(
                s.center,
                quad_r,
                mid,
                0.035,
                cfg.hot(mix(c1, [1.0; 3], 0.3), 1.8),
                a,
                0.5,
            );
            // Chromatic aberration: red and blue copies drift apart as the ring grows.
            let spread = 0.018 * ease;
            batch.ring(s.center, quad_r, mid + spread, 0.02, cfg.hot([1.0, 0.1, 0.3], 1.5), a * 0.6, 1.0);
            batch.ring(s.center, quad_r, (mid - spread).max(0.01), 0.02, cfg.hot([0.15, 0.5, 1.0], 1.5), a * 0.6, 1.0);
        }
    }
}

impl Default for Shockwaves {
    fn default() -> Self {
        Self::new()
    }
}
