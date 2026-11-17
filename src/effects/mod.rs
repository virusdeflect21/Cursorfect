//! CPU-side effect simulation. Every effect emits triangles into a [`Batch`], which the
//! renderer uploads to the GPU and draws in one call (after the black & white world pass).

pub mod dash;
pub mod ghosts;
pub mod particles;
pub mod rays;
pub mod shockwave;
pub mod the_world;
pub mod trails;

use std::time::Instant;

use glam::Vec2;

use crate::input::cursor::CursorTracker;
use crate::settings::config::EffectToggles;
use crate::settings::presets::{Rgb, Style};

/// One GPU vertex. Layout must match the HLSL input layout in `overlay/renderer.rs`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Vertex {
    /// Position in normalized device coordinates.
    pub pos: [f32; 2],
    /// Shape / texture coordinates (meaning depends on the shape mode).
    pub uv: [f32; 2],
    /// Straight (non-premultiplied) color, alpha.
    pub color: [f32; 4],
    /// x = shape mode, y/z = shape parameters, w = additive factor (0 = normal, 1 = additive).
    pub params: [f32; 4],
}

pub const MODE_DISC: f32 = 0.0;
pub const MODE_RING: f32 = 1.0;
pub const MODE_SOLID: f32 = 2.0;
pub const MODE_RIBBON: f32 = 3.0;
/// World pass (built by the renderer, not by effects).
pub const MODE_DESKTOP: f32 = 4.0;
/// Tinted copy of the real cursor bitmap.
pub const MODE_SPRITE: f32 = 5.0;
/// Neon halo hugging the cursor silhouette (dilated alpha of the cursor bitmap).
pub const MODE_SPRITE_GLOW: f32 = 6.0;

/// How the world pass grades the desktop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Look {
    /// Black & white, film grade.
    Sandevistan,
    /// The World: gold / violet frozen grade, colour-inversion sphere, flash.
    TimeStop {
        /// Radius (px) of the inverted-colour sphere around the centre; 0 = none.
        inversion: f32,
        /// 0..1 golden flash.
        flash: f32,
        /// 0..1 gold / violet split-tone.
        tint: f32,
        /// 0..1 chromatic separation on the sphere rim.
        chroma: f32,
        /// 0..1 lens pinch towards the centre (the instant before time stops).
        pinch: f32,
    },
}

/// Parameters of the black & white world pass for one frame.
#[derive(Clone, Copy, Debug)]
pub struct World {
    /// Wave centre in virtual-desktop pixels.
    pub center: Vec2,
    /// Wave radius in pixels.
    pub radius: f32,
    /// Soft edge width of the wave front in pixels.
    pub feather: f32,
    /// 0..1 overall desaturation strength.
    pub amount: f32,
    /// false: grey inside the radius (draining); true: grey outside it (colour flooding back).
    pub invert: bool,
    pub contrast: f32,
    /// 0..1 strength of the lens warp / chromatic fringe on the travelling wave front.
    pub front: f32,
    /// 0..1 digital glitch (torn slices + RGB split) on the world.
    pub glitch: f32,
    /// Air ripple: centre, front radius (px) and amplitude (0 = none).
    pub ripple: (Vec2, f32, f32),
    pub look: Look,
}

/// Accumulates triangles in virtual-desktop pixel coordinates.
pub struct Batch {
    pub verts: Vec<Vertex>,
    origin: Vec2,
    size: Vec2,
}

#[derive(Clone, Copy)]
pub struct RibbonPoint {
    pub pos: Vec2,
    pub half_width: f32,
    pub alpha: f32,
}

impl Batch {
    pub const MAX_VERTS: usize = 60_000;

