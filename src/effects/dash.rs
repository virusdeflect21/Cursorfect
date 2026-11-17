//! Bright energy streak drawn from the dash start to its destination.

use std::time::Instant;

use glam::Vec2;

use super::{Batch, Config, RibbonPoint, mix, palette_at};

const LIFE: f32 = 0.42;

struct Streak {
    from: Vec2,
    to: Vec2,
    born: Instant,
}

pub struct Dashes {
    list: Vec<Streak>,
}

impl Dashes {
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

    pub fn spawn(&mut self, from: Vec2, to: Vec2, now: Instant, _cfg: &Config) {
        if self.list.len() >= 4 {
            self.list.remove(0);
        }
        self.list.push(Streak {
            from,
            to,
            born: now,
        });
    }

    pub fn prune(&mut self, now: Instant) {
        self.list
            .retain(|s| now.saturating_duration_since(s.born).as_secs_f32() < LIFE);
    }

    pub fn draw(&self, batch: &mut Batch, now: Instant, cfg: &Config) {
        let st = &cfg.style;
        let c0 = palette_at(&st.palette, 0.0);
        let c1 = palette_at(&st.palette, 5.0);
        let core = mix(c0, [1.0; 3], 0.7);
        for s in &self.list {
            let k = (now.saturating_duration_since(s.born).as_secs_f32() / LIFE).clamp(0.0, 1.0);
            let fade = (1.0 - k).powf(1.4);
            let thin = (1.0 - k * 0.8).max(0.1);
            let base = 6.0 * st.dash_width * cfg.scale.max(0.75) * thin;
            let a = fade * (0.5 + 0.5 * cfg.intensity);
            // Tapered streak: thin at the origin, thick and bright at the destination.
            let seg = 12;
            let pts = |wmul: f32, amul: f32| -> Vec<RibbonPoint> {
                (0..=seg)
                    .map(|i| {
                        let f = i as f32 / seg as f32;
                        RibbonPoint {
                            pos: s.from.lerp(s.to, f),
                            half_width: base * wmul * (0.15 + 0.85 * f),
                            alpha: a * amul * (0.15 + 0.85 * f),
                        }
                    })
                    .collect()
            };
            batch.ribbon(&pts(3.6, 0.45), c1, 1.0);
            batch.ribbon(&pts(1.4, 0.9), cfg.hot(c0, 1.4), 0.7);
            batch.ribbon(&pts(0.45, 1.0), cfg.hot(core, 2.6), 0.6);
            // Speed lines: thin parallel streaks that lag behind the main beam.
            let along = (s.to - s.from).normalize_or_zero();
            let perp = glam::Vec2::new(-along.y, along.x);
            let len = s.from.distance(s.to);
            for (i, off) in [-1.0f32, -0.55, 0.6, 1.05].iter().enumerate() {
                let shift = perp * off * base * 4.5;
                let lag = 0.15 + 0.12 * i as f32;
                let a0 = s.from.lerp(s.to, lag + 0.3 * k) + shift;
                let a1 = s.from.lerp(s.to, (0.92 - 0.1 * i as f32).max(lag + 0.1)) + shift;
                if len < 20.0 {
                    break;
                }
                let line = [
                    RibbonPoint {
                        pos: a0,
                        half_width: base * 0.15,
                        alpha: 0.0,
                    },
                    RibbonPoint {
                        pos: a0.lerp(a1, 0.7),
                        half_width: base * 0.28,
                        alpha: a * 0.7,
                    },
                    RibbonPoint {
                        pos: a1,
                        half_width: base * 0.1,
                        alpha: 0.0,
                    },
                ];
                let c = palette_at(&st.palette, 1.0 + i as f32);
                batch.ribbon(&line, cfg.hot(c, 1.6), 1.0);
            }
            // Impact flash at the destination.
            batch.disc(
                s.to,
                40.0 * cfg.scale.max(0.75) * (0.5 + k),
                cfg.hot(c0, 1.8),
                fade * 0.6,
                0.95,
            );
        }
    }
}

impl Default for Dashes {
    fn default() -> Self {
        Self::new()
    }
}
