//! Audio thread: preloaded SFX buffers played through rodio.
//!
//! Playback is requested through a channel, so input hooks and the render loop never touch
//! the audio device. If no output device exists the engine keeps running silently and
//! retries periodically.
//!
//! # The Sandevistan sound, cut into parts
//! A full "start to finish" sound (`sandevistan.mp3`, or any file with "sandevistan" in its
//! name) is decoded once and cut into four parts (see [`CutPlan`]):
//!
//! | part    | played when                                    |
//! |---------|------------------------------------------------|
//! | `start` | the effect turns on (opening hit + boom)       |
//! | `move`  | seamless loop whose volume follows cursor speed |
//! | `dash`  | every warp dash (alternates between variants)  |
//! | `end`   | the effect turns off / overheats (power-down)  |
//!
//! Any part can be replaced with your own file: `activation.*` (start), `move.*`, `dash.*`,
//! `deactivation.*` (end). The cut points can be overridden with a `cuts.json` next to the
//! sound (seconds): `{"start":[0.08,3.27],"move":[5.45,9.35],"dash":[[3.27,4.25]],"end":[13.72,15.84]}`.
//! In [`SoundMode::FullTrack`] the whole file plays once and the effect lasts exactly as long.

use std::collections::{HashMap, HashSet};
use std::num::NonZero;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rodio::Source;
use rodio::buffer::SamplesBuffer;
use rodio::stream::{DeviceSinkBuilder, MixerDeviceSink};
use serde::Deserialize;

use super::dubs::{self, Clip};
use super::mih::{self, MihSound};
use super::synthesis::{SAMPLE_RATE, Sfx, synth};
use crate::settings::config::SoundMode;
use crate::settings::presets::SfxProfile;

const FADE_START: Duration = Duration::from_millis(260);
const FADE_MOVE: Duration = Duration::from_millis(420);
/// Audio thread tick while something is fading or the movement layer is live.
const TICK: Duration = Duration::from_millis(12);

/// Parts that can be previewed from the settings window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Start,
    Move,
    Dash,
    End,
    Full,
}

#[derive(Debug)]
pub enum AudioCmd {
    /// One-shot effect sound (overheat, cooldown, ...).
    Play(Sfx),
    /// Preview a part regardless of the "sound enabled" switch.
    Preview(Part),
    /// The effect turned on.
    Start,
    /// The effect turned off. `natural`: the full track just ran out (play nothing more).
    End {
        natural: bool,
    },
    /// Energy ran out: stop everything and play the overheat / power-down sound.
    Overheat,
    Dash,
    /// Cursor movement level, 0 (still) ..= 1 (fast).
    Motion(f32),
    SetEnabled(bool),
    /// 0..=100
    SetVolume(u8),
    /// 0..=100
    SetMoveVolume(u8),
    SetMode(SoundMode),
    /// Sandevistan parts that may play: start, movement, dash, end.
    SetSvParts([bool; 4]),
    /// The World: play these clips, each at its offset (seconds from now).
    TwPlay(Vec<(f32, Clip)>),
    /// The World: one clock tick.
    TwTick,
    /// The World: preview clips from the settings window (plays even with sound off).
    TwPreview(Vec<(f32, Clip)>),
    /// The World: fade out every time-stop sound still playing.
    TwStopAll,
    /// MIH one-shots, decoded on this audio thread, never in the input hook.
    MihPlay(MihSound),
    MihPreview(MihSound),
    /// Normal completion may let the reset one-shot finish; cancellation never does.
    MihFinish,
    /// `immediate`: emergency / exit; otherwise use the existing fade engine.
    MihStopAll {
        immediate: bool,
    },
    SetMihEnabled(bool),
    SetMihVolume(u8),
    SetProfile(SfxProfile),
    /// Re-scan the sound folders (picks up newly added files).
    Reload,
    Shutdown,
}

/// What the engine needs to know about the loaded sounds.
#[derive(Clone, Debug, Default)]
pub struct AudioInfo {
    /// Initial decoding/reload completes asynchronously; don't report missing files early.
    pub loaded: bool,
    /// Length of the full-sequence sound, if one is loaded.
    pub sequence_len: Option<Duration>,
    /// File names of user sounds in use.
    pub custom: Vec<String>,
    /// Human-readable description of the parts (for the settings window).
    pub parts: String,
    pub mih_parts: String,
    pub mih_loaded: [bool; 5],
}

pub struct AudioHandle {
    tx: Sender<AudioCmd>,
    join: Option<JoinHandle<()>>,
    info: Arc<Mutex<AudioInfo>>,
}

#[derive(Clone, Copy, Debug)]
pub struct AudioConfig {
    pub enabled: bool,
    pub volume: u8,
    pub move_volume: u8,
    pub mode: SoundMode,
    pub profile: SfxProfile,
    /// Sandevistan: start, movement, dash, end.
    pub sv_parts: [bool; 4],
    pub mih_enabled: bool,
    pub mih_volume: u8,
}

/// Folder with The World's sounds, searched like the other sound folders.
pub fn theworld_file(name: &str) -> Option<PathBuf> {
    sound_dirs()
        .into_iter()
        .map(|d| d.join("theworld").join(name))
        .find(|p| p.is_file())
}

/// Bring a clip to a common loudness (the supplied files differ by ~10x) without clipping.
fn normalize_clip(p: Pcm) -> Pcm {
    let rms = p.rms();
    let peak = p.samples.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    if rms < 1e-5 || peak < 1e-5 {
        return p;
    }
    let g = (0.16 / rms).min(0.92 / peak).clamp(0.3, 8.0);
    p.gain(g)
}

impl AudioHandle {
    pub fn spawn(cfg: AudioConfig) -> Self {
        let (tx, rx) = mpsc::channel();
        let info = Arc::new(Mutex::new(AudioInfo::default()));
        let shared = info.clone();
        let join = std::thread::Builder::new()
            .name("cursorfect-audio".into())
            .spawn(move || run(rx, cfg, shared))
            .map_err(|e| tracing::error!("cannot spawn audio thread: {e}"))
            .ok();
        if join.is_none()
            && let Ok(mut state) = info.lock()
        {
            state.loaded = true;
            state.mih_parts = "Audio thread unavailable; MIH will run silently".into();
        }
        Self { tx, join, info }
    }