    pub fn new(origin: Vec2, size: Vec2) -> Self {
        Self {
            verts: Vec::with_capacity(8192),
            origin,
            size,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.verts.is_empty()
    }

    fn v(&mut self, p: Vec2, uv: [f32; 2], c: Rgb, a: f32, params: [f32; 4]) {
        if self.verts.len() >= Self::MAX_VERTS {
            return;
        }
        let q = p - self.origin;
        self.verts.push(Vertex {
            pos: [q.x / self.size.x * 2.0 - 1.0, 1.0 - q.y / self.size.y * 2.0],
            uv,
            color: [c[0], c[1], c[2], a.clamp(0.0, 1.0)],
            params,
        });
    }

    fn quad(&mut self, corners: [Vec2; 4], uvs: [[f32; 2]; 4], c: Rgb, a: f32, params: [f32; 4]) {
        for i in [0usize, 1, 2, 0, 2, 3] {
            self.v(corners[i], uvs[i], c, a, params);
        }
    }

    /// Full-surface rectangle (fallback world dim when desktop capture is unavailable).
    pub fn fill_all(&mut self, c: Rgb, a: f32) {
        if a <= 0.002 {
            return;
        }
        let (o, s) = (self.origin, self.size);
        let p = [o, o + Vec2::new(s.x, 0.0), o + s, o + Vec2::new(0.0, s.y)];
        self.quad(p, [[0.0; 2]; 4], c, a, [MODE_SOLID, 0.0, 0.0, 0.0]);
    }

    /// Soft radial glow (gaussian falloff).
    pub fn disc(&mut self, center: Vec2, radius: f32, c: Rgb, a: f32, add: f32) {
        self.disc_sharp(center, radius, c, a, add, 0.0);
    }

    /// Radial glow with a custom falloff: `sharp` 0 = default (4), higher = tighter core.
    pub fn disc_sharp(&mut self, center: Vec2, radius: f32, c: Rgb, a: f32, add: f32, sharp: f32) {
        if a <= 0.002 || radius < 0.5 {
            return;
        }
        let r = Vec2::splat(radius);
        let corners = [
            center - r,
            center + Vec2::new(r.x, -r.y),
            center + r,
            center + Vec2::new(-r.x, r.y),
        ];
        let uvs = [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]];
        self.quad(corners, uvs, c, a, [MODE_DISC, sharp, 0.0, add]);
    }

