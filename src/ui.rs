//! Glue between the Slint settings window and [`Settings`].

use slint::Color;

use crate::audio::dubs::Dub;
use crate::settings::config::{
    ActivationMode, EffectKind, PresetId, Settings, SoundMode, TimeStopMode,
};
use crate::settings::presets::{Rgb, parse_hex, style_for};
use crate::state::SysState;

slint::include_modules!();

/// Frame-rate choices of the "Frame rate" combo box (0 = match the monitor).
const FPS_CHOICES: [u32; 6] = [0, 60, 120, 144, 165, 240];

fn fps_index(fps: u32) -> i32 {
    FPS_CHOICES
        .iter()
        .position(|f| *f == fps)
        .or_else(|| {
            // Closest listed value for hand-edited caps.
            FPS_CHOICES
                .iter()
                .enumerate()
                .skip(1)
                .min_by_key(|(_, f)| f.abs_diff(fps))
                .map(|(i, _)| i)
        })
        .unwrap_or(0) as i32
}

fn color(c: Rgb) -> Color {
    Color::from_rgb_u8(
        (c[0] * 255.0).round() as u8,
        (c[1] * 255.0).round() as u8,
        (c[2] * 255.0).round() as u8,
    )
}

pub fn status_color(s: SysState) -> Color {
    match s {
        SysState::Offline => Color::from_rgb_u8(0x8B, 0x94, 0x9C),
        SysState::Ready => Color::from_rgb_u8(0x3B, 0x82, 0xF6),
        SysState::Active => Color::from_rgb_u8(0x22, 0xC5, 0x5E),
        SysState::Overheated => Color::from_rgb_u8(0xFF, 0x7A, 0x59),
        SysState::TimeStopped => Color::from_rgb_u8(0xE9, 0xC4, 0x6A),
    }
}

/// Push every setting into the window's properties.
pub fn apply_settings(ui: &MainWindow, s: &Settings) {
    ui.set_master_on(s.master_enabled);
    ui.set_effect(match s.effect {
        EffectKind::Sandevistan => 0,
        EffectKind::TheWorld => 1,
    });
    ui.set_sv_start(s.audio.sv_start);
    ui.set_sv_move(s.audio.sv_move);
    ui.set_sv_dash(s.audio.sv_dash);
    ui.set_sv_end(s.audio.sv_end);
    let t = &s.the_world;
    ui.set_tw_mode(match t.mode {
        TimeStopMode::Hold => 0,
        TimeStopMode::Toggle => 1,
        TimeStopMode::Timed => 2,
    });
    ui.set_tw_duration(f32::from(t.duration_secs));
    ui.set_tw_dub(t.dub.index());
    ui.set_tw_voice(t.voice);
    ui.set_tw_sfx(t.stop_sfx);
    ui.set_tw_ticks(t.ticks);
    ui.set_tw_rvoice(t.resume_voice);
    ui.set_tw_rsfx(t.resume_sfx);
    ui.set_tw_wait(t.wait_for_voice);
    ui.set_tw_flash(t.flash);
    ui.set_tw_invert(t.inversion);
    ui.set_tw_tint(t.tint);
    ui.set_tw_ripples(t.ripples);
    ui.set_tw_countdown(t.countdown);
    ui.set_tw_move(t.move_windows);
    ui.set_tw_streaks(t.streaks);
    ui.set_tw_flare(t.flare);
    ui.set_tw_rays(t.rays);
    ui.set_fx_ripples(s.effects.ripples);
    ui.set_fx_glitch(s.effects.glitch);
    ui.set_fx_bursts(s.effects.speed_bursts);
    ui.set_activation_text(s.activation.display().into());
    ui.set_dash_text(s.dash.display().into());
    ui.set_hold_mode(s.activation_mode == ActivationMode::Hold);
    ui.set_consume_mouse(s.consume_mouse_buttons);
    ui.set_keep_arrow(s.keep_arrow_cursor);
    ui.set_fx_ghosts(s.effects.ghosts);
    ui.set_fx_trails(s.effects.trails);
    ui.set_fx_glow(s.effects.glow);
    ui.set_fx_particles(s.effects.particles);
    ui.set_fx_shock(s.effects.shockwave);
    ui.set_fx_dash(s.effects.warp_dash);
    ui.set_fx_dim(s.effects.world_dim);
    ui.set_fx_bloom(s.effects.hdr_bloom);
    ui.set_fx_film(s.effects.film_grade);
    ui.set_fx_rays(s.effects.light_rays);
    ui.set_intensity(f32::from(s.intensity));
    ui.set_ghost_count(f32::from(s.ghost_count));
    ui.set_ghost_opacity(f32::from(s.ghost_opacity));
    ui.set_dash_distance(s.dash_settings.distance_px as f32);
    ui.set_dash_cooldown(s.dash_settings.cooldown_ms as f32);
    ui.set_sfx_on(s.audio.enabled);
    ui.set_volume(f32::from(s.audio.volume));
    ui.set_move_volume(f32::from(s.audio.move_volume));
    ui.set_sound_mode(match s.audio.mode {
        SoundMode::Split => 0,
        SoundMode::FullTrack => 1,
    });
    ui.set_preset(s.preset.index());
    ui.set_custom_primary(s.custom.primary.clone().into());
    ui.set_custom_secondary(s.custom.secondary.clone().into());
    apply_preview_colors(ui, s);
    ui.set_energy_on(s.energy.enabled);
    ui.set_perf_mode(s.performance.performance_mode);
    ui.set_fps_index(fps_index(s.performance.fps_cap));
    ui.set_startup_on(s.startup.start_with_windows);
}

