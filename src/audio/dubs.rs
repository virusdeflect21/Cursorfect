//! The World: which slice of which sound file plays for each dub, and when time actually
//! stops / resumes relative to the sound (so the freeze lands on the SFX impact, after the
//! voice line, like in the show).
//!
//! All cut points were measured on the supplied files (20 ms resolution): voice and SFX are
//! separated where the file has them one after the other. Files live in `sounds/theworld/`.

use serde::{Deserialize, Serialize};

/// A slice of a sound file (seconds).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clip {
    pub file: &'static str,
    pub from: f32,
    pub to: f32,
}

impl Clip {
    pub const fn new(file: &'static str, from: f32, to: f32) -> Self {
        Self { file, from, to }
    }

    pub fn len(&self) -> f32 {
        (self.to - self.from).max(0.0)
    }
}

pub const F_DIO_START: &str = "dio_stop.mp3";
pub const F_DIO_RESUME: &str = "dio_resume.mp3";
pub const F_SFX_ONLY: &str = "sfx_only.mp3";
pub const F_JOTARO_JP: &str = "jotaro_jp.mp3";
pub const F_JOTARO_EN: &str = "jotaro_en.mp3";
pub const F_JOTARO_RESUME: &str = "jotaro_resume.mp3";
pub const F_DIEGO: &str = "diego.mp3";
pub const F_OVER_HEAVEN: &str = "over_heaven.mp3";

pub const ALL_FILES: [&str; 11] = [
    F_DIO_START,
    F_DIO_RESUME,
    F_SFX_ONLY,
    F_JOTARO_JP,
    F_JOTARO_EN,
    F_JOTARO_RESUME,
    F_DIEGO,
    F_OVER_HEAVEN,
    "custom_stop.mp3", "custom_tick.mp3", "custom_resume.mp3",
];

/// Voice-less time stop (also the fallback when a dub's voice is switched off but its SFX
/// can't be separated from the voice). The SFX swells, its peak is ~0.85 s in.
pub const GENERIC_STOP: Clip = Clip::new(F_SFX_ONLY, 0.15, 4.85);
pub const GENERIC_STOP_IMPACT: f32 = 0.85;
/// Voice-less time resume, loudest ~0.6 s in.
pub const GENERIC_RESUME: Clip = Clip::new(F_SFX_ONLY, 14.75, 19.6);
pub const GENERIC_RESUME_IMPACT: f32 = 0.6;
/// One clock tick (played once per frozen second).
pub const TICK: Clip = Clip::new(F_DIEGO, 3.61, 4.08);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dub {
    /// DIO (Japanese): "ZA WARUDO! Toki yo tomare!" / "Toki wa ugoki dasu".
    #[default]
    DioJp,
    /// Jotaro (Japanese): "Star Platinum: The World".
    JotaroJp,
    /// Jotaro (English dub).
    JotaroEn,
    /// Diego Brando (Steel Ball Run).
    Diego,
    /// DIO, The World Over Heaven.
    OverHeaven,
    /// No voice lines, sound effects only.
    SfxOnly,
    Custom,
}

impl Dub {
    pub const ALL: [Dub; 7] = [
        Dub::DioJp,
        Dub::JotaroJp,
        Dub::JotaroEn,
        Dub::Diego,
        Dub::OverHeaven,
        Dub::SfxOnly,
        Dub::Custom,
    ];

    pub fn index(self) -> i32 {
        Self::ALL.iter().position(|d| *d == self).unwrap_or(0) as i32
    }

    pub fn from_index(i: i32) -> Self {
        usize::try_from(i)
            .ok()
            .and_then(|i| Self::ALL.get(i).copied())
            .unwrap_or_default()
    }