    /// Ring whose center line is at `radius * mid` with half thickness `radius * half`.
    #[allow(clippy::too_many_arguments)]
    pub fn ring(
        &mut self,
        center: Vec2,
        radius: f32,
        mid: f32,
        half: f32,
        c: Rgb,
        a: f32,
        add: f32,
    ) {
        if a <= 0.002 || radius < 1.0 {
            return;
        }
        let r = Vec2::splat(radius);
        let corners = [
            center - r,
            center + Vec2::new(r.x, -r.y),
            center + r,
            center + Vec2::new(-r.x, r.y),
        ];
        let uvs = [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]];
        self.quad(corners, uvs, c, a, [MODE_RING, mid, half.max(0.0005), add]);
    }

    pub fn tri(&mut self, p: [Vec2; 3], c: Rgb, a: f32, add: f32) {
        if a <= 0.002 {
            return;
        }
        for q in p {
            self.v(q, [0.0, 0.0], c, a, [MODE_SOLID, 0.0, 0.0, add]);
        }
    }

    /// Tinted copy of the real cursor bitmap, `top_left` .. `top_left + size`.
    pub fn sprite(&mut self, top_left: Vec2, size: Vec2, c: Rgb, a: f32, add: f32) {
        if a <= 0.002 {
            return;
        }
        let corners = [
            top_left,
            top_left + Vec2::new(size.x, 0.0),
            top_left + size,
            top_left + Vec2::new(0.0, size.y),
        ];
        let uvs = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        self.quad(corners, uvs, c, a, [MODE_SPRITE, 0.0, 0.0, add]);
    }

    /// Neon halo around the cursor silhouette, `margin` px wide, always additive.
    pub fn sprite_glow(&mut self, top_left: Vec2, size: Vec2, margin: f32, c: Rgb, a: f32) {
        if a <= 0.002 || size.x < 1.0 || size.y < 1.0 {
            return;
        }
        let m = Vec2::splat(margin);
        let tl = top_left - m;
        let br = top_left + size + m;
        let corners = [tl, Vec2::new(br.x, tl.y), br, Vec2::new(tl.x, br.y)];
        let (u0, v0) = (-margin / size.x, -margin / size.y);
        let (u1, v1) = (1.0 - u0, 1.0 - v0);
        let uvs = [[u0, v0], [u1, v0], [u1, v1], [u0, v1]];
        // Sampling radius in uv units (the shader dilates the alpha over it).
        let (rx, ry) = (margin * 0.62 / size.x, margin * 0.62 / size.y);
        self.quad(corners, uvs, c, a, [MODE_SPRITE_GLOW, rx, ry, 1.0]);
    }

    /// A smooth ribbon through `pts` (ordered along the path). Width and alpha
    /// are interpolated per point; edges fade softly.
    pub fn ribbon(&mut self, pts: &[RibbonPoint], c: Rgb, add: f32) {
        if pts.len() < 2 {
            return;
        }
        let n = pts.len();
        let mut left = Vec::with_capacity(n);
        let mut right = Vec::with_capacity(n);
        for i in 0..n {
            let prev = pts[i.saturating_sub(1)].pos;
            let next = pts[(i + 1).min(n - 1)].pos;
            let t = (next - prev).normalize_or_zero();
            let t = if t == Vec2::ZERO { Vec2::X } else { t };
            let nrm = Vec2::new(-t.y, t.x) * pts[i].half_width;
            left.push(pts[i].pos + nrm);
            right.push(pts[i].pos - nrm);
        }
        for i in 0..n - 1 {
            let (a0, a1) = (pts[i].alpha, pts[i + 1].alpha);
            if a0 <= 0.002 && a1 <= 0.002 {
                continue;
            }
            let p = [MODE_RIBBON, 0.0, 0.0, add];
            self.v(left[i], [-1.0, 0.0], c, a0, p);
            self.v(right[i], [1.0, 0.0], c, a0, p);
            self.v(left[i + 1], [-1.0, 0.0], c, a1, p);
            self.v(right[i], [1.0, 0.0], c, a0, p);
            self.v(right[i + 1], [1.0, 0.0], c, a1, p);
            self.v(left[i + 1], [-1.0, 0.0], c, a1, p);
        }
    }
}

/// Small xorshift RNG (no allocation, deterministic seed for tests).
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        Self(seed.max(1))
    }

    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x >> 8) as f32 / (1u32 << 24) as f32
    }

    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }
}

pub fn scale(c: Rgb, k: f32) -> Rgb {
    [c[0] * k, c[1] * k, c[2] * k]
}

pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// Sample the preset palette cyclically and smoothly (`t` = 1.0 per palette entry).
pub fn palette_at(p: &[Rgb], t: f32) -> Rgb {
    if p.is_empty() {
        return [1.0; 3];
    }
    let n = p.len() as f32;
    let t = t.rem_euclid(n);
    let i = t.floor() as usize % p.len();
    let j = (i + 1) % p.len();
    mix(p[i], p[j], t.fract())
}

/// Size/hotspot of the captured real cursor bitmap (None: fall back to a vector arrow).
#[derive(Clone, Copy, Debug)]
pub struct CursorSprite {
    pub size: Vec2,
    pub hotspot: Vec2,
}

/// Everything an effect needs to know about the current configuration.
#[derive(Clone)]
pub struct Config {
    pub toggles: EffectToggles,
    pub style: Style,
    /// 0..=1
    pub intensity: f32,
    pub ghost_count: usize,
    /// 0..=1
    pub ghost_opacity: f32,
    pub max_particles: usize,
    pub performance_mode: bool,
    /// Cursor size relative to a 32px cursor (DPI / accessibility cursor size).
    pub scale: f32,
    pub cursor: Option<CursorSprite>,
    /// Real black & white world pass available (else a dim veil is used).
    pub world_supported: bool,
    /// Duration of the colour-drain wave on activation (seconds).
    pub ramp_in: f32,
    /// Duration of the colour flooding back on deactivation (seconds).
    pub ramp_out: f32,
    /// The renderer has a float (HDR) target: colours above 1.0 become bloom, not clipping.
    pub hdr: bool,
}

