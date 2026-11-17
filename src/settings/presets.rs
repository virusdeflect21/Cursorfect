//! Visual/audio styles for the built-in presets and the user's custom style.

use super::config::{PresetId, Settings};

pub type Rgb = [f32; 3];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SfxProfile {
    David,
    Smasher,
    Ghost,
}

/// Fully resolved look of the effects for the current settings.
#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub primary: Rgb,
    pub secondary: Rgb,
    /// Multiplier on the cursor glow radius/strength.
    pub glow: f32,
    /// Multiplier on trail ribbon width.
    pub trail_width: f32,
    /// Multiplier on particle spawn rate.
    pub particles: f32,
    /// Multiplier on shockwave size.
    pub shock_scale: f32,
    /// Multiplier on shockwave brightness.
    pub shock_alpha: f32,
    /// Multiplier on dash streak width.
    pub dash_width: f32,
    /// Afterimage colour when young / old (echoes shift colour as they age).
    pub echo_young: Rgb,
    pub echo_old: Rgb,
    /// Afterimage colour cycle: consecutive frames sweep through these colours.
    pub palette: [Rgb; 6],
    pub sfx: SfxProfile,
}

pub fn parse_hex(s: &str) -> Option<Rgb> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 || !s.is_ascii() {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some([
        ((v >> 16) & 0xFF) as f32 / 255.0,
        ((v >> 8) & 0xFF) as f32 / 255.0,
        (v & 0xFF) as f32 / 255.0,
    ])
}

fn hx(s: &str) -> Rgb {
    parse_hex(s).unwrap_or([1.0, 1.0, 1.0])
}

pub fn preset_style(id: PresetId) -> Style {
    match id {
        PresetId::David | PresetId::Custom => Style {
            primary: parse_hex("#68F5B0").unwrap_or([0.4, 0.96, 0.69]),
            secondary: parse_hex("#00D9FF").unwrap_or([0.0, 0.85, 1.0]),
            glow: 1.0,
            trail_width: 1.0,
            particles: 0.8,
            shock_scale: 1.0,
            shock_alpha: 1.0,
            dash_width: 1.0,
            echo_young: [0.45, 0.93, 1.0],
            echo_old: [0.16, 0.38, 1.0],
            // cyan -> green -> lime -> yellow -> magenta -> blue (the show's afterimage sweep)
            palette: [
                hx("#3DF2FF"),
                hx("#2BFF9C"),
                hx("#C6FF3D"),
                hx("#FFE14A"),
                hx("#FF4FD8"),
                hx("#4F7BFF"),
            ],
            sfx: SfxProfile::David,
        },
        PresetId::Smasher => Style {
            primary: parse_hex("#FF304F").unwrap_or([1.0, 0.19, 0.31]),
            secondary: parse_hex("#FF783D").unwrap_or([1.0, 0.47, 0.24]),
            glow: 1.15,
            trail_width: 1.2,
            particles: 1.8,
            shock_scale: 1.25,
            shock_alpha: 1.1,
            dash_width: 0.8, // sharper
            echo_young: [1.0, 0.62, 0.30],
            echo_old: [0.85, 0.08, 0.25],
            palette: [
                hx("#FF2A4F"),
                hx("#FF7A2E"),
                hx("#FFD23F"),
                hx("#FF3DAA"),
                hx("#B3122E"),
                hx("#FF5A36"),
            ],
            sfx: SfxProfile::Smasher,
        },
        PresetId::Ghost => Style {
            primary: parse_hex("#4EA5FF").unwrap_or([0.31, 0.65, 1.0]),
            secondary: parse_hex("#B3E6FF").unwrap_or([0.7, 0.9, 1.0]),
            glow: 0.8,
            trail_width: 0.6,
            particles: 0.5,
            shock_scale: 0.6,
            shock_alpha: 0.6,
            dash_width: 0.9,
            echo_young: [0.85, 0.96, 1.0],
            echo_old: [0.22, 0.45, 1.0],
            palette: [
                hx("#E8F6FF"),
                hx("#8FD3FF"),
                hx("#4EA5FF"),
                hx("#7B6BFF"),
                hx("#B3E6FF"),
                hx("#2E6BFF"),
            ],
            sfx: SfxProfile::Ghost,
        },
    }
}

pub fn style_for(settings: &Settings) -> Style {
    let mut st = preset_style(settings.preset);
    if settings.preset == PresetId::Custom {
        if let Some(c) = parse_hex(&settings.custom.primary) {
            st.primary = c;
        }
        if let Some(c) = parse_hex(&settings.custom.secondary) {
            st.secondary = c;
        }
        st.echo_young = st.secondary;
        st.echo_old = st.primary;
        let (a, b) = (st.primary, st.secondary);
        let m = |t: f32| {
            [
                a[0] + (b[0] - a[0]) * t,
                a[1] + (b[1] - a[1]) * t,
                a[2] + (b[2] - a[2]) * t,
            ]
        };
        st.palette = [a, m(0.25), m(0.5), m(0.75), b, m(0.5)];
    }
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: Rgb, b: Rgb) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.01)
    }

    #[test]
    fn hex_parsing() {
        assert!(approx(parse_hex("#FF0000").unwrap(), [1.0, 0.0, 0.0]));
        assert!(approx(parse_hex("00ff00").unwrap(), [0.0, 1.0, 0.0]));
        assert!(parse_hex("#12345").is_none());
        assert!(parse_hex("#GG0000").is_none());
        assert!(parse_hex("\u{e9}\u{e9}\u{e9}").is_none());
    }

    #[test]
    fn builtin_preset_colors() {
        let d = preset_style(PresetId::David);
        assert!(approx(d.primary, parse_hex("#68F5B0").unwrap()));
        assert!(approx(d.secondary, parse_hex("#00D9FF").unwrap()));
        let s = preset_style(PresetId::Smasher);
        assert!(approx(s.primary, parse_hex("#FF304F").unwrap()));
        assert!(approx(s.secondary, parse_hex("#FF783D").unwrap()));
        let g = preset_style(PresetId::Ghost);
        assert!(approx(g.primary, parse_hex("#4EA5FF").unwrap()));
        assert!(approx(g.secondary, parse_hex("#B3E6FF").unwrap()));
    }

    #[test]
    fn custom_uses_user_colors() {
        let mut s = Settings::default();
        s.preset = PresetId::Custom;
        s.custom.primary = "#112233".into();
        let st = style_for(&s);
        assert!(approx(st.primary, parse_hex("#112233").unwrap()));
        s.preset = PresetId::Smasher;
        assert!(approx(style_for(&s).primary, parse_hex("#FF304F").unwrap()));
    }

    #[test]
    fn preset_index_roundtrip() {
        for p in PresetId::ALL {
            assert_eq!(PresetId::from_index(p.index()), p);
        }
        assert_eq!(PresetId::from_index(99), PresetId::David);
    }
}