    pub fn def(self) -> DubDef {
        match self {
            Dub::DioJp => DubDef {
                stop: Part::Split {
                    voice: Clip::new(F_DIO_START, 0.18, 2.11),
                    sfx: Clip::new(F_DIO_START, 2.11, 4.25),
                    impact: 0.05,
                },
                resume: Part::Split {
                    voice: Clip::new(F_DIO_RESUME, 0.35, 1.70),
                    sfx: Clip::new(F_DIO_RESUME, 1.70, 4.15),
                    impact: 0.28,
                },
            },
            Dub::JotaroJp => DubDef {
                stop: Part::Split {
                    voice: Clip::new(F_JOTARO_JP, 0.20, 2.14),
                    sfx: Clip::new(F_JOTARO_JP, 2.14, 4.15),
                    impact: 0.05,
                },
                resume: Part::SfxOnly {
                    sfx: Clip::new(F_JOTARO_JP, 9.50, 11.20),
                    impact: 0.7,
                },
            },
            Dub::JotaroEn => DubDef {
                stop: Part::Split {
                    voice: Clip::new(F_JOTARO_EN, 0.0, 1.97),
                    sfx: Clip::new(F_JOTARO_EN, 1.97, 3.67),
                    impact: 0.05,
                },
                resume: Part::SfxOnly {
                    sfx: Clip::new(F_JOTARO_RESUME, 0.05, 1.76),
                    impact: 0.72,
                },
            },
            Dub::Diego => DubDef {
                // Voice and SFX overlap in this file: they can't be separated.
                stop: Part::Mixed {
                    clip: Clip::new(F_DIEGO, 0.15, 3.40),
                    impact: 0.45,
                },
                resume: Part::Mixed {
                    clip: Clip::new(F_DIEGO, 9.55, 11.65),
                    impact: 0.5,
                },
            },
            Dub::OverHeaven => DubDef {
                stop: Part::Split {
                    voice: Clip::new(F_OVER_HEAVEN, 0.30, 2.32),
                    sfx: Clip::new(F_OVER_HEAVEN, 2.32, 4.20),
                    impact: 0.08,
                },
                // "end it with normal time resume"
                resume: Part::SfxOnly {
                    sfx: GENERIC_RESUME,
                    impact: GENERIC_RESUME_IMPACT,
                },
            },
            Dub::Custom => DubDef {
                stop: Part::SfxOnly { sfx: Clip::new("custom_stop.mp3", 0.0, 3600.0), impact: 0.0 },
                resume: Part::SfxOnly { sfx: Clip::new("custom_resume.mp3", 0.0, 3600.0), impact: 0.0 },
            },
            Dub::SfxOnly => DubDef {
                stop: Part::SfxOnly {
                    sfx: GENERIC_STOP,
                    impact: GENERIC_STOP_IMPACT,
                },
                resume: Part::SfxOnly {
                    sfx: GENERIC_RESUME,
                    impact: GENERIC_RESUME_IMPACT,
                },
            },
        }
    }
}

/// How one moment (stop or resume) is voiced.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Part {
    /// Voice line, then the SFX. `impact`: seconds into the SFX where time stops/resumes.
    Split { voice: Clip, sfx: Clip, impact: f32 },
    /// Voice and SFX in one inseparable clip.
    Mixed { clip: Clip, impact: f32 },
    /// No voice line.
    SfxOnly { sfx: Clip, impact: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DubDef {
    pub stop: Part,
    pub resume: Part,
}

/// What to play and when the visual moment (freeze / unfreeze) happens.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Plan {
    /// Clips with their start offset (seconds from the trigger).
    pub clips: Vec<(f32, Clip)>,
    /// Seconds from the trigger to the freeze (or unfreeze).
    pub moment: f32,
}