impl Config {
    /// An "over-bright" colour for glowing cores. With HDR the value goes above 1.0 and turns
    /// into bloom / white-hot highlights; without it the colour is pushed towards white instead.
    pub fn hot(&self, c: Rgb, k: f32) -> Rgb {
        if self.hdr {
            // Kept moderate: highlights glow, shapes stay readable.
            scale(c, 1.0 + (k - 1.0) * 0.6)
        } else {
            mix(c, [1.0; 3], ((k - 1.0) * 0.2).clamp(0.0, 0.6))
        }
    }

    /// Afterimage lifetime: each step of the "Ghost Count" slider keeps echoes 60 ms longer
    /// (frames are stamped every 20 ms, so 14 -> ~42 frames trailing a moving cursor).
    pub fn ghost_lifetime(&self) -> f32 {
        (self.ghost_count as f32 * 0.06).clamp(0.15, 1.6)
    }
}

pub const TRAIL_LIFETIME: f32 = 0.25;

pub struct Effects {
    pub ghosts: ghosts::Ghosts,
    pub particles: particles::Particles,
    pub shocks: shockwave::Shockwaves,
    pub dashes: dash::Dashes,
    pub rays: rays::Rays,
    rng: Rng,
    active: bool,
    /// When effects were last deactivated; trails/ghosts are not created past this instant.
    cutoff: Option<Instant>,
    glow: f32,
    flash: f32,
    world_on_at: Option<Instant>,
    world_off_at: Option<Instant>,
    /// World level reached when deactivated (so a quick toggle reverses smoothly).
    world_center: Vec2,
    last_update: Option<Instant>,
    /// Running glitch burst: (start, duration s, strength).
    glitch: Option<(Instant, f32, f32)>,
    /// Last "sonic boom" (speed burst) so they don't fire every frame.
    last_boom: Option<Instant>,
    /// The cursor dropped below the boom speed since the last one (re-arms the boom).
    boom_armed: bool,
}

/// Cursor speed (px/s) that triggers a speed burst: ring + rays + sparks.
const BOOM_SPEED: f32 = 3400.0;

/// The wave front warps hardest right after it leaves the cursor and calms down at the edges.
fn front_strength(radius: f32, max_radius: f32) -> f32 {
    let travel = (radius / max_radius.max(1.0)).clamp(0.0, 1.0);
    if travel >= 0.999 {
        0.0
    } else {
        (1.0 - travel).powf(0.6) * (radius / 60.0).clamp(0.0, 1.0)
    }
}

fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

impl Effects {
    pub fn new() -> Self {
        Self {
            ghosts: ghosts::Ghosts::new(),
            particles: particles::Particles::new(),
            shocks: shockwave::Shockwaves::new(),
            dashes: dash::Dashes::new(),
            rays: rays::Rays::new(),
            rng: Rng::new(0xC0FF_EE11),
            active: false,
            cutoff: None,
            glow: 0.0,
            flash: 0.0,
            world_on_at: None,
            world_off_at: None,
            world_center: Vec2::ZERO,
            last_update: None,
            last_boom: None,
            boom_armed: true,
            glitch: None,
        }
    }

