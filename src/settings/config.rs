//! Persistent configuration: `%APPDATA%\CursorFect\settings.json`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::audio::dubs::Dub;
use crate::input::shortcuts::Shortcut;

/// Which effect the activation shortcut triggers (only one at a time).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectKind {
    #[default]
    Sandevistan,
    TheWorld,
    MadeInHeaven,
}

/// Hold / toggle / timed activation (The World and Made in Heaven).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeStopMode {
    /// Time is stopped while the shortcut is held.
    Hold,
    /// Press to stop time, press again to resume. No time limit.
    #[default]
    Toggle,
    /// Time resumes by itself after `duration_secs`.
    Timed,
}

/// MIH has independent controls; adding it never changes either existing shortcut.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MadeInHeavenSettings {
    pub enabled: bool,
    pub activation: Shortcut,
    pub mode: TimeStopMode,
    /// Overall activation limit in Timed mode, including repeated cycles.
    pub duration_secs: f32,
    /// Optional screen-space pointer multiplier, not a Windows sensitivity setting.
    pub real_acceleration: bool,
    pub repeat_after_reset: bool,
    pub max_multiplier: f32,
    pub ramp_up_secs: f32,
    pub deceleration_secs: f32,
    pub reset_secs: f32,
    pub afterimages: bool,
    pub motion_trails: bool,
    pub speed_particles: bool,
    pub heaven_aura: bool,
    pub reset_flash: bool,
    pub indicator: bool,
    /// 0..=100, independent of the Sandevistan look.
    pub intensity: u8,
    pub sfx: bool,
    /// Relative to the existing master audio volume; 100 preserves that volume.
    pub volume: u8,
}

impl Default for MadeInHeavenSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            activation: Shortcut::default_activation(),
            mode: TimeStopMode::Toggle,
            duration_secs: 20.0,
            real_acceleration: true,
            repeat_after_reset: false,
            max_multiplier: 4.0,
            ramp_up_secs: 20.0,
            deceleration_secs: 1.5,
            reset_secs: 1.5,
            afterimages: true,
            motion_trails: true,
            speed_particles: true,
            heaven_aura: true,
            reset_flash: true,
            indicator: true,
            intensity: 80,
            sfx: true,
            volume: 100,
        }
    }
}