/// Build the plan for one moment.
/// * `voice` / `sfx`: the user's per-sound switches for this moment.
/// * `wait_for_voice`: time stops after the voice line (authentic) instead of instantly.
/// * `generic`: voice-less fallback SFX + its impact (used when a mixed clip's voice is off).
/// * `available`: whether a file can be played (missing files are skipped; timing adapts).
pub fn plan(
    part: Part,
    voice: bool,
    sfx: bool,
    wait_for_voice: bool,
    generic: (Clip, f32),
    available: &dyn Fn(&str) -> bool,
) -> Plan {
    let mut p = Plan::default();
    let ok = |c: &Clip| available(c.file);
    match part {
        Part::Split {
            voice: v,
            sfx: s,
            impact,
        } => {
            let voice_on = voice && ok(&v);
            let sfx_on = sfx && ok(&s);
            let lead = if voice_on && wait_for_voice {
                v.len()
            } else {
                0.0
            };
            if voice_on {
                p.clips.push((0.0, v));
            }
            if sfx_on {
                p.clips.push((lead, s));
            }
            p.moment = lead + if sfx_on { impact } else { 0.0 };
        }
        Part::Mixed { clip, impact } => {
            if voice && ok(&clip) {
                p.clips.push((0.0, clip));
                p.moment = if wait_for_voice { impact } else { 0.0 };
            } else if sfx && ok(&generic.0) {
                p.clips.push((0.0, generic.0));
                p.moment = generic.1;
            }
        }
        Part::SfxOnly { sfx: s, impact } => {
            if sfx && ok(&s) {
                p.clips.push((0.0, s));
                p.moment = impact;
            }
        }
    }
    if !wait_for_voice {
        // "Instant" mode: time stops right away; the sounds still play.
        p.moment = p.moment.min(0.12);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(_: &str) -> bool {
        true
    }

    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn dio_waits_for_the_voice_then_freezes_on_the_sfx() {
        let d = Dub::DioJp.def();
        let p = plan(d.stop, true, true, true, (GENERIC_STOP, 0.85), &all);
        assert_eq!(p.clips.len(), 2);
        let voice_len = 2.11 - 0.18;
        assert!((p.clips[1].0 - voice_len).abs() < 1e-4, "SFX starts after the voice");
        assert!((p.moment - (voice_len + 0.05)).abs() < 1e-4);
    }

    #[test]
    fn voice_off_freezes_on_the_sfx_immediately() {
        let d = Dub::DioJp.def();
        let p = plan(d.stop, false, true, true, (GENERIC_STOP, 0.85), &all);
        assert_eq!(p.clips.len(), 1);
        assert_eq!(p.clips[0].0, 0.0);
        assert!(p.moment < 0.1);
    }

    #[test]
    fn sfx_off_keeps_the_voice() {
        let d = Dub::DioJp.def();
        let p = plan(d.stop, true, false, true, (GENERIC_STOP, 0.85), &all);
        assert_eq!(p.clips.len(), 1);
        assert_eq!(p.clips[0].1.from, 0.18);
        assert!((p.moment - (2.11 - 0.18)).abs() < 1e-4);
    }

    #[test]
    fn mixed_dub_without_voice_uses_the_generic_sfx() {
        let d = Dub::Diego.def();
        let p = plan(d.stop, false, true, true, (GENERIC_STOP, 0.85), &all);
        assert_eq!(p.clips, vec![(0.0, GENERIC_STOP)]);
        assert_eq!(p.moment, 0.85);
    }

    #[test]
    fn missing_files_or_everything_off_means_instant_and_silent() {
        let d = Dub::DioJp.def();
        let p = plan(d.stop, true, true, true, (GENERIC_STOP, 0.85), &none);
        assert!(p.clips.is_empty());
        assert_eq!(p.moment, 0.0);
        let q = plan(d.stop, false, false, true, (GENERIC_STOP, 0.85), &all);
        assert!(q.clips.is_empty());
        assert_eq!(q.moment, 0.0);
    }

    #[test]
    fn instant_mode_never_waits() {
        for dub in Dub::ALL {
            let d = dub.def();
            let p = plan(d.stop, true, true, false, (GENERIC_STOP, 0.85), &all);
            assert!(p.moment <= 0.12, "{dub:?}");
        }
    }

    #[test]
    fn every_clip_is_well_formed() {
        for dub in Dub::ALL {
            let d = dub.def();
            for part in [d.stop, d.resume] {
                let clips: Vec<Clip> = match part {
                    Part::Split { voice, sfx, .. } => vec![voice, sfx],
                    Part::Mixed { clip, .. } => vec![clip],
                    Part::SfxOnly { sfx, .. } => vec![sfx],
                };
                for c in clips {
                    assert!(c.len() > 0.3, "{dub:?} {c:?}");
                    assert!(ALL_FILES.contains(&c.file));
                }
            }
            assert_eq!(Dub::from_index(dub.index()), dub);
        }
    }
}
