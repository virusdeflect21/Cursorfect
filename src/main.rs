// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod effects;
mod icon;
mod input;
mod overlay;
mod platform;
mod settings;
mod state;
mod ui;

use std::cell::RefCell;
use std::sync::mpsc;

use anyhow::Context;
use slint::ComponentHandle;

use app::{Cmd, EngineArgs, Msg, UiMsg};
use input::hooks::{Bindings, HookShared, HookThread, RECORD_NONE};
use platform::native;
use platform::tray::{Tray, TrayAction};
use settings::config::{self, Settings};
use ui::MainWindow;

thread_local! {
    // UI-thread-only state: the tray icon and the last settings snapshot known to the UI.
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
    static BASE: RefCell<Settings> = RefCell::new(Settings::default());
    static ENGINE_STATE: RefCell<crate::state::SysState> = const { RefCell::new(crate::state::SysState::Offline) };
}

fn init_logging() {
    use std::sync::Mutex;
    let file = config::settings_path()
        .and_then(|p| p.parent().map(|d| d.join("cursorfect.log")))
        .and_then(|p| {
            let _ = std::fs::create_dir_all(p.parent()?);
            // Keep the log small: start fresh once it grows past 1 MB.
            if std::fs::metadata(&p)
                .map(|m| m.len() > 1_000_000)
                .unwrap_or(false)
            {
                let _ = std::fs::remove_file(&p);
            }
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .ok()
        });
    let builder = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_target(false)
        .with_max_level(if cfg!(debug_assertions) {
            tracing::Level::DEBUG
        } else {
            tracing::Level::INFO
        });
    match file {
        Some(f) => builder.with_writer(Mutex::new(f)).init(),
        None => builder.init(),
    }
    std::panic::set_hook(Box::new(|info| { native::boost_mouse_speed(false); tracing::error!("panic: {info}"); }));
}

fn show_window(ui: &MainWindow) {
    let _ = ui.show();
    ui.window().set_minimized(false);
}

