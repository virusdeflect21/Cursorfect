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
        SysState::Awakening
        | SysState::TimeAccelerating
        | SysState::HeavenAscended
        | SysState::UniverseReset
        | SysState::Decelerating => Color::from_rgb_u8(0x60, 0xE7, 0xFF),
    }
}

/// Push every setting into the window's properties.
pub fn apply_settings(ui: &MainWindow, s: &Settings) {
    ui.set_master_on(s.master_enabled);
    ui.set_mouse_boost(s.mouse_boost);
    ui.set_effect(match s.effect {
        EffectKind::Sandevistan => 0,
        EffectKind::TheWorld => 1,
        EffectKind::MadeInHeaven => 2,
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
    ui.set_tw_windup(t.windups[t.dub.index() as usize]);
    ui.set_tw_custom_primary(t.custom_primary.clone().into());
    ui.set_tw_custom_secondary(t.custom_secondary.clone().into());
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
    let mih = &s.made_in_heaven;
    ui.set_mih_enabled(mih.enabled);
    ui.set_mih_mode(match mih.mode {
        TimeStopMode::Hold => 0,
        TimeStopMode::Toggle => 1,
        TimeStopMode::Timed => 2,
    });
    ui.set_mih_duration(mih.duration_secs);
    ui.set_mih_real(mih.real_acceleration);
    ui.set_mih_repeat(mih.repeat_after_reset);
    ui.set_mih_maximum(mih.max_multiplier);
    ui.set_mih_ramp(mih.ramp_up_secs);
    ui.set_mih_deceleration(mih.deceleration_secs);
    ui.set_mih_reset_duration(mih.reset_secs);
    ui.set_mih_ghosts(mih.afterimages);
    ui.set_mih_trails(mih.motion_trails);
    ui.set_mih_particles(mih.speed_particles);
    ui.set_mih_aura(mih.heaven_aura);
    ui.set_mih_flash(mih.reset_flash);
    ui.set_mih_indicator(mih.indicator);
    ui.set_mih_intensity(f32::from(mih.intensity));
    ui.set_mih_sfx(mih.sfx);
    ui.set_mih_volume(f32::from(mih.volume));
    ui.set_fx_ripples(s.effects.ripples);
    ui.set_fx_glitch(s.effects.glitch);
    ui.set_fx_bursts(s.effects.speed_bursts);
    ui.set_activation_text(s.active_shortcut().display().into());
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
    s.effect = match ui.get_effect() {
        1 => EffectKind::TheWorld,
        2 => EffectKind::MadeInHeaven,
        _ => EffectKind::Sandevistan,
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
    let old_dub = t.dub;
    let new_dub = Dub::from_index(ui.get_tw_dub());
    if old_dub == new_dub {
        t.windups[new_dub.index() as usize] = ui.get_tw_windup().clamp(-1.0, 15.0);
    }
    t.dub = new_dub;
    t.custom_primary = ui.get_tw_custom_primary().to_string();
    t.custom_secondary = ui.get_tw_custom_secondary().to_string();
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
    let mih = &mut s.made_in_heaven;
    mih.enabled = ui.get_mih_enabled();
    mih.mode = match ui.get_mih_mode() {
        0 => TimeStopMode::Hold,
        2 => TimeStopMode::Timed,
        _ => TimeStopMode::Toggle,
    };
    mih.duration_secs = ui.get_mih_duration();
    mih.real_acceleration = ui.get_mih_real();
    mih.repeat_after_reset = ui.get_mih_repeat();
    mih.max_multiplier = ui.get_mih_maximum();
    mih.ramp_up_secs = ui.get_mih_ramp();
    mih.deceleration_secs = ui.get_mih_deceleration();
    mih.reset_secs = ui.get_mih_reset_duration();
    mih.afterimages = ui.get_mih_ghosts();
    mih.motion_trails = ui.get_mih_trails();
    mih.speed_particles = ui.get_mih_particles();
    mih.heaven_aura = ui.get_mih_aura();
    mih.reset_flash = ui.get_mih_flash();
    mih.indicator = ui.get_mih_indicator();
    mih.intensity = ui.get_mih_intensity().round().clamp(0.0, 100.0) as u8;
    mih.sfx = ui.get_mih_sfx();
    mih.volume = ui.get_mih_volume().round().clamp(0.0, 100.0) as u8;
    mih.sanitize();
    s.mouse_boost = ui.get_mouse_boost();
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
    // The two legacy effects share their existing shortcut; MIH has its own.
    // Update the displayed binding immediately when the effect chip changes.
    ui.set_activation_text(s.active_shortcut().display().into());
    s
}

/// Native Slint diagnostic: round-trip controls for all three effects, verify
/// the real tray menu, and save this application's own UI snapshot. No input
/// hooks/audio engine are started and the user's configuration is not loaded/saved.
pub fn self_test(dir: &std::path::Path) -> anyhow::Result<()> {
    use slint::ComponentHandle;
    std::fs::create_dir_all(dir)?;
    let ui = MainWindow::new()?;
    ui.set_version(env!("CARGO_PKG_VERSION").into());
    let mut settings = Settings {
        activation: crate::input::shortcuts::Shortcut::key(
            120,
            crate::input::shortcuts::Modifiers::NONE,
        ),
        made_in_heaven: crate::settings::config::MadeInHeavenSettings {
            activation: crate::input::shortcuts::Shortcut::key(
                119,
                crate::input::shortcuts::Modifiers::NONE,
            ),
            max_multiplier: 3.7,
            ramp_up_secs: 36.0,
            deceleration_secs: 0.8,
            volume: 42,
            ..Default::default()
        },
        ..Default::default()
    };
    for effect in [
        EffectKind::Sandevistan,
        EffectKind::TheWorld,
        EffectKind::MadeInHeaven,
    ] {
        settings.effect = effect;
        apply_settings(&ui, &settings);
        let read = read_settings(&ui, &settings);
        anyhow::ensure!(read == settings, "UI round-trip mismatch for {effect:?}");
        anyhow::ensure!(
            ui.get_activation_text().as_str() == settings.active_shortcut().display(),
            "UI displayed the wrong shortcut for {effect:?}"
        );
    }
    // Exercise selector feedback with distinct bindings, then show the original
    // controls/style with MIH selected. This is not a desktop screenshot.
    settings.effect = EffectKind::Sandevistan;
    apply_settings(&ui, &settings);
    ui.set_effect(2);
    let selected = read_settings(&ui, &settings);
    anyhow::ensure!(
        selected.effect == EffectKind::MadeInHeaven && selected.activation == settings.activation,
        "selecting MIH changed the legacy shortcut"
    );
    anyhow::ensure!(
        ui.get_activation_text().as_str() == selected.made_in_heaven.activation.display(),
        "selector did not refresh MIH's binding"
    );
    ui.set_mih_parts_text("Activation: made-in-heaven-summon-start.mp3\nAcceleration: time-accelerate.mp3\nUniverse Reset: universe-reset.mp3\nOptional maximum/deactivation: silent if absent".into());
    apply_status(&ui, SysState::Offline, 100.0, 0);
    let tray = crate::platform::tray::Tray::create(|_| {})?;
    tray.self_test()?;
    let result = std::rc::Rc::new(std::cell::RefCell::new(None));
    let captured = result.clone();
    let weak = ui.as_weak();
    let path = dir.join("settings-mih.bmp");
    slint::Timer::single_shot(std::time::Duration::from_millis(350), move || {
        let capture = (|| -> anyhow::Result<()> {
            let ui = weak
                .upgrade()
                .ok_or_else(|| anyhow::anyhow!("diagnostic window disappeared"))?;
            let image = ui.window().take_snapshot()?;
            let width = image.width();
            let height = image.height();
            let row = (width as usize * 3).div_ceil(4) * 4;
            let mut bmp = vec![0u8; 54 + row * height as usize];
            let length = bmp.len() as u32;
            bmp[0..2].copy_from_slice(b"BM");
            bmp[2..6].copy_from_slice(&length.to_le_bytes());
            bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
            bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
            bmp[18..22].copy_from_slice(&width.to_le_bytes());
            bmp[22..26].copy_from_slice(&height.to_le_bytes());
            bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
            bmp[28..30].copy_from_slice(&24u16.to_le_bytes());
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let source = (y * width as usize + x) * 4;
                    let target = 54 + (height as usize - 1 - y) * row + x * 3;
                    let rgba = &image.as_bytes()[source..source + 4];
                    bmp[target..target + 3].copy_from_slice(&[rgba[2], rgba[1], rgba[0]]);
                }
            }
            std::fs::write(path, bmp)?;
            Ok(())
        })();
        *captured.borrow_mut() = Some(capture);
        let _ = slint::quit_event_loop();
    });
    ui.show()?;
    slint::run_event_loop_until_quit()?;
    let _ = ui.hide();
    drop(tray);
    result
        .borrow_mut()
        .take()
        .ok_or_else(|| anyhow::anyhow!("UI diagnostic did not complete"))??;
    std::fs::write(
        dir.join("ui-test.txt"),
        "Native UI/tray diagnostic passed: all three effect settings round-trip, independent shortcuts, immediate selector feedback, MIH tray activation/deactivation labels and master state. No user settings changed.",
    )?;
    Ok(())
}