fn apply_preview_colors(ui: &MainWindow, s: &Settings) {
    let st = style_for(s);
    ui.set_primary_color(color(st.primary));
    ui.set_secondary_color(color(st.secondary));
}

pub fn apply_status(ui: &MainWindow, state: SysState, energy_pct: f32, recording: u8) {
    ui.set_status_text(state.label().into());
    ui.set_status_color(status_color(state));
    ui.set_energy_pct(energy_pct);
    ui.set_recording(i32::from(recording));
}

/// Read the window's controls into a new [`Settings`], keeping everything the UI
/// does not edit (shortcuts, energy tuning, ...) from `base`.
pub fn read_settings(ui: &MainWindow, base: &Settings) -> Settings {
    let mut s = base.clone();
    s.master_enabled = ui.get_master_on();
    s.effect = if ui.get_effect() == 1 {
        EffectKind::TheWorld
    } else {
        EffectKind::Sandevistan
    };
    s.audio.sv_start = ui.get_sv_start();
    s.audio.sv_move = ui.get_sv_move();
    s.audio.sv_dash = ui.get_sv_dash();
    s.audio.sv_end = ui.get_sv_end();
    let t = &mut s.the_world;
    t.mode = match ui.get_tw_mode() {
        0 => TimeStopMode::Hold,
        2 => TimeStopMode::Timed,
        _ => TimeStopMode::Toggle,
    };
    t.duration_secs = ui.get_tw_duration().round().clamp(1.0, 9.0) as u8;
    t.dub = Dub::from_index(ui.get_tw_dub());
    t.voice = ui.get_tw_voice();
    t.stop_sfx = ui.get_tw_sfx();
    t.ticks = ui.get_tw_ticks();
    t.resume_voice = ui.get_tw_rvoice();
    t.resume_sfx = ui.get_tw_rsfx();
    t.wait_for_voice = ui.get_tw_wait();
    t.flash = ui.get_tw_flash();
    t.inversion = ui.get_tw_invert();
    t.tint = ui.get_tw_tint();
    t.ripples = ui.get_tw_ripples();
    t.countdown = ui.get_tw_countdown();
    t.move_windows = ui.get_tw_move();
    t.streaks = ui.get_tw_streaks();
    t.flare = ui.get_tw_flare();
    t.rays = ui.get_tw_rays();
    s.effects.ripples = ui.get_fx_ripples();
    s.effects.glitch = ui.get_fx_glitch();
    s.effects.speed_bursts = ui.get_fx_bursts();
    s.activation_mode = if ui.get_hold_mode() {
        ActivationMode::Hold
    } else {
        ActivationMode::Toggle
    };
    s.consume_mouse_buttons = ui.get_consume_mouse();
    s.keep_arrow_cursor = ui.get_keep_arrow();
    s.effects.ghosts = ui.get_fx_ghosts();
    s.effects.trails = ui.get_fx_trails();
    s.effects.glow = ui.get_fx_glow();
    s.effects.particles = ui.get_fx_particles();
    s.effects.shockwave = ui.get_fx_shock();
    s.effects.warp_dash = ui.get_fx_dash();
    s.effects.world_dim = ui.get_fx_dim();
    s.effects.hdr_bloom = ui.get_fx_bloom();
    s.effects.film_grade = ui.get_fx_film();
    s.effects.light_rays = ui.get_fx_rays();
    s.intensity = ui.get_intensity().round().clamp(0.0, 100.0) as u8;
    s.ghost_count = ui.get_ghost_count().round().clamp(1.0, 20.0) as u8;
    s.ghost_opacity = ui.get_ghost_opacity().round().clamp(0.0, 100.0) as u8;
    s.dash_settings.distance_px = ui.get_dash_distance().round().clamp(100.0, 1000.0) as u32;
    s.dash_settings.cooldown_ms = ui.get_dash_cooldown().round().clamp(0.0, 5000.0) as u32;
    s.audio.enabled = ui.get_sfx_on();
    s.audio.volume = ui.get_volume().round().clamp(0.0, 100.0) as u8;
    s.audio.move_volume = ui.get_move_volume().round().clamp(0.0, 100.0) as u8;
    s.audio.mode = if ui.get_sound_mode() == 1 {
        SoundMode::FullTrack
    } else {
        SoundMode::Split
    };
    s.preset = PresetId::from_index(ui.get_preset());
    // Only accept valid hex colors; invalid text keeps the previous valid color.
    let p = ui.get_custom_primary().to_string();
    if parse_hex(&p).is_some() {
        s.custom.primary = p;
    }
    let sec = ui.get_custom_secondary().to_string();
    if parse_hex(&sec).is_some() {
        s.custom.secondary = sec;
    }
    s.energy.enabled = ui.get_energy_on();
    s.performance.performance_mode = ui.get_perf_mode();
    s.performance.fps_cap = usize::try_from(ui.get_fps_index())
        .ok()
        .and_then(|i| FPS_CHOICES.get(i).copied())
        .unwrap_or(0);
    s.startup.start_with_windows = ui.get_startup_on();
    apply_preview_colors(ui, &s);
    s
}