    pub fn send(&self, cmd: AudioCmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn play(&self, sfx: Sfx) {
        self.send(AudioCmd::Play(sfx));
    }

    pub fn info(&self) -> AudioInfo {
        self.info.lock().map(|i| i.clone()).unwrap_or_default()
    }

    pub fn shutdown(&mut self) {
        let _ = self.tx.send(AudioCmd::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for AudioHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Folders searched (in order) for user sound files such as `activation.mp3`.
pub fn sound_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(cfg) =
        crate::settings::config::settings_path().and_then(|p| p.parent().map(|d| d.join("sounds")))
    {
        v.push(cfg);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("sounds"));
            v.push(dir.join("assets").join("sounds"));
            // cargo layout: target/release/../../assets/sounds
            if let Some(root) = dir.parent().and_then(|p| p.parent()) {
                v.push(root.join("assets").join("sounds"));
            }
        }
    }
    v
}

// ------------------------------------------------------------------------------------ PCM

/// Decoded audio kept as raw interleaved samples so it can be cut into parts.
#[derive(Clone)]
pub struct Pcm {
    pub samples: Vec<f32>,
    pub channels: NonZero<u16>,
    pub rate: NonZero<u32>,
}

impl Pcm {
    pub fn mono(samples: Vec<f32>) -> Self {
        Self {
            samples,
            channels: NonZero::<u16>::MIN,
            rate: NonZero::new(SAMPLE_RATE).unwrap_or(NonZero::<u32>::MIN),
        }
    }

    fn ch(&self) -> usize {
        usize::from(self.channels.get())
    }

    pub fn frames(&self) -> usize {
        self.samples.len() / self.ch()
    }

    pub fn secs(&self) -> f32 {
        self.frames() as f32 / self.rate.get() as f32
    }

    pub fn buffer(&self) -> SamplesBuffer {
        SamplesBuffer::new(self.channels, self.rate, self.samples.clone())
    }

    fn frame_at(&self, secs: f32) -> usize {
        ((secs.max(0.0) * self.rate.get() as f32) as usize).min(self.frames())
    }

    /// Cut `[from, to)` seconds with fades (seconds) so joins never click.
    pub fn cut(&self, from: f32, to: f32, fade_in: f32, fade_out: f32) -> Pcm {
        let ch = self.ch();
        let a = self.frame_at(from);
        let b = self.frame_at(to).max(a);
        let mut out = self.samples[a * ch..b * ch].to_vec();
        let frames = b - a;
        let rate = self.rate.get() as f32;
        let fi = ((fade_in * rate) as usize).min(frames / 2);
        let fo = ((fade_out * rate) as usize).min(frames / 2);
        for f in 0..frames {
            let mut g = 1.0;
            if f < fi {
                // Equal-power curve: transients stay crisp, no audible dip.
                g *= (f as f32 / fi as f32 * std::f32::consts::FRAC_PI_2).sin();
            }
            let left = frames - f;
            if left <= fo {
                g *= (left as f32 / fo as f32 * std::f32::consts::FRAC_PI_2).sin();
            }
            for c in 0..ch {
                out[f * ch + c] *= g;
            }
        }
        Pcm {
            samples: out,
            ..self.clone()
        }
    }

    /// Turn `[from, to)` into a seamless loop: the last `xfade` seconds are cross-faded
    /// (equal power) into the beginning, so wrapping around is inaudible.
    pub fn seamless_loop(&self, from: f32, to: f32, xfade: f32) -> Pcm {
        let seg = self.cut(from, to, 0.0, 0.0);
        let ch = seg.ch();
        let frames = seg.frames();
        let x = ((xfade * seg.rate.get() as f32) as usize).min(frames / 3);
        if x == 0 {
            return seg;
        }
        let len = frames - x;
        let mut out = seg.samples[..len * ch].to_vec();
        for f in 0..x {
            let t = f as f32 / x as f32;
            let gin = (t * std::f32::consts::FRAC_PI_2).sin();
            let gout = (t * std::f32::consts::FRAC_PI_2).cos();
            for c in 0..ch {
                let head = seg.samples[f * ch + c];
                let tail = seg.samples[(len + f) * ch + c];
                out[f * ch + c] = head * gin + tail * gout;
            }
        }
        Pcm {
            samples: out,
            ..seg
        }
    }

    pub fn rms(&self) -> f32 {
        if self.samples.is_empty() {
            return 0.0;
        }
        (self.samples.iter().map(|s| s * s).sum::<f32>() / self.samples.len() as f32).sqrt()
    }

    /// Even out the loudness (slow automatic gain) so a loop has no swells that pulse every
    /// time it wraps around. `window` seconds of smoothing; gain limited to 0.5..2.5.
    pub fn flatten(&self, window: f32) -> Pcm {
        let ch = self.ch();
        let frames = self.frames();
        let win = ((window * self.rate.get() as f32) as usize).max(1);
        let target = self.rms().max(1e-4);
        // Per-block RMS, then a smoothed gain curve interpolated per frame.
        let blocks: Vec<f32> = (0..frames.div_ceil(win))
            .map(|b| {
                let a = b * win * ch;
                let e = ((b + 1) * win * ch).min(self.samples.len());
                let s = &self.samples[a..e];
                (s.iter().map(|x| x * x).sum::<f32>() / s.len().max(1) as f32).sqrt()
            })
            .collect();
        let gains: Vec<f32> = (0..blocks.len())
            .map(|i| {
                let lo = i.saturating_sub(2);
                let hi = (i + 3).min(blocks.len());
                let avg = blocks[lo..hi].iter().sum::<f32>() / (hi - lo) as f32;
                (target / avg.max(1e-4)).clamp(0.5, 2.5)
            })
            .collect();
        let mut out = self.samples.clone();
        for f in 0..frames {
            let pos = f as f32 / win as f32 - 0.5;
            let i = (pos.floor().max(0.0) as usize).min(gains.len() - 1);
            let j = (i + 1).min(gains.len() - 1);
            let t = (pos - i as f32).clamp(0.0, 1.0);
            let g = gains[i] + (gains[j] - gains[i]) * t;
            for c in 0..ch {
                out[f * ch + c] *= g;
            }
        }
        Pcm {
            samples: out,
            ..self.clone()
        }
    }

