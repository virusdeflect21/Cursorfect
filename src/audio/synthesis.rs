//! Original, procedurally generated sound effects (no external or copyrighted audio).

use std::f32::consts::TAU;

use crate::settings::presets::SfxProfile;

pub const SAMPLE_RATE: u32 = 44_100;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sfx {
    Activation,
    Deactivation,
    WarpDash,
    SpeedBurst,
    Overheat,
    Cooldown,
}

impl Sfx {
    pub const ALL: [Sfx; 6] = [
        Sfx::Activation,
        Sfx::Deactivation,
        Sfx::WarpDash,
        Sfx::SpeedBurst,
        Sfx::Overheat,
        Sfx::Cooldown,
    ];

    /// Minimum time between two plays of the same effect.
    pub fn min_interval_ms(self) -> u64 {
        match self {
            Sfx::SpeedBurst => 400,
            Sfx::WarpDash => 120,
            _ => 80,
        }
    }

    /// File stem looked up for user-supplied replacements (`activation.mp3`, ...).
    pub fn file_stem(self) -> &'static str {
        match self {
            Sfx::Activation => "activation",
            Sfx::Deactivation => "deactivation",
            Sfx::WarpDash => "dash",
            Sfx::SpeedBurst => "burst",
            Sfx::Overheat => "overheat",
            Sfx::Cooldown => "cooldown",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }
}

struct Params {
    pitch: f32,
    drive: f32,
    length: f32,
}

fn params(profile: SfxProfile) -> Params {
    match profile {
        SfxProfile::David => Params {
            pitch: 1.0,
            drive: 1.2,
            length: 1.0,
        },
        SfxProfile::Smasher => Params {
            pitch: 0.68,
            drive: 3.0,
            length: 1.25,
        },
        SfxProfile::Ghost => Params {
            pitch: 1.3,
            drive: 0.7,
            length: 0.85,
        },
    }
}

struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

fn n_samples(secs: f32) -> usize {
    (secs * SAMPLE_RATE as f32) as usize
}

/// Slightly buzzy oscillator (fundamental + 2nd/3rd harmonics) for a "cybernetic" timbre.
fn buzz(phase: f32) -> f32 {
    (phase * TAU).sin() * 0.7 + (phase * 2.0 * TAU).sin() * 0.35 + (phase * 3.0 * TAU).sin() * 0.2
}

fn finish(mut buf: Vec<f32>, drive: f32, peak: f32) -> Vec<f32> {
    for s in &mut buf {
        *s = (*s * drive).tanh();
    }
    let max = buf.iter().fold(0.0_f32, |m, s| m.max(s.abs())).max(1e-6);
    let g = peak / max;
    let len = buf.len();
    for (i, s) in buf.iter_mut().enumerate() {
        // Short fade-out to guarantee no click at the end.
        let tail = (len - i) as f32 / n_samples(0.004) as f32;
        *s *= g * tail.min(1.0);
    }
    buf
}

pub fn synth(sfx: Sfx, profile: SfxProfile) -> Vec<f32> {
    let p = params(profile);
    match sfx {
        Sfx::Activation => activation(&p),
        Sfx::Deactivation => deactivation(&p),
        Sfx::WarpDash => warp_dash(&p),
        Sfx::SpeedBurst => speed_burst(&p),
        Sfx::Overheat => overheat(&p),
        Sfx::Cooldown => cooldown(&p),
    }
}

/// Short electronic charge, rising pitch, sharp cybernetic pulse.
fn activation(p: &Params) -> Vec<f32> {
    let charge = 0.38 * p.length;
    let total = charge + 0.28;
    let n = n_samples(total);
    let mut rng = Rng(0x1234_5678);
    let mut phase = 0.0_f32;
    let mut out = vec![0.0; n];
    let (f0, f1) = (150.0 * p.pitch, 1500.0 * p.pitch);
    for (i, o) in out.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let mut s = 0.0;
        if t < charge {
            let k = t / charge;
            let freq = f0 * (f1 / f0).powf(k);
            phase += freq / SAMPLE_RATE as f32;
            let env = (t / 0.01).min(1.0) * (0.25 + 0.75 * k.powf(1.4));
            s += buzz(phase) * env * 0.6;
            // Electrical flutter grows with the charge.
            s += rng.next() * 0.06 * k;
        } else {
            let u = t - charge;
            // Sharp pulse: bright ping + noise snap + sub thump.
            s += (u * 2000.0 * p.pitch * TAU).sin() * (-u * 26.0).exp() * 0.8;
            s += (u * 3100.0 * p.pitch * TAU).sin() * (-u * 40.0).exp() * 0.4;
            s += rng.next() * (-u * 90.0).exp() * 0.6;
            s += (u * 70.0 * p.pitch * TAU).sin() * (-u * 14.0).exp() * 0.7;
        }
        *o = s;
    }
    finish(out, p.drive, 0.8)
}

/// Descending synthesized tone with a soft electrical decay.
fn deactivation(p: &Params) -> Vec<f32> {
    let total = 0.55 * p.length;
    let n = n_samples(total);
    let mut rng = Rng(0x9E37_79B9);
    let mut phase = 0.0_f32;
    let mut out = vec![0.0; n];
    for (i, o) in out.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let k = t / total;
        let freq = 950.0 * p.pitch * (0.09_f32).powf(k);
        phase += freq / SAMPLE_RATE as f32;
        let env = (t / 0.006).min(1.0) * (-t * 5.5).exp();
        let tremolo = 0.75 + 0.25 * (t * 38.0 * TAU).sin();
        let crackle = if rng.next() > 0.985 {
            rng.next() * 0.5
        } else {
            0.0
        };
        *o = buzz(phase) * env * tremolo * 0.7 + crackle * (1.0 - k);
    }
    finish(out, p.drive, 0.7)
}