fn main() -> anyhow::Result<()> {
    init_logging();
    native::set_dpi_awareness();
    // Visual self-test: `cursorfect.exe --render-test <folder>` renders frames to .bmp files.
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--demo-shots") {
        let arg = |k: usize| args.get(i + k).map(std::path::PathBuf::from);
        let (Some(bg), Some(bg2), Some(out)) = (arg(1), arg(2), arg(3)) else {
            anyhow::bail!("usage: --demo-shots <bg.bmp> <bg_moved.bmp> <folder>");
        };
        return app::demo_shots(&bg, &bg2, &out);
    }
    if let Some(i) = args.iter().position(|a| a == "--render-test-tw") {
        let dir = args
            .get(i + 1)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("cursorfect-render-test"));
        return app::render_test_tw(&dir, args.iter().any(|a| a == "--measure"));
    }
    if let Some(i) = args.iter().position(|a| a == "--render-test") {
        let dir = args
            .get(i + 1)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("cursorfect-render-test"));
        return app::render_test(&dir);
    }
    let display_version = env!("CARGO_PKG_VERSION").replace('+', ".");
    let running_file = config::running_version_path();
    let Some(_instance) =
        native::SingleInstance::acquire_or_show(&display_version, running_file.as_deref())
    else {
        tracing::info!("another CursorFect instance is running; exiting");
        return Ok(());
    };
    let _timer = native::TimerResolution::new();
    native::restore_system_cursors();
    tracing::info!("CursorFect {display_version} starting");

    let settings = config::load();
    let start_minimized =
        std::env::args().any(|a| a == "--minimized") || settings.startup.start_minimized;

    let (tx, rx) = mpsc::channel::<Msg>();
    let hooks = HookShared::new(
        tx.clone(),
        Bindings {
            activation: settings.activation,
            dash: settings.dash,
            consume_mouse: settings.consume_mouse_buttons,
        },
    );
    // A failed hook install leaves the app usable (settings window, tray), just without global shortcuts.
    let mut hook_thread = match HookThread::spawn(hooks.clone()) {
        Ok(h) => Some(h),
        Err(e) => {
            tracing::error!("global input hooks unavailable: {e:#}");
            None
        }
    };

    let ui = MainWindow::new().context("creating settings window")?;
    ui.set_version(display_version.into());
    ui.set_fps_auto_text(format!("Auto ({} Hz monitor)", native::max_refresh_rate()).into());
    ui::apply_settings(&ui, &settings);
    ui::apply_status(&ui, state::SysState::Offline, 100.0, RECORD_NONE);
    if hook_thread.is_none() {
        ui.set_notice(
            "Global input hooks could not be installed; shortcuts are unavailable.".into(),
        );
    }
    BASE.with(|b| *b.borrow_mut() = settings.clone());

    // ---- engine -> UI
    let weak = ui.as_weak();
    let ui_sink: app::UiSink = Box::new(move |msg: UiMsg| {
        let _ = weak.upgrade_in_event_loop(move |ui| match msg {
            UiMsg::Status {
                state,
                energy_pct,
                recording,
            } => {
                ui::apply_status(&ui, state, energy_pct, recording);
                ENGINE_STATE.with(|s| *s.borrow_mut() = state);
                let preset = BASE.with(|b| b.borrow().preset);
                TRAY.with(|t| {
                    if let Some(t) = t.borrow().as_ref() {
                        t.update(state, preset);
                    }
                });
            }
            UiMsg::Settings(s) => {
                ui::apply_settings(&ui, &s);
                let state = ENGINE_STATE.with(|s| *s.borrow());
                TRAY.with(|t| {
                    if let Some(t) = t.borrow().as_ref() {
                        t.update(state, s.preset);
                    }
                });
                BASE.with(|b| *b.borrow_mut() = *s);
            }
            UiMsg::Notice(text) => ui.set_notice(text.into()),
            UiMsg::SoundInfo(text) => ui.set_parts_text(text.into()),
        });
    });

    let engine = app::spawn(EngineArgs {
        settings: settings.clone(),
        rx,
        tx: tx.clone(),
        hooks: hooks.clone(),
        ui: ui_sink,
    })
    .context("starting engine")?;

    // ---- UI -> engine
    {
        let tx = tx.clone();
        let weak = ui.as_weak();
        ui.on_changed(move || {
            if let Some(ui) = weak.upgrade() {
                let new = BASE.with(|b| {
                    let s = ui::read_settings(&ui, &b.borrow());
                    *b.borrow_mut() = s.clone();
                    s
                });
                let _ = tx.send(Msg::Cmd(Cmd::SetSettings(Box::new(new))));
            }
        });
    }
    {
        let tx = tx.clone();
        let weak = ui.as_weak();
        ui.on_record(move |which| {
            let which = which.clamp(0, 2) as u8;
            let currently = weak.upgrade().map(|u| u.get_recording()).unwrap_or(0);
            let target = if currently == i32::from(which) {
                RECORD_NONE
            } else {
                which
            };
            let _ = tx.send(Msg::Cmd(Cmd::Record(target)));
        });
    }
    {
        let tx = tx.clone();
        ui.on_preview(move |which| {
            use audio::engine::Part;
            let part = match which {
                0 => Part::Start,
                1 => Part::Move,
                2 => Part::Dash,
                3 => Part::End,
                _ => Part::Full,
            };
            let _ = tx.send(Msg::Cmd(Cmd::Preview(part)));
        });
    }
    {
        let tx = tx.clone();
        ui.on_tw_preview(move |which| {
            let _ = tx.send(Msg::Cmd(Cmd::TwPreview(which.clamp(0, 2) as u8)));
        });
    }
    {
        let tx = tx.clone();
        ui.on_reset_defaults(move || {
            let _ = tx.send(Msg::Cmd(Cmd::ResetDefaults));
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_minimize_tray(move || {
            if let Some(ui) = weak.upgrade() {
                let _ = ui.hide();
            }
        });
    }
    {
        let weak = ui.as_weak();
        let tx = tx.clone();
        ui.on_choose_custom_sound(move |which| {
            use std::os::windows::process::CommandExt;
            let Some(name) = ["custom_stop.mp3", "custom_tick.mp3", "custom_resume.mp3"].get(which as usize) else { return; };
            let result = std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-STA", "-Command", "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.OpenFileDialog; $d.Filter = 'MP3 audio (*.mp3)|*.mp3'; $d.Title = 'Choose a sound'; if ($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8; [Console]::Write($d.FileName) }"])
                .creation_flags(0x08000000)
                .output();
            let selected = match result { Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_owned(), _ => String::new() };
            if selected.is_empty() { return; }
            let result = audio::engine::sound_dirs().into_iter().next().ok_or_else(|| "Sound folder unavailable".to_string()).and_then(|dir| {
                let dir = dir.join("theworld");
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                std::fs::copy(&selected, dir.join(name)).map_err(|e| e.to_string())?;
                Ok(())
            });
            if let Some(ui) = weak.upgrade() {
                match result {
                    Ok(()) => { let _ = tx.send(Msg::Cmd(Cmd::ReloadSounds)); ui.set_notice(format!("Loaded {}", selected).into()); }
                    Err(e) => ui.set_notice(format!("Could not load sound: {e}").into()),
                }
            }
        });
    }
    // Sounds folder: created up-front so users can drop their .mp3 files in.
    let sounds_dir = audio::engine::sound_dirs().into_iter().next();
    if let Some(d) = sounds_dir.as_ref() {
        let _ = std::fs::create_dir_all(d);
        ui.set_sounds_dir(d.display().to_string().into());
    }
    ui.on_open_sounds(move || {
        if let Some(d) = sounds_dir.as_ref() {
            let _ = std::fs::create_dir_all(d);
            let _ = std::process::Command::new("explorer").arg(d).spawn();
        }
    });
    ui.window().on_close_requested(|| {
        if !TRAY.with(|t| t.borrow().is_some()) {
            let _ = slint::quit_event_loop();
        }
        slint::CloseRequestResponse::HideWindow
    });

    // ---- tray
    {
        let tx = tx.clone();
        let weak = ui.as_weak();
        let tray = Tray::create(move |action| match action {
            TrayAction::OpenSettings => {
                let weak = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        show_window(&ui);
                    }
                });
            }
            TrayAction::Exit => {
                let _ = slint::invoke_from_event_loop(|| {
                    let _ = slint::quit_event_loop();
                });
            }
            other => {
                let _ = tx.send(Msg::Cmd(Cmd::Tray(other)));
            }
        });
        match tray {
            Ok(t) => {
                t.update(state::SysState::Offline, settings.preset);
                TRAY.with(|slot| *slot.borrow_mut() = Some(t));
            }
            Err(e) => tracing::warn!("tray icon unavailable: {e:#}"),
        }
    }

    let weak = ui.as_weak();
    native::listen_for_show(move || {
        let weak = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() { show_window(&ui); }
        });
    });
    let tray_ok = TRAY.with(|t| t.borrow().is_some());
    if start_minimized && tray_ok {
        tracing::info!("starting minimized to tray");
    } else {
        ui.show().context("showing settings window")?;
    }
    slint::run_event_loop_until_quit().context("UI event loop")?;

    // ---- orderly shutdown: effects off, hooks released, overlay destroyed, settings saved.
    tracing::info!("shutting down");
    let _ = tx.send(Msg::Cmd(Cmd::Shutdown));
    let _ = engine.join();
    if let Some(h) = hook_thread.as_mut() {
        h.shutdown();
    }
    TRAY.with(|t| *t.borrow_mut() = None);
    native::restore_system_cursors();
    native::boost_mouse_speed(false);
    drop(ui);
    Ok(())
}