    pub fn gain(mut self, g: f32) -> Pcm {
        for s in &mut self.samples {
            *s *= g;
        }
        self
    }

    /// `secs` of this (looping) clip with soft fade in/out, used for previews.
    pub fn looped_preview(&self, secs: f32) -> Pcm {
        let ch = self.ch();
        let frames = self.frames().max(1);
        let n = (secs * self.rate.get() as f32) as usize;
        let mut samples = Vec::with_capacity(n * ch);
        for f in 0..n {
            let src = f % frames;
            samples.extend_from_slice(&self.samples[src * ch..src * ch + ch]);
        }
        Pcm {
            samples,
            ..self.clone()
        }
        .cut(0.0, secs, 0.35, 0.6)
    }
}

// ------------------------------------------------------------------------------------ cuts

/// Where each part sits in the full sound, in seconds.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CutPlan {
    pub start: [f32; 2],
    #[serde(rename = "move")]
    pub move_: [f32; 2],
    pub dash: Vec<[f32; 2]>,
    pub end: [f32; 2],
}

impl CutPlan {
    /// Cut points measured on the "Sandevistan Sound Re-Design" (15.84 s), stored as fractions
    /// so other sounds with the same structure get proportional cuts:
    /// * 0.08-3.27 s  opening hit + low boom, ends in a near-silent gap
    /// * 3.27-4.25 s  the sharpest crack in the file + falling sweep -> dash A
    /// * 5.45-9.35 s  steady low "slowed time" drone -> movement loop
    /// * 9.37-10.05 s bright whoosh -> dash B
    /// * 13.72-end    final burst and power-down -> end
    pub fn for_length(len: f32) -> Self {
        const REF: f32 = 15.84;
        let k = len / REF;
        let m = |a: f32, b: f32| [a * k, (b * k).min(len)];
        Self {
            start: m(0.08, 3.27),
            move_: m(5.45, 9.35),
            dash: vec![m(3.27, 4.25), m(9.37, 10.05)],
            end: [13.72 * k, len],
        }
    }

    fn sane(&self, len: f32) -> bool {
        let ok = |r: &[f32; 2]| r[0] >= 0.0 && r[1] > r[0] + 0.05 && r[0] < len;
        ok(&self.start) && ok(&self.move_) && ok(&self.end) && self.dash.iter().all(ok)
    }
}

fn load_cut_plan(sound: &Path, len: f32) -> CutPlan {
    let mut candidates = Vec::new();
    if let (Some(dir), Some(stem)) = (sound.parent(), sound.file_stem()) {
        candidates.push(dir.join(format!("{}.cuts.json", stem.to_string_lossy())));
        candidates.push(dir.join("cuts.json"));
    }
    for c in candidates {
        let Ok(text) = std::fs::read_to_string(&c) else {
            continue;
        };
        match serde_json::from_str::<CutPlan>(&text) {
            Ok(plan) if plan.sane(len) => {
                tracing::info!("using cut points from {}", c.display());
                return plan;
            }
            Ok(_) => tracing::warn!("{} has out-of-range cut points; ignored", c.display()),
            Err(e) => tracing::warn!("{} is not valid: {e}", c.display()),
        }
    }
    CutPlan::for_length(len)
}

/// The full sound cut into its parts.
pub struct Parts {
    pub full: Pcm,
    pub start: Pcm,
    pub move_loop: Pcm,
    pub dashes: Vec<Pcm>,
    pub end: Pcm,
}

/// Target loudness of the movement loop before the user's movement volume is applied.
const MOVE_RMS: f32 = 0.14;
/// Sound files are usually mastered to full scale: parts that overlap (start + movement +
/// dash) would add up past 1.0 and clip into crackle, so every part gets this headroom.
const PART_HEADROOM: f32 = 0.6;

impl Parts {
    pub fn cut(p: &Pcm, plan: &CutPlan) -> Self {
        // Flatten first (no pulsing swell), then make the wrap-around seamless.
        let move_loop = p.cut(plan.move_[0], plan.move_[1], 0.0, 0.0).flatten(0.12);
        let len = move_loop.secs();
        let move_loop = move_loop.seamless_loop(0.0, len, 0.45);
        let rms = move_loop.rms();
        let move_loop = if rms > 1e-4 {
            move_loop.gain((MOVE_RMS / rms).clamp(0.2, 4.0))
        } else {
            move_loop
        };
        Self {
            full: p.clone().gain(PART_HEADROOM),
            start: p
                .cut(plan.start[0], plan.start[1], 0.0, 0.08)
                .gain(PART_HEADROOM),
            move_loop,
            dashes: plan
                .dash
                .iter()
                .map(|d| {
                    p.cut(d[0], d[1], 0.003, ((d[1] - d[0]) * 0.32).min(0.3))
                        .gain(PART_HEADROOM)
                })
                .collect(),
            end: p
                .cut(plan.end[0], plan.end[1], 0.03, 0.15)
                .gain(PART_HEADROOM),
        }
    }
}

// ------------------------------------------------------------------------------------ loading

fn decode_pcm(path: &Path) -> Option<Pcm> {
    let decoded = std::fs::File::open(path)
        .map_err(|e| e.to_string())
        .and_then(|f| rodio::Decoder::try_from(f).map_err(|e| e.to_string()));
    match decoded {
        Ok(dec) => {
            let (channels, rate) = (dec.channels(), dec.sample_rate());
            let samples: Vec<f32> = dec.collect();
            if samples.is_empty() {
                return None;
            }
            tracing::info!("loaded sound {}", path.display());
            Some(Pcm {
                samples,
                channels,
                rate,
            })
        }
        Err(e) => {
            tracing::warn!("cannot decode {}: {e}", path.display());
            None
        }
    }
}