/// Fast synthetic whoosh with a sharp electric snap.
fn warp_dash(p: &Params) -> Vec<f32> {
    let whoosh = 0.22 * p.length;
    let total = whoosh + 0.16;
    let n = n_samples(total);
    let mut rng = Rng(0xA5A5_1234);
    let mut lp = 0.0_f32;
    let mut lp2 = 0.0_f32;
    let mut out = vec![0.0; n];
    for (i, o) in out.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let mut s = 0.0;
        if t < whoosh {
            let k = t / whoosh;
            // Noise through a low-pass whose cutoff sweeps upward = rising "whoosh".
            let cutoff = (300.0 + 6500.0 * k * k) * p.pitch;
            let a = 1.0 - (-TAU * cutoff / SAMPLE_RATE as f32).exp();
            lp += a * (rng.next() - lp);
            lp2 += a * (lp - lp2);
            let env = (t / 0.015).min(1.0) * (0.3 + 0.7 * k);
            s += lp2 * env * 3.0;
        } else {
            let u = t - whoosh;
            s += rng.next() * (-u * 120.0).exp() * 0.9;
            s += (u * 2600.0 * p.pitch * TAU).sin() * (-u * 45.0).exp() * 0.9;
            s += (u * 1300.0 * p.pitch * TAU).sin() * (-u * 28.0).exp() * 0.5;
        }
        *o = s;
    }
    finish(out, p.drive, 0.8)
}

/// Quiet, high-frequency electrical shimmer.
fn speed_burst(p: &Params) -> Vec<f32> {
    let total = 0.22;
    let n = n_samples(total);
    let mut rng = Rng(0x0BAD_CAFE);
    let mut out = vec![0.0; n];
    for (i, o) in out.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let env = (t / 0.02).min(1.0) * (1.0 - t / total).max(0.0).powf(1.5);
        let mut s = 0.0;
        for (j, f) in [5200.0_f32, 6900.0, 8300.0].iter().enumerate() {
            let am = 0.5 + 0.5 * (t * (45.0 + 17.0 * j as f32) * TAU).sin();
            s += (t * f * p.pitch * TAU).sin() * am * 0.3;
        }
        s += rng.next() * 0.05;
        *o = s * env;
    }
    finish(out, 1.0, 0.25)
}

/// Distorted warning beeps.
fn overheat(p: &Params) -> Vec<f32> {
    let beep = 0.14;
    let gap = 0.07;
    let total = 3.0 * beep + 2.0 * gap + 0.05;
    let n = n_samples(total);
    let mut out = vec![0.0; n];
    for (i, o) in out.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let slot = t / (beep + gap);
        let idx = slot.floor();
        let local = (slot - idx) * (beep + gap);
        if idx < 3.0 && local < beep {
            let f = (if idx as i32 % 2 == 0 { 420.0 } else { 340.0 }) * p.pitch;
            let sq = if (t * f * TAU).sin() >= 0.0 {
                1.0
            } else {
                -1.0
            };
            let env = (local / 0.004).min(1.0) * (1.0 - local / beep).max(0.0).powf(0.4);
            *o = (sq * 0.7 + (t * f * 0.5 * TAU).sin() * 0.3) * env;
        }
    }
    finish(out, 2.2 + p.drive, 0.7)
}

/// Subtle electronic confirmation (two rising pings).
fn cooldown(p: &Params) -> Vec<f32> {
    let total = 0.32;
    let n = n_samples(total);
    let mut out = vec![0.0; n];
    for (i, o) in out.iter_mut().enumerate() {
        let t = i as f32 / SAMPLE_RATE as f32;
        let a = (t * 660.0 * p.pitch * TAU).sin() * (-t * 16.0).exp();
        let u = (t - 0.09).max(0.0);
        let b = if t >= 0.09 {
            (u * 990.0 * p.pitch * TAU).sin() * (-u * 14.0).exp()
        } else {
            0.0
        };
        *o = (a + b) * 0.6 * (t / 0.004).min(1.0);
    }
    finish(out, 1.0, 0.45)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sound_is_finite_bounded_and_nonempty() {
        for profile in [SfxProfile::David, SfxProfile::Smasher, SfxProfile::Ghost] {
            for sfx in Sfx::ALL {
                let buf = synth(sfx, profile);
                assert!(buf.len() > 1000, "{sfx:?} too short");
                assert!(buf.len() < SAMPLE_RATE as usize * 2, "{sfx:?} too long");
                assert!(
                    buf.iter().all(|s| s.is_finite() && s.abs() <= 1.0),
                    "{sfx:?}/{profile:?}"
                );
                assert!(buf.iter().any(|s| s.abs() > 0.05), "{sfx:?} is silent");
                assert!(
                    buf.last().copied().unwrap_or(1.0).abs() < 0.01,
                    "{sfx:?} ends with a click"
                );
            }
        }
    }

    #[test]
    fn rate_limit_intervals_are_sane() {
        assert!(Sfx::SpeedBurst.min_interval_ms() > Sfx::Activation.min_interval_ms());
    }
}