impl MadeInHeavenSettings {
    pub fn sanitize(&mut self) {
        self.duration_secs = finite_or(self.duration_secs, 20.0).clamp(1.0, 300.0);
        self.max_multiplier = finite_or(self.max_multiplier, 4.0).clamp(1.0, 6.0);
        self.ramp_up_secs = finite_or(self.ramp_up_secs, 20.0).clamp(5.0, 120.0);
        self.deceleration_secs = finite_or(self.deceleration_secs, 1.5).clamp(0.2, 5.0);
        self.reset_secs = finite_or(self.reset_secs, 1.5).clamp(0.2, 5.0);
        self.intensity = self.intensity.min(100);
        self.volume = self.volume.min(100);
        if self.activation.validate().is_err() {
            self.activation = Shortcut::default_activation();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TheWorldSettings {
    pub mode: TimeStopMode,
    /// Timed mode length, 1..=9 seconds.
    pub duration_secs: u8,
    pub dub: Dub,
    pub windups: [f32; 7],
    pub custom_primary: String,
    pub custom_secondary: String,
    /// "ZA WARUDO!" voice line.
    pub voice: bool,
    /// The time-stop sound effect.
    pub stop_sfx: bool,
    /// Clock tick every frozen second.
    pub ticks: bool,
    /// "Toki wa ugoki dasu" voice line.
    pub resume_voice: bool,
    pub resume_sfx: bool,
    /// Time stops after the voice line, on the SFX impact (as in the show). Off: instantly.
    pub wait_for_voice: bool,
    pub flash: bool,
    /// Colour inversion sphere + chromatic separation.
    pub inversion: bool,
    /// Gold / violet grade on the frozen world.
    pub tint: bool,
    pub ripples: bool,
    /// Energy streaks drawn into the cursor during the voice line.
    pub streaks: bool,
    /// Lens pinch + anamorphic flare the instant before time stops.
    pub flare: bool,
    /// Light rays when time stops.
    pub rays: bool,
    pub countdown: bool,
    /// Drag real windows while time is stopped; they move when time resumes.
    pub move_windows: bool,
    /// Safety limit for Hold / Toggle (seconds); time always resumes after this.
    pub max_secs: u32,
}

impl Default for TheWorldSettings {
    fn default() -> Self {
        Self {
            mode: TimeStopMode::Toggle,
            duration_secs: 5,
            dub: Dub::DioJp,
            windups: [-1.0; 7],
            custom_primary: "#E95BFF".into(),
            custom_secondary: "#6037C8".into(),
            voice: true,
            stop_sfx: true,
            ticks: true,
            resume_voice: true,
            resume_sfx: true,
            wait_for_voice: true,
            flash: true,
            inversion: true,
            tint: true,
            ripples: true,
            streaks: true,
            flare: true,
            rays: true,
            countdown: true,
            move_windows: true,
            max_secs: 300,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivationMode {
    #[default]
    Toggle,
    Hold,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PresetId {
    #[default]
    David,
    Smasher,
    Ghost,
    Custom,
}

impl PresetId {
    pub const ALL: [PresetId; 4] = [
        PresetId::David,
        PresetId::Smasher,
        PresetId::Ghost,
        PresetId::Custom,
    ];

    pub fn index(self) -> i32 {
        Self::ALL.iter().position(|p| *p == self).unwrap_or(0) as i32
    }

    pub fn from_index(i: i32) -> Self {
        usize::try_from(i)
            .ok()
            .and_then(|i| Self::ALL.get(i).copied())
            .unwrap_or_default()
    }

    pub fn label(self) -> &'static str {
        match self {
            PresetId::David => "David \u{2014} Neon Green",
            PresetId::Smasher => "Smasher \u{2014} Blood Red",
            PresetId::Ghost => "Ghost \u{2014} Electric Blue",
            PresetId::Custom => "Custom",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EffectToggles {
    pub ghosts: bool,
    pub trails: bool,
    pub glow: bool,
    pub particles: bool,
    pub shockwave: bool,
    pub warp_dash: bool,
    /// Dim and grey the desktop while active so only the cursor and its echoes keep colour.
    pub world_dim: bool,
    /// HDR pipeline: float render target, multi-pass bloom, tonemapping and dithering.
    pub hdr_bloom: bool,
    /// Film grain, chromatic aberration and lens warp on the black & white world.
    pub film_grade: bool,
    /// Light rays bursting out of the cursor on activation and dash.
    pub light_rays: bool,
    /// Air ripples (screen refraction) on activation.
    pub ripples: bool,
    /// Digital glitch (torn slices) on activation, dash and deactivation.
    pub glitch: bool,
    /// Rays + sparks when the cursor is flicked very fast.
    pub speed_bursts: bool,
}

impl Default for EffectToggles {
    fn default() -> Self {
        Self {
            ghosts: true,
            trails: true,
            glow: true,
            particles: true,
            shockwave: true,
            warp_dash: true,
            world_dim: true,
            hdr_bloom: true,
            film_grade: true,
            light_rays: true,
            ripples: true,
            glitch: true,
            speed_bursts: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomStyle {
    pub primary: String,
    pub secondary: String,
}

impl Default for CustomStyle {
    fn default() -> Self {
        Self {
            primary: "#B24CFF".into(),
            secondary: "#FF5CE1".into(),
        }
    }
}

/// How a full Sandevistan sound file is used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SoundMode {
    /// Cut into parts: start on activation, a loop that follows your movement, a crack for
    /// every dash and the power-down when it ends. The effect lasts as long as you want.
    #[default]
    Split,
    /// Play the whole file once; the effect ends when the sound ends.
    FullTrack,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    pub enabled: bool,
    /// 0..=100
    pub volume: u8,
    pub mode: SoundMode,
    /// Loudness of the movement layer relative to the master volume, 0..=100.
    pub move_volume: u8,
    /// Sandevistan parts that play (each can be switched off on its own).
    pub sv_start: bool,
    pub sv_move: bool,
    pub sv_dash: bool,
    pub sv_end: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            volume: 70,
            mode: SoundMode::Split,
            move_volume: 70,
            sv_start: true,
            sv_move: true,
            sv_dash: true,
            sv_end: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EnergySettings {
    pub enabled: bool,
    pub max: f32,
    pub drain_per_sec: f32,
    pub regen_per_sec: f32,
    pub dash_cost: f32,
    pub overheat_cooldown_secs: f32,
}

impl Default for EnergySettings {
    fn default() -> Self {
        Self {
            // Off: a toggled Sandevistan stays on until you turn it off.
            enabled: false,
            max: 100.0,
            drain_per_sec: 12.0,
            regen_per_sec: 18.0,
            dash_cost: 20.0,
            overheat_cooldown_secs: 4.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DashSettings {
    pub distance_px: u32,
    pub cooldown_ms: u32,
}

impl Default for DashSettings {
    fn default() -> Self {
        Self {
            distance_px: 350,
            cooldown_ms: 1000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PerformanceSettings {
    pub performance_mode: bool,
    /// Frame-rate cap used while effects are animating (30..=360). 0 = match the monitor.
    pub fps_cap: u32,
    pub max_particles: u32,
}

impl Default for PerformanceSettings {
    fn default() -> Self {
        Self {
            performance_mode: false,
            fps_cap: 0,
            max_particles: 260,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StartupSettings {
    pub start_with_windows: bool,
    pub start_minimized: bool,
}

impl Default for StartupSettings {
    fn default() -> Self {
        Self {
            start_with_windows: false,
            start_minimized: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Settings format version (used to migrate older files; missing = written by 0.1).
    #[serde(default)]
    pub version: u32,
    pub master_enabled: bool,
    pub mouse_boost: bool,
    /// Effect triggered by the activation shortcut.
    pub effect: EffectKind,
    pub the_world: TheWorldSettings,
    pub made_in_heaven: MadeInHeavenSettings,
    pub activation: Shortcut,
    pub activation_mode: ActivationMode,
    /// Swallow the activation/dash mouse buttons so they do not also trigger their normal
    /// action (middle-click autoscroll / paste, back/forward) while the system is enabled.
    pub consume_mouse_buttons: bool,
    /// While active, show the arrow instead of the hand / I-beam cursor (restored afterwards).
    pub keep_arrow_cursor: bool,
    pub dash: Shortcut,
    pub preset: PresetId,
    pub custom: CustomStyle,
    pub effects: EffectToggles,
    /// 0..=100
    pub intensity: u8,
    /// 1..=20
    pub ghost_count: u8,
    /// 0..=100
    pub ghost_opacity: u8,
    pub audio: AudioSettings,
    pub energy: EnergySettings,
    pub dash_settings: DashSettings,
    pub performance: PerformanceSettings,
    pub startup: StartupSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            master_enabled: false,
            mouse_boost: false,
            effect: EffectKind::Sandevistan,
            the_world: TheWorldSettings::default(),
            made_in_heaven: MadeInHeavenSettings::default(),
            activation: Shortcut::default_activation(),
            activation_mode: ActivationMode::Toggle,
            consume_mouse_buttons: true,
            keep_arrow_cursor: true,
            dash: Shortcut::default_dash(),
            preset: PresetId::David,
            custom: CustomStyle::default(),
            effects: EffectToggles::default(),
            intensity: 80,
            ghost_count: 14,
            ghost_opacity: 65,
            audio: AudioSettings::default(),
            energy: EnergySettings::default(),
            dash_settings: DashSettings::default(),
            performance: PerformanceSettings::default(),
            startup: StartupSettings::default(),
        }
    }
}

pub const SETTINGS_VERSION: u32 = 5;

impl Settings {
    /// Upgrade a settings file written by an older version.
    pub fn migrate(&mut self) {
        if self.version < 3 {
            // The energy limit made a toggled effect overheat after ~8 s; toggling now means
            // "on until I turn it off". The neon trail is part of the default look now.
            self.energy.enabled = false;
            self.effects.trails = true;
            self.version = 3;
        }
        if self.version < 4 {
            // 4 added The World; nothing to convert.
            self.version = 4;
        }
        if self.version < 5 {
            // MIH is additive. Serde supplies its defaults without touching old controls.
            self.version = 5;
        }
    }

    pub fn active_shortcut(&self) -> Shortcut {
        if self.effect == EffectKind::MadeInHeaven {
            self.made_in_heaven.activation
        } else {
            self.activation
        }
    }

    /// Clamp every value into its valid range and repair invalid shortcuts, so a
    /// hand-edited or corrupted file can never put the app in an invalid state.
    pub fn sanitize(&mut self) {
        let d = Settings::default();
        self.made_in_heaven.sanitize();
        self.intensity = self.intensity.min(100);
        self.ghost_count = self.ghost_count.clamp(1, 20);
        self.ghost_opacity = self.ghost_opacity.min(100);
        self.audio.volume = self.audio.volume.min(100);
        self.dash_settings.distance_px = self.dash_settings.distance_px.clamp(100, 1000);
        self.dash_settings.cooldown_ms = self.dash_settings.cooldown_ms.clamp(0, 10_000);
        if self.performance.fps_cap != 0 {
            self.performance.fps_cap = self.performance.fps_cap.clamp(30, 360);
        }
        self.performance.max_particles = self.performance.max_particles.clamp(0, 600);
        self.audio.move_volume = self.audio.move_volume.min(100);
        for value in &mut self.the_world.windups {
            *value = finite_or(*value, -1.0).clamp(-1.0, 15.0);
        }
        self.the_world.duration_secs = self.the_world.duration_secs.clamp(1, 9);
        self.the_world.max_secs = self.the_world.max_secs.clamp(5, 3600);
        let e = &mut self.energy;
        e.max = finite_or(e.max, 100.0).clamp(10.0, 1000.0);
        e.drain_per_sec = finite_or(e.drain_per_sec, 12.0).clamp(0.0, 200.0);
        e.regen_per_sec = finite_or(e.regen_per_sec, 18.0).clamp(0.1, 500.0);
        e.dash_cost = finite_or(e.dash_cost, 20.0).clamp(0.0, e.max);
        e.overheat_cooldown_secs = finite_or(e.overheat_cooldown_secs, 4.0).clamp(0.0, 60.0);
        if crate::settings::presets::parse_hex(&self.custom.primary).is_none() {
            self.custom.primary = d.custom.primary.clone();
        }
        if crate::settings::presets::parse_hex(&self.custom.secondary).is_none() {
            self.custom.secondary = d.custom.secondary.clone();
        }
        if self.activation.validate().is_err() {
            self.activation = d.activation;
        }
        if self.dash.validate().is_err() || self.dash == self.activation {
            self.dash = if d.dash == self.activation {
                Shortcut::default_activation()
            } else {
                d.dash
            };
        }
    }
}

fn finite_or(v: f32, fallback: f32) -> f32 {
    if v.is_finite() { v } else { fallback }
}

pub const APP_DIR: &str = "CursorFect";

pub fn settings_path() -> Option<PathBuf> {
    // %APPDATA%\CursorFect\settings.json  (Roaming)
    directories::BaseDirs::new().map(|d| d.config_dir().join(APP_DIR).join("settings.json"))
}

/// Parse settings JSON. Invalid JSON yields defaults; a field with an invalid type or
/// out-of-type value is dropped individually (that field falls back to its default)
/// instead of discarding the whole file. Never panics.
pub fn parse(text: &str) -> Settings {
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!("settings file is malformed ({err}); using defaults");
            return Settings::default();
        }
    };
    let mut settings = match serde_json::from_value::<Settings>(value.clone()) {
        Ok(s) => s,
        Err(_) => {
            let mut kept = serde_json::Map::new();
            if let serde_json::Value::Object(map) = &value {
                for (k, v) in map {
                    let mut probe = serde_json::Map::new();
                    probe.insert(k.clone(), v.clone());
                    if serde_json::from_value::<Settings>(serde_json::Value::Object(probe)).is_ok()
                    {
                        kept.insert(k.clone(), v.clone());
                    } else {
                        tracing::warn!("ignoring invalid settings field `{k}`");
                    }
                }
            }
            serde_json::from_value::<Settings>(serde_json::Value::Object(kept)).unwrap_or_default()
        }
    };
    settings.migrate();
    settings.sanitize();
    settings
}

pub fn load() -> Settings {
    let Some(path) = settings_path() else {
        return Settings::default();
    };
    match fs::read_to_string(&path) {
        Ok(text) => {
            let s = parse(&text);
            // Persist an upgraded (migrated) file right away.
            let on_disk = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v.get("version").and_then(|v| v.as_u64()))
                .unwrap_or(0);
            if on_disk < u64::from(SETTINGS_VERSION) {
                if let Err(e) = save(&s) {
                    tracing::warn!("could not save migrated settings: {e:#}");
                }
            }
            // First launch / loaded config never auto-enables anything dangerous:
            // master state is persisted as the user left it.
            s
        }
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("cannot read {}: {err}", path.display());
            }
            Settings::default()
        }
    }
}

/// Atomic write: write a temp file in the same directory, flush, then rename over.
pub fn save(settings: &Settings) -> anyhow::Result<()> {
    let path = settings_path().ok_or_else(|| anyhow::anyhow!("no config directory available"))?;
    save_to(settings, &path)
}

pub fn save_to(settings: &Settings, path: &Path) -> anyhow::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("settings path has no parent"))?;
    fs::create_dir_all(dir)?;
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(settings)?;
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_off_and_valid() {
        let s = Settings::default();
        assert!(!s.master_enabled, "first launch must default to OFF");
        assert_eq!(s.ghost_count, 14);
        assert_eq!(s.ghost_opacity, 65);
        assert_eq!(s.dash_settings.distance_px, 350);
        assert_eq!(s.dash_settings.cooldown_ms, 1000);
        assert!(s.activation.validate().is_ok());
        assert!(s.dash.validate().is_ok());
    }

    #[test]
    fn serialization_roundtrip() {
        let mut s = Settings::default();
        s.master_enabled = true;
        s.preset = PresetId::Smasher;
        s.audio.volume = 33;
        s.activation_mode = ActivationMode::Hold;
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn malformed_json_gives_defaults() {
        assert_eq!(parse("{ not json"), Settings::default());
        assert_eq!(parse(""), Settings::default());
    }

    #[test]
    fn partial_json_fills_defaults_and_clamps() {
        let s = parse(r#"{"ghost_count": 99, "intensity": 250, "audio": {"volume": 200}}"#);
        assert_eq!(s.ghost_count, 20);
        assert_eq!(s.intensity, 100);
        assert_eq!(s.audio.volume, 100);
        assert!(s.audio.enabled);
        assert_eq!(s.activation, Shortcut::default_activation());
    }

    #[test]
    fn one_bad_field_does_not_discard_the_rest() {
        // 999 overflows u8 and "yes" is not a bool: only those fields fall back to defaults.
        let s = parse(r#"{"ghost_count": 999, "master_enabled": "yes", "intensity": 33}"#);
        assert_eq!(s.ghost_count, 14);
        assert!(!s.master_enabled);
        assert_eq!(s.intensity, 33);
    }

    #[test]
    fn older_files_are_upgraded() {
        let s = parse(r#"{"version": 2, "energy": {"enabled": true}}"#);
        assert_eq!(s.version, SETTINGS_VERSION);
        assert!(
            !s.energy.enabled,
            "toggle must stay on forever after migrating"
        );
        // A deliberately chosen binding survives the migration.
        let k = parse(r#"{"activation": {"trigger": {"Key": 120}, "mods": {}}}"#);
        assert_eq!(
            k.activation.trigger,
            crate::input::shortcuts::Trigger::Key(120)
        );
        // Current files are left alone.
        let cur = parse(r#"{"version": 4, "energy": {"enabled": true}}"#);
        assert!(cur.energy.enabled);
    }

    #[test]
    fn invalid_shortcut_is_repaired() {
        let mut s = Settings::default();
        s.dash = s.activation;
        s.sanitize();
        assert_ne!(s.dash, s.activation);
    }

    #[test]
    fn atomic_save_and_reload() {
        let dir = std::env::temp_dir().join(format!("cursorfect-test-{}", std::process::id()));
        let path = dir.join("settings.json");
        let mut s = Settings {
            intensity: 42,
            effect: EffectKind::MadeInHeaven,
            ..Default::default()
        };
        s.made_in_heaven.max_multiplier = 5.2;
        s.made_in_heaven.real_acceleration = false;
        s.made_in_heaven.repeat_after_reset = true;
        save_to(&s, &path).unwrap();
        assert!(!path.with_extension("json.tmp").exists());
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(parse(&text), s);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn mih_is_additive_to_v110_settings_and_existing_shortcuts() {
        let old = r##"{"version":4,"effect":"TheWorld","activation":{"trigger":{"Key":120},"mods":{}},"dash":{"trigger":{"Mouse":"X2"},"mods":{"ctrl":true}},"the_world":{"dub":"Custom","duration_secs":7,"custom_primary":"#FF8844"},"audio":{"volume":31},"effects":{"ghosts":false}}"##;
        let settings = parse(old);
        assert_eq!(settings.version, SETTINGS_VERSION);
        assert_eq!(settings.effect, EffectKind::TheWorld);
        assert_eq!(
            settings.activation.trigger,
            crate::input::shortcuts::Trigger::Key(120)
        );
        assert_eq!(settings.the_world.dub, Dub::Custom);
        assert_eq!(settings.the_world.duration_secs, 7);
        assert_eq!(settings.audio.volume, 31);
        assert!(!settings.effects.ghosts);
        assert_eq!(settings.made_in_heaven, MadeInHeavenSettings::default());
        let activation = settings.activation;
        let dash = settings.dash;
        let mut settings = settings;
        settings.effect = EffectKind::MadeInHeaven;
        settings.made_in_heaven.activation =
            Shortcut::key(119, crate::input::shortcuts::Modifiers::NONE);
        assert_eq!(
            settings.active_shortcut(),
            settings.made_in_heaven.activation
        );
        assert_eq!(settings.activation, activation);
        assert_eq!(settings.dash, dash);
        settings.effect = EffectKind::Sandevistan;
        assert_eq!(settings.active_shortcut(), activation);
    }

    #[test]
    fn mih_settings_roundtrip_and_sanitize_every_control() {
        let mut settings = Settings {
            effect: EffectKind::MadeInHeaven,
            ..Default::default()
        };
        let mih = &mut settings.made_in_heaven;
        mih.mode = TimeStopMode::Timed;
        mih.duration_secs = 42.0;
        mih.max_multiplier = 5.5;
        mih.ramp_up_secs = 45.0;
        mih.deceleration_secs = 0.8;
        mih.reset_secs = 1.2;
        mih.real_acceleration = false;
        mih.repeat_after_reset = true;
        mih.afterimages = false;
        mih.motion_trails = false;
        mih.speed_particles = false;
        mih.heaven_aura = false;
        mih.reset_flash = false;
        mih.indicator = false;
        mih.intensity = 33;
        mih.sfx = false;
        mih.volume = 24;
        assert_eq!(parse(&serde_json::to_string(&settings).unwrap()), settings);
        let mut mih = settings.made_in_heaven;
        mih.max_multiplier = f32::NAN;
        mih.ramp_up_secs = f32::INFINITY;
        mih.deceleration_secs = -1.0;
        mih.reset_secs = 900.0;
        mih.duration_secs = 0.0;
        mih.volume = 200;
        mih.intensity = 200;
        mih.sanitize();
        assert_eq!(mih.max_multiplier, 4.0);
        assert_eq!(mih.ramp_up_secs, 20.0);
        assert_eq!(mih.deceleration_secs, 0.2);
        assert_eq!(mih.reset_secs, 5.0);
        assert_eq!(mih.duration_secs, 1.0);
        assert_eq!(mih.volume, 100);
        assert_eq!(mih.intensity, 100);
    }
}