/// Any audio file whose name contains "sandevistan" (e.g. a downloaded
/// "Sandevistan Sound Re-Design.mp3"), searched in the sound folders and next to the exe.
fn find_sequence_file() -> Option<PathBuf> {
    let mut dirs = sound_dirs();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(root) = exe
            .parent()
            .and_then(|d| d.parent())
            .and_then(|d| d.parent())
        {
            dirs.push(root.to_path_buf());
        }
        if let Some(dir) = exe.parent() {
            dirs.push(dir.to_path_buf());
        }
    }
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut found: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                let name = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                name.contains("sandevistan") && (name.ends_with(".mp3") || name.ends_with(".wav"))
            })
            .collect();
        // Prefer the plain `sandevistan.mp3`.
        found.sort_by_key(|p| (p.file_stem().map(|s| s != "sandevistan"), p.clone()));
        if let Some(p) = found.into_iter().next() {
            return Some(p);
        }
    }
    None
}

fn find_named(stem: &str) -> Option<PathBuf> {
    for dir in sound_dirs() {
        for ext in ["mp3", "wav", "ogg", "flac"] {
            let path = dir.join(format!("{stem}.{ext}"));
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?")
        .to_string()
}

struct Sounds {
    /// Built-in synthesized sounds.
    synth: HashMap<Sfx, SamplesBuffer>,
    /// The user's own single sounds (activation.mp3, ...).
    custom: HashMap<Sfx, SamplesBuffer>,
    custom_move: Option<Pcm>,
    parts: Option<Parts>,
    /// Fallback movement loop when no sound provides one.
    synth_move: Pcm,
    info: AudioInfo,
}

impl Sounds {
    fn load(profile: SfxProfile) -> Self {
        let rate = NonZero::new(SAMPLE_RATE).unwrap_or(NonZero::<u32>::MIN);
        let ch = NonZero::<u16>::MIN;
        let mut info = AudioInfo::default();
        let mut synth_map = HashMap::new();
        let mut custom = HashMap::new();
        for s in Sfx::ALL {
            synth_map.insert(s, SamplesBuffer::new(ch, rate, synth(s, profile)));
            if let Some(path) = find_named(s.file_stem()) {
                if let Some(p) = decode_pcm(&path) {
                    info.custom.push(file_name(&path));
                    custom.insert(s, p.gain(PART_HEADROOM).buffer());
                }
            }
        }
        let custom_move = find_named("move").and_then(|path| {
            let p = decode_pcm(&path)?;
            info.custom.push(file_name(&path));
            // Loop the whole file seamlessly.
            let len = p.secs();
            Some(p.seamless_loop(0.0, len, (len * 0.12).min(0.4)))
        });
        let seq_path = find_named("sandevistan").or_else(find_sequence_file);
        let parts = seq_path.and_then(|path| {
            let p = decode_pcm(&path)?;
            info.custom.push(file_name(&path));
            let len = p.secs();
            let plan = load_cut_plan(&path, len);
            tracing::info!("full-sequence sound {:.2}s, cuts {:?}", len, plan);
            info.sequence_len = Some(Duration::from_secs_f32(len));
            Some(Parts::cut(&p, &plan))
        });
        let mut s = Self {
            synth: synth_map,
            custom,
            custom_move,
            parts,
            synth_move: synth_move_loop(profile),
            info,
        };
        s.info.parts = s.describe();
        s
    }

    fn describe(&self) -> String {
        let src = |sfx: Sfx, part: Option<&Pcm>| -> String {
            if self.custom.contains_key(&sfx) {
                format!("{}.*", sfx.file_stem())
            } else if let Some(p) = part {
                format!("{:.1}s cut", p.secs())
            } else {
                "built-in".into()
            }
        };
        let p = self.parts.as_ref();
        let mv = if self.custom_move.is_some() {
            "move.*".to_string()
        } else if let Some(p) = p {
            format!("{:.1}s loop", p.move_loop.secs())
        } else {
            "built-in".into()
        };
        let dash = if self.custom.contains_key(&Sfx::WarpDash) {
            "dash.*".to_string()
        } else if let Some(p) = p {
            format!("{} variants", p.dashes.len())
        } else {
            "built-in".into()
        };
        format!(
            "Start: {} \u{b7} Move: {} \u{b7} Dash: {} \u{b7} End: {}",
            src(Sfx::Activation, p.map(|p| &p.start)),
            mv,
            dash,
            src(Sfx::Deactivation, p.map(|p| &p.end)),
        )
    }

    fn start(&self) -> SamplesBuffer {
        self.pick(Sfx::Activation, self.parts.as_ref().map(|p| &p.start))
    }

    fn end(&self) -> SamplesBuffer {
        self.pick(Sfx::Deactivation, self.parts.as_ref().map(|p| &p.end))
    }

    fn dash(&self, n: usize) -> SamplesBuffer {
        let part = self
            .parts
            .as_ref()
            .filter(|p| !p.dashes.is_empty())
            .map(|p| &p.dashes[n % p.dashes.len()]);
        self.pick(Sfx::WarpDash, part)
    }

    fn move_loop(&self) -> &Pcm {
        self.custom_move
            .as_ref()
            .or(self.parts.as_ref().map(|p| &p.move_loop))
            .unwrap_or(&self.synth_move)
    }

    /// The user's file, else the cut part, else the synthesized sound.
    fn pick(&self, sfx: Sfx, part: Option<&Pcm>) -> SamplesBuffer {
        if let Some(b) = self.custom.get(&sfx) {
            return b.clone();
        }
        if let Some(p) = part {
            return p.buffer();
        }
        self.single(sfx)
    }

    fn single(&self, sfx: Sfx) -> SamplesBuffer {
        self.custom
            .get(&sfx)
            .or_else(|| self.synth.get(&sfx))
            .cloned()
            .unwrap_or_else(|| {
                SamplesBuffer::new(NonZero::<u16>::MIN, NonZero::<u32>::MIN, vec![0.0])
            })
    }

    /// With the user's Sandevistan sound loaded, synthesized one-shots would clash with it:
    /// only the user's own files play then.
    fn one_shot(&self, sfx: Sfx) -> Option<SamplesBuffer> {
        if let Some(b) = self.custom.get(&sfx) {
            return Some(b.clone());
        }
        if self.parts.is_some() {
            return None;
        }
        self.synth.get(&sfx).cloned()
    }
}

/// Procedural movement layer used when no sound file provides one: filtered noise "air"
/// over a detuned low hum, 2 s, loops seamlessly.
fn synth_move_loop(profile: SfxProfile) -> Pcm {
    let pitch = match profile {
        SfxProfile::David => 1.0,
        SfxProfile::Smasher => 0.7,
        SfxProfile::Ghost => 1.35,
    };
    let sr = SAMPLE_RATE as f32;
    let n = (2.4 * sr) as usize;
    let mut out = Vec::with_capacity(n);
    let mut seed = 0x2545_F491u32;
    let (mut lp, mut bp) = (0.0f32, 0.0f32);
    for i in 0..n {
        let t = i as f32 / sr;
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let noise = (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
        lp += (noise - lp) * 0.08;
        bp += (lp - bp) * 0.3;
        let air = (lp - bp) * 2.2;
        let tau = std::f32::consts::TAU;
        let hum = (t * 55.0 * pitch * tau).sin() * 0.5
            + (t * 55.4 * pitch * tau).sin() * 0.35
            + (t * 110.0 * pitch * tau).sin() * 0.12;
        let wob = 0.8 + 0.2 * (t * tau / 1.2).sin();
        out.push((air * 0.6 + hum * 0.35) * wob);
    }
    let p = Pcm::mono(out).seamless_loop(0.0, 2.4, 0.4);
    let rms = p.rms().max(1e-4);
    p.gain(MOVE_RMS / rms)
}

// ------------------------------------------------------------------------------------ thread

fn open_sink() -> Option<MixerDeviceSink> {
    match DeviceSinkBuilder::open_default_sink() {
        Ok(mut sink) => {
            sink.log_on_drop(false);
            Some(sink)
        }
        Err(e) => {
            tracing::warn!("audio output unavailable: {e}");
            None
        }
    }
}

fn gain(volume: u8) -> f32 {
    // Perceptual volume curve so the slider feels linear.
    (f32::from(volume) / 100.0).powf(1.6)
}

struct Fade {
    player: rodio::Player,
    from: f32,
    start: Instant,
    dur: Duration,
    /// Allows a fresh MIH activation to stop its previous fading copy, not overlap it.
    mih_owner: Option<MihSound>,
}

struct MoveLayer {
    player: rodio::Player,
    level: f32,
    target: f32,
}

struct State {
    cfg: AudioConfig,
    sounds: Sounds,
    sink: Option<MixerDeviceSink>,
    last_retry: Instant,
    last_play: HashMap<Sfx, Instant>,
    /// Start part (split mode) or whole track (full mode).
    main: Option<rodio::Player>,
    movement: Option<MoveLayer>,
    fades: Vec<Fade>,
    dash_count: usize,
    dash_duck: Option<Instant>,
    /// The World: decoded files by name.
    tw_files: HashMap<&'static str, Pcm>,
    tw_players: Vec<rodio::Player>,
    mih_files: HashMap<MihSound, SamplesBuffer>,
    mih_names: HashMap<MihSound, String>,
    mih_players: HashMap<MihSound, rodio::Player>,
    mih_preview: Option<rodio::Player>,
    published: HashSet<String>,
}

impl State {
    fn ensure_sink(&mut self) -> bool {
        if self.sink.is_none() && self.last_retry.elapsed() > Duration::from_secs(1) {
            self.last_retry = Instant::now();
            self.sink = open_sink();
        }
        self.sink.is_some()
    }

    fn audible(&self) -> bool {
        self.cfg.enabled && self.cfg.volume > 0
    }

    fn fire(&mut self, buf: SamplesBuffer, vol: f32) {
        if !self.ensure_sink() {
            return;
        }
        if let Some(s) = self.sink.as_ref() {
            s.mixer().add(buf.amplify(vol));
        }
    }

    fn player(&mut self) -> Option<rodio::Player> {
        if !self.ensure_sink() {
            return None;
        }
        self.sink
            .as_ref()
            .map(|s| rodio::Player::connect_new(s.mixer()))
    }

    fn fade_out(&mut self, p: rodio::Player, dur: Duration) {
        let from = p.volume();
        self.fades.push(Fade {
            player: p,
            from,
            start: Instant::now(),
            dur,
            mih_owner: None,
        });
    }

    fn stop_main(&mut self, dur: Duration) {
        if let Some(p) = self.main.take() {
            self.fade_out(p, dur);
        }
    }

    fn stop_move(&mut self, dur: Duration) {
        if let Some(m) = self.movement.take() {
            self.fade_out(m.player, dur);
        }
    }

    fn move_volume(&self, level: f32) -> f32 {
        // Duck under the start hit and for a moment under each dash crack, so the layers
        // never pile up.
        let mut duck = 1.0;
        if self.main.as_ref().is_some_and(|p| !p.empty()) {
            duck *= 0.5;
        }
        if let Some(t) = self.dash_duck {
            let k = (t.elapsed().as_secs_f32() / 0.6).clamp(0.0, 1.0);
            duck *= 0.35 + 0.65 * k;
        }
        gain(self.cfg.volume) * f32::from(self.cfg.move_volume) / 100.0 * level.powf(1.3) * duck
    }

    fn start_move_layer(&mut self) {
        self.stop_move(Duration::from_millis(80));
        let pcm = self.sounds.move_loop().clone();
        let Some(p) = self.player() else { return };
        p.set_volume(0.0);
        p.append(pcm.buffer().repeat_infinite());
        self.movement = Some(MoveLayer {
            player: p,
            level: 0.0,
            target: 0.0,
        });
    }

    fn on_start(&mut self) {
        if !self.audible() {
            return;
        }
        self.stop_main(Duration::from_millis(60));
        let full_track = self.cfg.mode == SoundMode::FullTrack && self.sounds.parts.is_some();
        let buf = if full_track {
            self.sounds
                .parts
                .as_ref()
                .map(|p| p.full.buffer())
                .unwrap_or_else(|| self.sounds.start())
        } else {
            self.sounds.start()
        };
        if full_track || self.cfg.sv_parts[0] {
            if let Some(p) = self.player() {
                p.set_volume(gain(self.cfg.volume));
                p.append(buf);
                self.main = Some(p);
            }
        }
        if !full_track && self.cfg.sv_parts[1] {
            self.start_move_layer();
        }
    }

    fn on_end(&mut self, natural: bool) {
        self.stop_move(FADE_MOVE);
        let full_track = self.cfg.mode == SoundMode::FullTrack && self.sounds.parts.is_some();
        if full_track && natural {
            // The track already contains its own ending.
            self.main = None;
            return;
        }
        self.stop_main(FADE_START);
        if self.audible() && self.cfg.sv_parts[3] {
            let end = self.sounds.end();
            self.fire(end, gain(self.cfg.volume));
        }
    }

    fn on_overheat(&mut self) {
        self.stop_move(Duration::from_millis(200));
        self.stop_main(Duration::from_millis(200));
        if !self.audible() {
            return;
        }
        let buf = match self.sounds.custom.get(&Sfx::Overheat) {
            Some(b) => b.clone(),
            None if self.sounds.parts.is_some() => self.sounds.end(),
            None => self.sounds.single(Sfx::Overheat),
        };
        self.fire(buf, gain(self.cfg.volume));
    }

    fn on_dash(&mut self) {
        if !self.audible() || !self.cfg.sv_parts[2] || self.rate_limited(Sfx::WarpDash) {
            return;
        }
        let buf = self.sounds.dash(self.dash_count);
        self.dash_count = self.dash_count.wrapping_add(1);
        self.fire(buf, gain(self.cfg.volume));
        self.dash_duck = Some(Instant::now());
    }

    fn tw_clip(&self, c: &Clip) -> Option<Pcm> {
        let p = self.tw_files.get(c.file)?;
        Some(normalize_clip(p.cut(c.from, c.to, 0.004, 0.06)))
    }

    /// The World: play clips at their offsets (a silent lead-in keeps them sample-accurate).
    fn tw_play(&mut self, clips: &[(f32, Clip)], force: bool) {
        if !(self.audible() || force) {
            return;
        }
        let vol = gain(if force {
            self.cfg.volume.max(15)
        } else {
            self.cfg.volume
        });
        for (offset, clip) in clips {
            let Some(pcm) = self.tw_clip(clip) else {
                continue;
            };
            let lead = (offset * pcm.rate.get() as f32) as usize * usize::from(pcm.channels.get());
            let mut samples = vec![0.0; lead];
            samples.extend_from_slice(&pcm.samples);
            let buf = SamplesBuffer::new(pcm.channels, pcm.rate, samples);
            if let Some(p) = self.player() {
                p.set_volume(vol);
                p.append(buf);
                self.tw_players.push(p);
            }
        }
        self.tw_players.retain(|p| !p.empty());
    }

    fn tw_stop_all(&mut self) {
        let players: Vec<_> = self.tw_players.drain(..).collect();
        for p in players {
            self.fade_out(p, Duration::from_millis(180));
        }
    }

    fn mih_volume(&self) -> f32 {
        gain(self.cfg.volume) * gain(self.cfg.mih_volume)
    }

    fn mih_stop_all(&mut self, immediate: bool, keep_reset: bool) {
        let players = std::mem::take(&mut self.mih_players);
        for (sound, player) in players {
            if keep_reset && sound == MihSound::UniverseReset && !player.empty() {
                self.mih_players.insert(sound, player);
            } else if immediate {
                player.stop();
            } else {
                self.fades.push(Fade {
                    from: player.volume(),
                    player,
                    start: Instant::now(),
                    dur: Duration::from_millis(180),
                    mih_owner: Some(sound),
                });
            }
        }
        if let Some(player) = self.mih_preview.take() {
            player.stop();
        }
        if immediate {
            self.fades.retain(|fade| {
                if fade.mih_owner.is_some() {
                    fade.player.stop();
                    false
                } else {
                    true
                }
            });
        }
    }

    fn mih_play(&mut self, sound: MihSound, preview: bool) {
        if !preview && !(self.audible() && self.cfg.mih_enabled) {
            return;
        }
        if self.cfg.volume == 0 || self.cfg.mih_volume == 0 {
            return;
        }
        let Some(buffer) = self.mih_files.get(&sound).cloned() else {
            return;
        };
        if preview {
            if let Some(player) = self.mih_preview.take() {
                player.stop();
            }
        } else {
            if self
                .mih_players
                .get(&sound)
                .is_some_and(|player| !player.empty())
            {
                return;
            }
            if sound == MihSound::Activation {
                // Loop/re-activation: don't overlap the previous reset or activation tail.
                self.mih_stop_all(true, false);
            }
            self.fades.retain(|fade| {
                if fade.mih_owner == Some(sound) {
                    fade.player.stop();
                    false
                } else {
                    true
                }
            });
        }
        let volume = self.mih_volume();
        if let Some(player) = self.player() {
            player.set_volume(volume);
            player.append(buffer);
            if preview {
                self.mih_preview = Some(player);
            } else {
                self.mih_players.insert(sound, player);
            }
        }
    }

    fn rate_limited(&mut self, sfx: Sfx) -> bool {
        let now = Instant::now();
        if let Some(prev) = self.last_play.get(&sfx) {
            if now.duration_since(*prev) < Duration::from_millis(sfx.min_interval_ms()) {
                return true;
            }
        }
        self.last_play.insert(sfx, now);
        false
    }

    fn preview(&mut self, part: Part) {
        let vol = gain(self.cfg.volume.max(15));
        self.stop_main(Duration::from_millis(80));
        let buf = match part {
            Part::Start => self.sounds.start(),
            Part::Dash => {
                let b = self.sounds.dash(self.dash_count);
                self.dash_count = self.dash_count.wrapping_add(1);
                b
            }
            Part::End => self.sounds.end(),
            Part::Move => {
                let mv = f32::from(self.cfg.move_volume.max(20)) / 100.0;
                self.sounds
                    .move_loop()
                    .looped_preview(3.0)
                    .gain(mv)
                    .buffer()
            }
            Part::Full => match self.sounds.parts.as_ref() {
                Some(p) => p.full.buffer(),
                None => self.sounds.start(),
            },
        };
        if let Some(p) = self.player() {
            p.set_volume(vol);
            p.append(buf);
            self.main = Some(p);
        }
    }

    /// Advance fades and the movement layer. Returns true while ticking is still needed.
    fn tick(&mut self, dt: f32) -> bool {
        self.fades.retain(|f| {
            let k = f.start.elapsed().as_secs_f32() / f.dur.as_secs_f32().max(0.001);
            if k >= 1.0 {
                f.player.stop();
                false
            } else {
                // Exponential-ish curve sounds smoother than linear.
                f.player.set_volume(f.from * (1.0 - k) * (1.0 - k));
                true
            }
        });
        let vol_for = |s: &State, l: f32| s.move_volume(l);
        if let Some(m) = self.movement.as_ref() {
            let (level, target) = (m.level, m.target);
            // Fast attack, slower release: movement "swooshes" in and trails off.
            let rate = if target > level { 14.0 } else { 3.2 };
            let new = level + (target - level) * (1.0 - (-dt * rate).exp());
            let v = vol_for(self, new);
            if let Some(m) = self.movement.as_mut() {
                m.level = new;
                // Volume only: changing the playback speed every tick makes rodio's mixer
                // re-resample and crackle.
                m.player.set_volume(v);
            }
        }
        if self.main.as_ref().is_some_and(|p| p.empty()) {
            self.main = None;
        }
        self.mih_players.retain(|_, player| !player.empty());
        if self
            .mih_preview
            .as_ref()
            .is_some_and(|player| player.empty())
        {
            self.mih_preview = None;
        }
        !self.fades.is_empty()
            || self.movement.is_some()
            || !self.mih_players.is_empty()
            || self.mih_preview.is_some()
    }

    fn publish(&mut self, shared: &Arc<Mutex<AudioInfo>>) {
        if let Ok(mut i) = shared.lock() {
            *i = self.sounds.info.clone();
            i.loaded = true;
            for sound in MihSound::ALL {
                i.mih_loaded[sound.index()] = self.mih_files.contains_key(&sound);
            }
            i.mih_parts = MihSound::ALL
                .iter()
                .map(|sound| {
                    format!(
                        "{}: {}",
                        sound.label(),
                        self.mih_names
                            .get(sound)
                            .map(String::as_str)
                            .unwrap_or("silent (no matching file)")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
        }
        let names: HashSet<String> = self.sounds.info.custom.iter().cloned().collect();
        if names != self.published {
            tracing::info!("sounds: {}", self.sounds.info.parts);
            self.published = names;
        }
    }
}

fn load_theworld() -> HashMap<&'static str, Pcm> {
    let mut map = HashMap::new();
    for name in dubs::ALL_FILES {
        match theworld_file(name) {
            Some(path) => {
                if let Some(p) = decode_pcm(&path) {
                    map.insert(name, p);
                }
            }
            None => tracing::info!("The World sound missing: theworld/{name}"),
        }
    }
    map
}

fn load_mih() -> (HashMap<MihSound, SamplesBuffer>, HashMap<MihSound, String>) {
    let mut buffers = HashMap::new();
    let mut names = HashMap::new();
    for (event, path) in mih::discover(&mih::asset_dirs()) {
        if let Some(pcm) = decode_pcm(&path) {
            names.insert(event, file_name(&path));
            // Keep the supplied recording intact, only add tiny anti-click fades and
            // common loudness/headroom using the existing PCM engine.
            let length = pcm.secs();
            buffers.insert(
                event,
                normalize_clip(pcm.cut(0.0, length, 0.004, 0.025))
                    .gain(0.75)
                    .buffer(),
            );
        }
    }
    (buffers, names)
}

fn run(rx: Receiver<AudioCmd>, cfg: AudioConfig, shared: Arc<Mutex<AudioInfo>>) {
    let (mih_files, mih_names) = load_mih();
    let mut st = State {
        cfg,
        sounds: Sounds::load(cfg.profile),
        sink: open_sink(),
        last_retry: Instant::now(),
        last_play: HashMap::new(),
        main: None,
        movement: None,
        fades: Vec::new(),
        dash_count: 0,
        dash_duck: None,
        tw_files: load_theworld(),
        tw_players: Vec::new(),
        mih_files,
        mih_names,
        mih_players: HashMap::new(),
        mih_preview: None,
        published: HashSet::new(),
    };
    st.publish(&shared);
    let mut ticking = false;
    let mut last_tick = Instant::now();

    loop {
        let timeout = if ticking {
            Some(TICK)
        } else if st.sink.is_none() {
            Some(Duration::from_secs(5))
        } else {
            None
        };
        let cmd = match timeout {
            Some(t) => match rx.recv_timeout(t) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            },
            None => match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            },
        };
        if let Some(cmd) = cmd {
            match cmd {
                AudioCmd::Shutdown => return,
                AudioCmd::SetEnabled(v) => {
                    st.cfg.enabled = v;
                    if !v {
                        st.stop_main(FADE_START);
                        st.stop_move(FADE_START);
                        st.mih_stop_all(false, false);
                    }
                }
                AudioCmd::SetVolume(v) => {
                    st.cfg.volume = v.min(100);
                    let g = gain(st.cfg.volume);
                    if let Some(p) = st.main.as_ref() {
                        p.set_volume(g);
                    }
                    let volume = st.mih_volume();
                    for player in st.mih_players.values() {
                        player.set_volume(volume);
                    }
                    if let Some(player) = &st.mih_preview {
                        player.set_volume(volume);
                    }
                }
                AudioCmd::SetMoveVolume(v) => st.cfg.move_volume = v.min(100),
                AudioCmd::SetMode(m) => st.cfg.mode = m,
                AudioCmd::SetSvParts(p) => {
                    st.cfg.sv_parts = p;
                    if !p[1] {
                        st.stop_move(FADE_MOVE);
                    }
                }
                AudioCmd::TwPlay(clips) => st.tw_play(&clips, false),
                AudioCmd::TwTick => st.tw_play(&[(0.0, dubs::TICK)], false),
                AudioCmd::TwPreview(clips) => {
                    st.tw_stop_all();
                    st.tw_play(&clips, true);
                }
                AudioCmd::TwStopAll => st.tw_stop_all(),
                AudioCmd::MihPlay(sound) => st.mih_play(sound, false),
                AudioCmd::MihPreview(sound) => st.mih_play(sound, true),
                AudioCmd::MihFinish => st.mih_stop_all(false, true),
                AudioCmd::MihStopAll { immediate } => st.mih_stop_all(immediate, false),
                AudioCmd::SetMihEnabled(enabled) => {
                    st.cfg.mih_enabled = enabled;
                    if !enabled {
                        st.mih_stop_all(false, false);
                    }
                }
                AudioCmd::SetMihVolume(volume) => {
                    st.cfg.mih_volume = volume.min(100);
                    let volume = st.mih_volume();
                    for player in st.mih_players.values() {
                        player.set_volume(volume);
                    }
                    if let Some(player) = &st.mih_preview {
                        player.set_volume(volume);
                    }
                }
                AudioCmd::SetProfile(p) => {
                    if p != st.cfg.profile {
                        st.cfg.profile = p;
                        st.sounds = Sounds::load(p);
                        st.publish(&shared);
                    }
                }
                AudioCmd::Reload => {
                    if let Ok(mut info) = shared.lock() {
                        info.loaded = false;
                    }
                    st.sounds = Sounds::load(st.cfg.profile);
                    st.tw_files = load_theworld();
                    (st.mih_files, st.mih_names) = load_mih();
                    st.publish(&shared);
                }
                AudioCmd::Start => st.on_start(),
                AudioCmd::End { natural } => st.on_end(natural),
                AudioCmd::Overheat => st.on_overheat(),
                AudioCmd::Dash => st.on_dash(),
                AudioCmd::Motion(level) => {
                    if let Some(m) = st.movement.as_mut() {
                        m.target = level.clamp(0.0, 1.0);
                    }
                }
                AudioCmd::Preview(part) => st.preview(part),
                AudioCmd::Play(sfx) => {
                    if st.audible() && !st.rate_limited(sfx) {
                        if let Some(buf) = st.sounds.one_shot(sfx) {
                            st.fire(buf, gain(st.cfg.volume));
                        }
                    }
                }
            }
        } else if st.sink.is_none() && st.last_retry.elapsed() > Duration::from_secs(5) {
            st.last_retry = Instant::now();
            st.sink = open_sink();
        }
        let now = Instant::now();
        let dt = now.duration_since(last_tick).as_secs_f32().min(0.1);
        last_tick = now;
        ticking = st.tick(dt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: f32) -> Pcm {
        let n = (secs * SAMPLE_RATE as f32) as usize;
        Pcm::mono(
            (0..n)
                .map(|i| (i as f32 / SAMPLE_RATE as f32 * 220.0 * std::f32::consts::TAU).sin())
                .collect(),
        )
    }

    #[test]
    fn cut_plan_matches_reference_sound() {
        let p = CutPlan::for_length(15.84);
        assert!((p.start[1] - 3.27).abs() < 1e-3);
        assert!((p.end[0] - 13.72).abs() < 1e-3);
        assert_eq!(p.dash.len(), 2);
        assert!(p.sane(15.84));
        // Proportional for other lengths, always inside the file.
        let q = CutPlan::for_length(8.0);
        assert!(q.sane(8.0));
        assert!(q.end[1] <= 8.0 && q.start[1] < q.move_[0]);
    }

    #[test]
    fn parts_have_expected_lengths() {
        let p = tone(15.84);
        let parts = Parts::cut(&p, &CutPlan::for_length(15.84));
        assert!((parts.start.secs() - 3.19).abs() < 0.01);
        assert!((parts.end.secs() - 2.12).abs() < 0.01);
        assert_eq!(parts.dashes.len(), 2);
        // Seamless loop is shorter by the cross-fade.
        assert!((parts.move_loop.secs() - (3.9 - 0.45)).abs() < 0.01);
        assert!((parts.move_loop.rms() - MOVE_RMS).abs() < 0.01);
    }

    #[test]
    fn cuts_are_faded_so_they_never_click() {
        let p = tone(4.0).cut(0.5, 2.0, 0.01, 0.05);
        assert!(p.samples[0].abs() < 1e-3);
        assert!(p.samples.last().unwrap().abs() < 0.01);
    }

    #[test]
    fn seamless_loop_wraps_continuously() {
        // A ramp makes discontinuities obvious: the wrap must not jump.
        let n = SAMPLE_RATE as usize * 2;
        let ramp = Pcm::mono((0..n).map(|i| (i as f32 / 4410.0).sin()).collect());
        let l = ramp.seamless_loop(0.0, 2.0, 0.3);
        let first = l.samples[0];
        let last = *l.samples.last().unwrap();
        assert!((first - last).abs() < 0.01, "jump {first} -> {last}");
    }

    #[test]
    fn flatten_removes_swells() {
        // 1 s quiet, 1 s loud: after flattening both halves are about equally loud.
        let mut s = tone(2.0);
        let half = s.samples.len() / 2;
        for x in &mut s.samples[..half] {
            *x *= 0.3;
        }
        let f = s.flatten(0.1);
        let a = f.cut(0.2, 0.8, 0.0, 0.0).rms();
        let b = f.cut(1.2, 1.8, 0.0, 0.0).rms();
        assert!((a / b - 1.0).abs() < 0.2, "{a} vs {b}");
    }

    #[test]
    fn cut_plan_json() {
        let j = r#"{"start":[0,1],"move":[1,2],"dash":[[2,2.5]],"end":[3,4]}"#;
        let p: CutPlan = serde_json::from_str(j).unwrap();
        assert!(p.sane(4.0));
        assert!(!p.sane(0.5));
    }

    #[test]
    fn synth_move_loop_is_normalized() {
        let l = synth_move_loop(SfxProfile::David);
        assert!(l.secs() > 1.5);
        assert!((l.rms() - MOVE_RMS).abs() < 0.01);
    }

    #[test]
    fn provided_mih_mp3s_decode_when_present() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("MIHassets");
        let files = mih::discover(&[dir]);
        for (event, path) in &files {
            let pcm =
                decode_pcm(path).expect("present MIH sound must decode with rodio's MP3 feature");
            assert!(pcm.secs() > 0.0);
            assert!(pcm.samples.iter().all(|sample| sample.is_finite()));
            eprintln!(
                "{}: {} ({:.3}s, {} channels, {} Hz)",
                event.label(),
                path.display(),
                pcm.secs(),
                pcm.channels,
                pcm.rate
            );
        }
        // Copyrighted audio is deliberately absent from fresh GitHub checkouts.
        eprintln!("Validated {} locally provided MIH audio files", files.len());
    }
}