    pub fn clear(&mut self) {
        self.ghosts.clear();
        self.particles.clear();
        self.shocks.clear();
        self.dashes.clear();
        self.rays.clear();
        self.glitch = None;
        self.glow = 0.0;
        self.flash = 0.0;
        self.active = false;
        self.cutoff = None;
        self.world_on_at = None;
        self.world_off_at = None;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    fn ripple(&self, now: Instant, cfg: &Config) -> (Vec2, f32, f32) {
        if !cfg.toggles.ripples {
            return (Vec2::ZERO, 0.0, 0.0);
        }
        self.shocks
            .ripple(now, cfg)
            .unwrap_or((Vec2::ZERO, 0.0, 0.0))
    }

    fn start_glitch(&mut self, now: Instant, dur: f32, strength: f32) {
        let current = self.glitch_level(now);
        if strength >= current {
            self.glitch = Some((now, dur, strength));
        }
    }

    /// Current glitch amount: decays quickly and flickers, like a corrupted signal.
    pub fn glitch_level(&self, now: Instant) -> f32 {
        let Some((t0, dur, strength)) = self.glitch else {
            return 0.0;
        };
        let t = now.saturating_duration_since(t0).as_secs_f32();
        let k = t / dur.max(0.01);
        if k >= 1.0 {
            return 0.0;
        }
        // Stutter: the glitch drops out for a few frames now and then.
        let flick = if ((t * 31.0).sin() * 43758.545).fract().abs() < 0.25 {
            0.35
        } else {
            1.0
        };
        strength * (1.0 - k) * (1.0 - k) * flick
    }

    /// Activation VFX: colour-drain wave, pulse, shockwave and an electric burst.
    pub fn activate(&mut self, pos: Vec2, now: Instant, cfg: &Config) {
        self.start_glitch(now, 0.6, 1.0);
        self.active = true;
        self.cutoff = None;
        self.world_center = pos;
        self.world_on_at = Some(now);
        self.world_off_at = None;
        if cfg.toggles.shockwave {
            self.shocks.spawn(pos, now, cfg);
        }
        if cfg.toggles.glow || cfg.toggles.shockwave {
            self.flash = 1.0;
        }
        if cfg.toggles.particles {
            self.particles.burst(pos, 34, 680.0, cfg, &mut self.rng);
        }
        if cfg.toggles.light_rays {
            self.rays.spawn(
                pos,
                now,
                28,
                420.0,
                0.62,
                Vec2::ZERO,
                cfg,
                &mut self.rng,
            );
        }
    }

    /// Deactivation: stop creating new elements; colour floods back from the cursor.
    pub fn deactivate(&mut self, pos: Vec2, now: Instant) {
        self.start_glitch(now, 0.35, 0.7);
        self.active = false;
        self.cutoff = Some(now);
        if self.world_on_at.is_some() {
            self.world_center = pos;
            self.world_off_at = Some(now);
        }
        self.world_on_at = None;
    }

    pub fn dash(&mut self, from: Vec2, to: Vec2, now: Instant, cfg: &Config) {
        self.start_glitch(now, 0.25, 0.65);
        self.dashes.spawn(from, to, now, cfg);
        if cfg.toggles.ghosts {
            // A string of frozen frames along the dash path.
            let n = if cfg.performance_mode { 8 } else { 22 };
            for i in 0..n {
                let f = i as f32 / n as f32;
                let life = cfg.ghost_lifetime() * (0.6 + 0.6 * f);
                self.ghosts
                    .add_colored(from.lerp(to, f), now, life, 1.0, cfg);
            }
        }
        if cfg.toggles.particles {
            let mid = from.lerp(to, 0.5);
            self.particles.burst(to, 18, 520.0, cfg, &mut self.rng);
            self.particles.burst(mid, 8, 260.0, cfg, &mut self.rng);
        }
        if cfg.toggles.light_rays {
            let dir = (to - from).normalize_or_zero();
            self.rays
                .spawn(to, now, 14, 220.0, 0.38, dir, cfg, &mut self.rng);
        }
        if cfg.toggles.glow {
            self.flash = self.flash.max(0.7);
        }
    }

    /// Parameters of the black & white world pass at `now` (None: no world pass).
    pub fn world(&self, now: Instant, max_radius: f32, cfg: &Config) -> Option<World> {
        if !cfg.toggles.world_dim {
            return None;
        }
        let feather = 160.0;
        if let Some(on) = self.world_on_at {
            let t = now.saturating_duration_since(on).as_secs_f32() / cfg.ramp_in.max(0.05);
            let radius = ease_out(t) * (max_radius + feather);
            return Some(World {
                center: self.world_center,
                radius,
                feather,
                amount: 1.0,
                invert: false,
                contrast: 1.22,
                front: front_strength(radius, max_radius),
                glitch: if cfg.toggles.glitch {
                    self.glitch_level(now)
                } else {
                    0.0
                },
                ripple: self.ripple(now, cfg),
                look: Look::Sandevistan,
            });
        }
        if let Some(off) = self.world_off_at {
            let t = now.saturating_duration_since(off).as_secs_f32() / cfg.ramp_out.max(0.05);
            if t >= 1.0 {
                return None;
            }
            let radius = ease_out(t) * (max_radius + feather);
            return Some(World {
                center: self.world_center,
                radius,
                feather,
                amount: 1.0,
                invert: true,
                contrast: 1.22,
                front: front_strength(radius, max_radius) * 0.7,
                glitch: if cfg.toggles.glitch {
                    self.glitch_level(now)
                } else {
                    0.0
                },
                ripple: self.ripple(now, cfg),
                look: Look::Sandevistan,
            });
        }
        None
    }

    pub fn update(&mut self, now: Instant, tracker: &CursorTracker, cfg: &Config) {
        let dt = self
            .last_update
            .map(|t| now.saturating_duration_since(t).as_secs_f32())
            .unwrap_or(0.0)
            .min(0.1);
        self.last_update = Some(now);

        let speed = tracker.speed(now, std::time::Duration::from_millis(60));
        let target_glow = if self.active && cfg.toggles.glow {
            1.0
        } else {
            0.0
        };
        let k = 1.0 - (-dt * if target_glow > self.glow { 14.0 } else { 6.0 }).exp();
        self.glow += (target_glow - self.glow) * k;
        self.flash = (self.flash - dt * 2.2).max(0.0);
        if self
            .world_off_at
            .is_some_and(|o| now.saturating_duration_since(o).as_secs_f32() > cfg.ramp_out)
        {
            self.world_off_at = None;
        }

        if self.active {
            if cfg.toggles.ghosts {
                self.ghosts.spawn_from_motion(tracker, now, speed, cfg);
            }
            self.speed_burst(now, tracker, speed, cfg);
            if cfg.toggles.particles {
                if let (Some(pos), Some(dir)) = (
                    tracker.current(),
                    tracker.direction(now, std::time::Duration::from_millis(80), 3.0),
                ) {
                    self.particles
                        .emit_from_motion(pos, dir, speed, dt, cfg, &mut self.rng);
                }
            }
        } else {
            self.ghosts.stop_spawning();
        }
        self.ghosts.prune(now);
        self.particles.update(dt, cfg.max_particles);
        self.shocks.prune(now);
        self.dashes.prune(now);
        self.rays.prune(now);
    }

    /// Flicking the cursor very fast "breaks the sound barrier": a fan of
    /// light rays pointing backwards and a spray of sparks (no ring). Re-arms once you slow down.
    fn speed_burst(&mut self, now: Instant, tracker: &CursorTracker, speed: f32, cfg: &Config) {
        if speed < BOOM_SPEED * 0.6 {
            self.boom_armed = true;
            return;
        }
        let cooled = self
            .last_boom
            .is_none_or(|t| now.saturating_duration_since(t).as_secs_f32() > 0.45);
        if speed < BOOM_SPEED
            || !self.boom_armed
            || !cooled
            || cfg.performance_mode
            || !cfg.toggles.speed_bursts
        {
            return;
        }
        let (Some(pos), Some(dir)) = (
            tracker.current(),
            tracker.direction(now, std::time::Duration::from_millis(60), 4.0),
        ) else {
            return;
        };
        self.boom_armed = false;
        self.last_boom = Some(now);
        if cfg.toggles.light_rays {
            self.rays
                .spawn(pos, now, 10, 170.0, 0.32, -dir, cfg, &mut self.rng);
        }
        if cfg.toggles.particles {
            self.particles.burst(pos, 14, 420.0, cfg, &mut self.rng);
        }
    }

    /// True while something on screen still changes with time.
    pub fn is_animating(&self, now: Instant, tracker: &CursorTracker) -> bool {
        // The live black & white desktop needs continuous frames while it is shown.
        if self.world_on_at.is_some() || self.world_off_at.is_some() {
            return true;
        }
        if !self.ghosts.is_empty()
            || !self.particles.is_empty()
            || !self.shocks.is_empty()
            || !self.dashes.is_empty()
            || !self.rays.is_empty()
            || self.flash > 0.01
        {
            return true;
        }
        let glow_target = if self.active { 1.0 } else { 0.0 };
        if (self.glow - glow_target).abs() > 0.01 {
            return true;
        }
        let recent = tracker
            .time_since_move(now)
            .is_some_and(|d| d.as_secs_f32() < TRAIL_LIFETIME + 0.05);
        recent
            && (self.active
                || self.cutoff.is_some_and(|c| {
                    now.saturating_duration_since(c).as_secs_f32() < TRAIL_LIFETIME
                }))
    }

    /// Whether the glow disc would be drawn (needs a redraw if the cursor moved).
    pub fn has_glow(&self) -> bool {
        self.glow > 0.01
    }

    pub fn draw(
        &mut self,
        batch: &mut Batch,
        tracker: &CursorTracker,
        now: Instant,
        cfg: &Config,
        max_radius: f32,
    ) {
        let head = tracker.current();
        let speed = tracker.speed(now, std::time::Duration::from_millis(60));
        let speedf = (speed / 2500.0).clamp(0.0, 1.0);
        let cutoff = if self.active { None } else { self.cutoff };
        let st = &cfg.style;

        // World: real desaturation happens in the renderer; without capture support fall
        // back to a neutral dim so the colourful echoes still stand out.
        if let Some(w) = self.world(now, max_radius, cfg) {
            if !cfg.world_supported {
                let level = if w.invert {
                    1.0 - (w.radius / (max_radius + w.feather))
                } else {
                    w.radius / (max_radius + w.feather)
                };
                batch.fill_all([0.32, 0.32, 0.33], 0.55 * level.clamp(0.0, 1.0));
            }
            // Glowing chromatic front of the wave while it travels across the screen.
            let travel = w.radius / (max_radius + w.feather);
            // With the live desktop the front is an air ripple in the world pass; the neon
            // rings are only the fallback when the desktop can't be captured.
            if !cfg.world_supported && travel < 0.98 && w.radius > 4.0 {
                let a = (1.0 - travel).powf(0.8) * 0.9;
                let quad = w.radius + 30.0;
                let mid = w.radius / quad;
                let half = 10.0 / quad;
                batch.ring(
                    w.center,
                    quad,
                    mid,
                    half * 3.0,
                    palette_at(&st.palette, 0.0),
                    a * 0.30,
                    1.0,
                );
                batch.ring(
                    w.center,
                    quad,
                    mid,
                    half,
                    cfg.hot(palette_at(&st.palette, 1.0), 1.5),
                    a,
                    0.85,
                );
                // Prism split: red leads, blue trails the front.
                batch.ring(
                    w.center,
                    quad,
                    (w.radius + 7.0) / quad,
                    half * 0.45,
                    cfg.hot([1.0, 0.15, 0.35], 1.4),
                    a * 0.7,
                    1.0,
                );
                batch.ring(
                    w.center,
                    quad,
                    (w.radius - 9.0).max(1.0) / quad,
                    half * 0.5,
                    cfg.hot([0.2, 0.55, 1.0], 1.4),
                    a * 0.8,
                    1.0,
                );
            }
        }

        if cfg.toggles.trails && (self.active || cutoff.is_some()) {
            trails::draw(batch, tracker, now, cutoff, speedf, cfg);
        }
        if cfg.toggles.warp_dash {
            self.dashes.draw(batch, now, cfg);
        }
        if cfg.toggles.ghosts {
            self.ghosts.draw(batch, now, cfg);
        }
        if cfg.toggles.shockwave && !(cfg.world_supported && cfg.toggles.world_dim) {
            self.shocks.draw(batch, now, cfg);
        }
        if cfg.toggles.light_rays {
            self.rays.draw(batch, now, cfg);
        }
        if cfg.toggles.particles {
            self.particles.draw(batch, cfg);
        }
        if let Some(p) = head {
            let level = self.glow.max(self.flash * 0.6);
            if level > 0.01 {
                let base_r = 30.0 * cfg.scale * st.glow;
                let r = base_r * (1.0 + self.flash * 1.4);
                let a = (0.22 * self.glow + 0.55 * self.flash) * cfg.intensity.max(0.15);
                let c = palette_at(&st.palette, 0.0);
                let center = p + Vec2::new(6.0, 9.0) * cfg.scale;
                // Wide aura + tight hot core (the core blooms with HDR).
                batch.disc_sharp(center, r * 1.6, c, a * 0.55, 1.0, 3.0);
                batch.disc(center, r, c, a, 0.9);
                batch.disc_sharp(
                    center,
                    r * 0.5,
                    cfg.hot(mix(c, [1.0, 1.0, 1.0], 0.5), 1.8),
                    a * 0.6,
                    1.0,
                    6.0,
                );
            }
        }
    }
}

impl Default for Effects {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_in_range_and_varies() {
        let mut r = Rng::new(1);
        let v: Vec<f32> = (0..1000).map(|_| r.unit()).collect();
        assert!(v.iter().all(|x| (0.0..1.0).contains(x)));
        assert!(v.windows(2).any(|w| w[0] != w[1]));
    }

