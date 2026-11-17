//! Light rays: thin, white-hot beams bursting out of a point (activation and dash impact).
//! With the HDR pipeline their cores bloom into a starburst.

use std::time::Instant;

use glam::Vec2;

use super::{Batch, Config, RibbonPoint, Rng, palette_at};

struct Ray {
    dir: Vec2,
    len: f32,
    width: f32,
    hue: f32,
    delay: f32,
}

struct Burst {
    center: Vec2,
    born: Instant,
    life: f32,
    rays: Vec<Ray>,
}

pub struct Rays {
    list: Vec<Burst>,
}

impl Rays {
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

    /// `count` rays up to `reach` px long; `bias` (unit vector or zero) concentrates them
    /// around a direction (used for the dash impact).
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        &mut self,
        center: Vec2,
        now: Instant,
        count: usize,
        reach: f32,
        life: f32,
        bias: Vec2,
        cfg: &Config,
        rng: &mut Rng,
    ) {
        if self.list.len() >= 4 {
            self.list.remove(0);
        }
        let count = if cfg.performance_mode {
            count / 2
        } else {
            count
        };
        let scale = cfg.scale.max(0.75) * (0.6 + 0.4 * cfg.intensity);
        let base = rng.range(0.0, std::f32::consts::TAU);
        let rays = (0..count)
            .map(|i| {
                let mut ang = base + i as f32 / count as f32 * std::f32::consts::TAU;
                ang += rng.range(-0.12, 0.12);
                let mut dir = Vec2::from_angle(ang);
                let mut len = reach * rng.range(0.45, 1.0);
                if bias != Vec2::ZERO {
                    // Fan forward: rays along the dash are longer.
                    let along = dir.dot(bias);
                    dir = (dir + bias * 0.6).normalize_or(dir);
                    len *= 0.55 + 0.45 * along.max(0.0);
                }
                Ray {
                    dir,
                    len: len * scale,
                    width: rng.range(0.6, 1.6) * scale,
                    hue: rng.range(0.0, 6.0),
                    delay: rng.range(0.0, 0.06),
                }
            })
            .collect();
        self.list.push(Burst {
            center,
            born: now,
            life,
            rays,
        });
    }

    pub fn prune(&mut self, now: Instant) {
        self.list
            .retain(|b| now.saturating_duration_since(b.born).as_secs_f32() < b.life + 0.08);
    }

    pub fn draw(&self, batch: &mut Batch, now: Instant, cfg: &Config) {
        for b in &self.list {
            let age = now.saturating_duration_since(b.born).as_secs_f32();
            for r in &b.rays {
                let k = ((age - r.delay) / b.life).clamp(0.0, 1.0);
                if k <= 0.0 || k >= 1.0 {
                    continue;
                }
                let ease = 1.0 - (1.0 - k).powi(3);
                let fade = (1.0 - k).powf(1.6);
                let head = b.center + r.dir * (12.0 + r.len * ease);
                let tail = b.center + r.dir * (8.0 + r.len * ease * ease * 0.55);
                let col = palette_at(&cfg.style.palette, r.hue);
                let a = fade * (0.55 + 0.45 * cfg.intensity);
                let pts = [
                    RibbonPoint {
                        pos: tail,
                        half_width: r.width * 0.3,
                        alpha: 0.0,
                    },
                    RibbonPoint {
                        pos: tail.lerp(head, 0.7),
                        half_width: r.width * 2.2,
                        alpha: a,
                    },
                    RibbonPoint {
                        pos: head,
                        half_width: r.width * 0.6,
                        alpha: a * 0.2,
                    },
                ];
                batch.ribbon(&pts, cfg.hot(col, 1.6), 0.95);
                let core: Vec<RibbonPoint> = pts
                    .iter()
                    .map(|p| RibbonPoint {
                        half_width: p.half_width * 0.35,
                        ..*p
                    })
                    .collect();
                batch.ribbon(&core, cfg.hot([1.0, 1.0, 1.0], 2.4), 1.0);
            }
        }
    }
}

impl Default for Rays {
    fn default() -> Self {
        Self::new()
    }
}
