//! Bounded pool of small electric sparks.

use glam::Vec2;

use super::{Batch, Config, RibbonPoint, Rng, mix, palette_at};

#[derive(Clone, Copy, Debug)]
pub struct Particle {
    pub pos: Vec2,
    pub vel: Vec2,
    pub age: f32,
    pub life: f32,
    pub size: f32,
    pub alt_color: bool,
}

impl Particle {
    /// 1.0 when born, 0.0 when dead.
    pub fn opacity(&self) -> f32 {
        (1.0 - self.age / self.life).clamp(0.0, 1.0)
    }

    pub fn alive(&self) -> bool {
        self.age < self.life
    }
}

pub struct Particles {
    pool: Vec<Particle>,
    accum: f32,
}

/// Speed (px/s) above which sparks are emitted.
pub const SPEED_THRESHOLD: f32 = 700.0;

impl Particles {
    pub fn new() -> Self {
        Self {
            pool: Vec::with_capacity(512),
            accum: 0.0,
        }
    }

    pub fn clear(&mut self) {
        self.pool.clear();
        self.accum = 0.0;
    }

    pub fn is_empty(&self) -> bool {
        self.pool.is_empty()
    }

    pub fn len(&self) -> usize {
        self.pool.len()
    }

    fn push(&mut self, p: Particle, cap: usize) {
        if self.pool.len() >= cap {
            // Pool full: recycle the oldest particle instead of allocating.
            if let Some((i, _)) = self.pool.iter().enumerate().max_by(|a, b| {
                a.1.age
                    .partial_cmp(&b.1.age)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }) {
                self.pool[i] = p;
            }
        } else {
            self.pool.push(p);
        }
    }

    pub fn cap(cfg: &Config) -> usize {
        let c = if cfg.performance_mode {
            cfg.max_particles / 3
        } else {
            cfg.max_particles
        };
        c.min(500)
    }

    /// Radial burst (activation, dash).
    pub fn burst(&mut self, pos: Vec2, count: usize, speed: f32, cfg: &Config, rng: &mut Rng) {
        let cap = Self::cap(cfg);
        let count =
            ((count as f32) * (0.4 + 0.6 * cfg.intensity) * cfg.style.particles.max(0.5)) as usize;
        for _ in 0..count {
            let ang = rng.range(0.0, std::f32::consts::TAU);
            let sp = rng.range(0.3, 1.0) * speed;
            self.push(
                Particle {
                    pos,
                    vel: Vec2::from_angle(ang) * sp,
                    age: 0.0,
                    life: rng.range(0.25, 0.55),
                    size: rng.range(1.0, 2.4),
                    alt_color: rng.unit() > 0.5,
                },
                cap,
            );
        }
    }

    /// Continuous sparks while the cursor moves fast.
    pub fn emit_from_motion(
        &mut self,
        pos: Vec2,
        dir: Vec2,
        speed: f32,
        dt: f32,
        cfg: &Config,
        rng: &mut Rng,
    ) {
        if speed < SPEED_THRESHOLD {
            self.accum = 0.0;
            return;
        }
        let cap = Self::cap(cfg);
        let rate = ((speed - SPEED_THRESHOLD) / 900.0).clamp(0.0, 2.0)
            * 90.0
            * cfg.intensity
            * cfg.style.particles;
        self.accum += rate * dt;
        while self.accum >= 1.0 {
            self.accum -= 1.0;
            let perp = Vec2::new(-dir.y, dir.x);
            let jitter = perp * rng.range(-1.0, 1.0);
            self.push(
                Particle {
                    pos: pos + jitter * 4.0,
                    vel: -dir * speed * rng.range(0.02, 0.12) + jitter * rng.range(80.0, 320.0),
                    age: 0.0,
                    life: rng.range(0.18, 0.45),
                    size: rng.range(0.9, 2.0),
                    alt_color: rng.unit() > 0.6,
                },
                cap,
            );
        }
    }