    #[test]
    fn batch_emits_ndc_coordinates() {
        let mut b = Batch::new(Vec2::new(-100.0, 0.0), Vec2::new(200.0, 100.0));
        b.disc(Vec2::new(0.0, 50.0), 10.0, [1.0; 3], 1.0, 0.0);
        assert_eq!(b.verts.len(), 6);
        let cx: f32 = b.verts.iter().map(|v| v.pos[0]).sum::<f32>() / 6.0;
        assert!(cx.abs() < 0.2);
        assert!(
            b.verts
                .iter()
                .all(|v| v.pos[0].abs() <= 1.0 && v.pos[1].abs() <= 1.0)
        );
    }

    #[test]
    fn mix_endpoints() {
        assert_eq!(mix([0.0; 3], [1.0; 3], 0.0), [0.0; 3]);
        assert_eq!(mix([0.0; 3], [1.0; 3], 1.0), [1.0; 3]);
    }

    #[test]
    fn palette_cycles() {
        let p = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        assert_eq!(palette_at(&p, 0.0), [1.0, 0.0, 0.0]);
        assert_eq!(palette_at(&p, 1.0), [0.0, 1.0, 0.0]);
        assert_eq!(palette_at(&p, 2.0), [1.0, 0.0, 0.0]);
        let mid = palette_at(&p, 0.5);
        assert!((mid[0] - 0.5).abs() < 1e-5);
    }

    #[test]
    fn world_wave_drains_then_floods_back() {
        let cfg = crate::app::effect_config(&crate::settings::config::Settings::default());
        let mut fx = Effects::new();
        let t0 = Instant::now();
        fx.activate(Vec2::ZERO, t0, &cfg);
        let w0 = fx.world(t0, 1000.0, &cfg).unwrap();
        let w1 = fx
            .world(t0 + std::time::Duration::from_secs(3), 1000.0, &cfg)
            .unwrap();
        assert!(w0.radius < 1.0 && !w0.invert);
        assert!(w1.radius >= 1000.0, "fully grey after the ramp");
        let t1 = t0 + std::time::Duration::from_secs(4);
        fx.deactivate(Vec2::ZERO, t1);
        assert!(fx.world(t1, 1000.0, &cfg).unwrap().invert);
        assert!(
            fx.world(t1 + std::time::Duration::from_secs(2), 1000.0, &cfg)
                .is_none()
        );
    }
}