    pub fn update(&mut self, dt: f32, cap: usize) {
        for p in &mut self.pool {
            p.age += dt;
            p.pos += p.vel * dt;
            p.vel *= (1.0 - 3.5 * dt).max(0.0);
        }
        self.pool.retain(Particle::alive);
        // Settings may have lowered the cap.
        if self.pool.len() > cap.max(1) * 2 {
            self.pool.truncate(cap.max(1) * 2);
        }
    }

    pub fn draw(&self, batch: &mut Batch, cfg: &Config) {
        let st = &cfg.style;
        for (i, p) in self.pool.iter().enumerate() {
            let o = p.opacity();
            let base = palette_at(&st.palette, i as f32 * 0.7);
            let col = if p.alt_color {
                mix(base, [1.0, 1.0, 1.0], 0.6)
            } else {
                base
            };
            let tail = (p.vel * 0.04).clamp_length_min(3.0);
            let dirn = if p.vel.length_squared() > 1e-4 {
                p.vel.normalize()
            } else {
                Vec2::X
            };
            let a = p.pos - dirn * tail.length();
            let pts = [
                RibbonPoint {
                    pos: a,
                    half_width: p.size * 0.4,
                    alpha: o * 0.1,
                },
                RibbonPoint {
                    pos: p.pos,
                    half_width: p.size,
                    alpha: o * 0.95,
                },
            ];
            batch.ribbon(&pts, cfg.hot(col, 1.0 + 1.6 * o), 0.6);
            batch.disc(
                p.pos,
                p.size * 3.6 * cfg.scale.max(0.75),
                base,
                o * 0.30,
                1.0,
            );
            // Tiny white-hot core: blooms into a star-like spark.
            batch.disc_sharp(
                p.pos,
                p.size * 1.4 * cfg.scale.max(0.75),
                cfg.hot([1.0, 1.0, 1.0], 2.5),
                o * 0.8,
                1.0,
                6.0,
            );
        }
    }
}

impl Default for Particles {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::config::Settings;

    fn cfg() -> Config {
        crate::app::effect_config(&Settings::default())
    }

    #[test]
    fn lifetime_and_opacity() {
        let mut p = Particle {
            pos: Vec2::ZERO,
            vel: Vec2::X,
            age: 0.0,
            life: 0.4,
            size: 1.0,
            alt_color: false,
        };
        assert_eq!(p.opacity(), 1.0);
        p.age = 0.2;
        assert!((p.opacity() - 0.5).abs() < 1e-5);
        p.age = 0.4;
        assert!(!p.alive());
        assert_eq!(p.opacity(), 0.0);
    }

    #[test]
    fn pool_is_bounded() {
        let c = cfg();
        let mut ps = Particles::new();
        let mut rng = Rng::new(7);
        for _ in 0..200 {
            ps.burst(Vec2::ZERO, 50, 500.0, &c, &mut rng);
        }
        assert!(
            ps.len() <= Particles::cap(&c),
            "{} > {}",
            ps.len(),
            Particles::cap(&c)
        );
    }

    #[test]
    fn particles_expire() {
        let c = cfg();
        let mut ps = Particles::new();
        let mut rng = Rng::new(3);
        ps.burst(Vec2::ZERO, 30, 400.0, &c, &mut rng);
        assert!(!ps.is_empty());
        for _ in 0..100 {
            ps.update(0.016, Particles::cap(&c));
        }
        assert!(ps.is_empty(), "all particles must die (max life 0.55s)");
    }

    #[test]
    fn slow_motion_emits_nothing() {
        let c = cfg();
        let mut ps = Particles::new();
        let mut rng = Rng::new(9);
        ps.emit_from_motion(Vec2::ZERO, Vec2::X, 100.0, 0.016, &c, &mut rng);
        assert!(ps.is_empty());
        for _ in 0..30 {
            ps.emit_from_motion(Vec2::ZERO, Vec2::X, 3000.0, 0.016, &c, &mut rng);
        }
        assert!(!ps.is_empty());
    }
}
