//! The engine: one thread that owns the state machine, cursor tracking, effects,
//! overlay and persistence. Input hooks, the UI and the tray talk to it through a
//! single channel; it renders only while something is animating.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use glam::Vec2;

use crate::audio::dubs;
use crate::audio::engine::{AudioCmd, AudioConfig, AudioHandle, Part, theworld_file};
use crate::audio::synthesis::Sfx;
use crate::effects::the_world::{TimeStop, TwEvent, TwOptions};
use crate::effects::{Batch, Config, Effects};
use crate::input::cursor::{CursorTracker, Rect, dash_endpoint};
use crate::input::shortcuts::{MouseButton, Shortcut, Trigger};
use crate::input::hooks::{
    Bindings, HookShared, InputEvent, RECORD_ACTIVATION, RECORD_DASH, RECORD_NONE,
};
use crate::overlay::Overlay;
use crate::overlay::renderer::{FrameResult, Grade};
use crate::overlay::window::take_display_changed;
use crate::platform::native;
use crate::platform::watchdog::Watchdog;
use crate::platform::wm::{self, WinRect};
use crate::platform::tray::TrayAction;
use crate::settings::config::{self, ActivationMode, EffectKind, Settings, SoundMode, TimeStopMode};
use crate::settings::presets::{SfxProfile, style_for};
use crate::state::{Machine, SysState, Transition};

/// Everything the engine can be told.
pub enum Msg {
    Input(InputEvent),
    Cmd(Cmd),
}

pub enum Cmd {
    /// The UI edited settings (full snapshot).
    SetSettings(Box<Settings>),
    Record(u8),
    /// Preview one part of the sound (re-scans the sound folder first).
    Preview(Part),
    /// Preview The World's sounds: 0 = time stop, 1 = resume, 2 = tick.
    TwPreview(u8),
    ResetDefaults,
    Tray(TrayAction),
    Shutdown,
}

/// Everything the engine tells the UI.
pub enum UiMsg {
    Status {
        state: SysState,
        energy_pct: f32,
        recording: u8,
    },
    /// Settings changed from somewhere other than the UI (tray, emergency, reset, recording).
    Settings(Box<Settings>),
    Notice(String),
    /// Which sounds are loaded and how they were cut (shown under the preview buttons).
    SoundInfo(String),
}

pub type UiSink = Box<dyn Fn(UiMsg) + Send>;

/// Build the effect configuration for the given settings.
pub fn effect_config(s: &Settings) -> Config {
    Config {
        toggles: s.effects.clone(),
        style: style_for(s),
        intensity: f32::from(s.intensity) / 100.0,
        ghost_count: usize::from(s.ghost_count),
        ghost_opacity: f32::from(s.ghost_opacity) / 100.0,
        max_particles: s.performance.max_particles as usize,
        performance_mode: s.performance.performance_mode,
        scale: 1.0,
        cursor: None,
        world_supported: false,
        ramp_in: 0.6,
        ramp_out: 0.45,
        hdr: false,
    }
}

fn audio_config(s: &Settings) -> AudioConfig {
    AudioConfig {
        enabled: s.audio.enabled,
        volume: s.audio.volume,
        move_volume: s.audio.move_volume,
        mode: s.audio.mode,
        profile: style_for(s).sfx,
        sv_parts: sv_parts(s),
    }
}

fn sv_parts(s: &Settings) -> [bool; 4] {
    [s.audio.sv_start, s.audio.sv_move, s.audio.sv_dash, s.audio.sv_end]
}

/// The time-stop / resume sound plan for the current settings.
fn tw_plan(s: &Settings, resume: bool) -> dubs::Plan {
    let tw = &s.the_world;
    let on = s.audio.enabled;
    let def = tw.dub.def();
    let available = |f: &str| theworld_file(f).is_some();
    let mut plan = if resume {
        dubs::plan(
            def.resume,
            on && tw.resume_voice,
            on && tw.resume_sfx,
            tw.wait_for_voice,
            (dubs::GENERIC_RESUME, dubs::GENERIC_RESUME_IMPACT),
            &available,
        )
    } else {
        dubs::plan(
            def.stop,
            on && tw.voice,
            on && tw.stop_sfx,
            tw.wait_for_voice,
            (dubs::GENERIC_STOP, dubs::GENERIC_STOP_IMPACT),
            &available,
        )
    };
    if !resume && tw.windups[tw.dub.index() as usize] >= 0.0 {
        plan.moment = tw.windups[tw.dub.index() as usize];
    }
    plan
}

const RECORD_TIMEOUT: Duration = Duration::from_secs(10);
const SAVE_DEBOUNCE: Duration = Duration::from_millis(600);
const OVERLAY_HIDE_DELAY: Duration = Duration::from_millis(1500);

pub struct EngineArgs {
    pub settings: Settings,
    pub rx: Receiver<Msg>,
    pub tx: Sender<Msg>,
    pub hooks: Arc<HookShared>,
    pub ui: UiSink,
}

pub fn spawn(args: EngineArgs) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("cursorfect-engine".into())
        .spawn(move || {
            let mut engine = Engine::new(args);
            engine.run();
        })
}

struct Engine {
    settings: Settings,
    cfg: Config,
    machine: Machine,
    tracker: CursorTracker,
    effects: Effects,
    overlay: Option<Overlay>,
    overlay_retry_at: Option<Instant>,
    batch: Batch,
    hooks: Arc<HookShared>,
    audio: AudioHandle,
    ui: UiSink,
    rx: Receiver<Msg>,
    monitors: Vec<Rect>,
    dash_ready_at: Instant,
    recording: Option<(u8, Instant)>,
    save_due: Option<Instant>,
    last_tick: Instant,
    next_frame: Instant,
    hide_at: Option<Instant>,
    last_status: Option<(SysState, i32, u8)>,
    last_render_cursor: Option<Vec2>,
    scratch: Vec<(i32, i32, Instant)>,
    cursor_sprite: Option<crate::effects::CursorSprite>,
    /// Auto-deactivation time when the Sandevistan lasts as long as the sequence sound.
    auto_end_at: Option<Instant>,
    /// The current activation plays the whole track (FullTrack mode).
    full_track: bool,
    /// Deactivation came from the sound ending: do not cut the sound or play another one.
    natural_end: bool,
    sound_report_at: Option<Instant>,
    dump_dir: Option<std::path::PathBuf>,
    dump_count: u32,
    frames_since_activation: u32,
    cursors_locked: bool,
    /// Highest monitor refresh rate (the "auto" frame cap).
    refresh_hz: u32,
    /// Last movement level sent to the audio thread.
    motion_sent: f32,
    started: Instant,
    last_reload: Option<Instant>,
    /// The World.
    tw: TimeStop,
    watchdog: std::sync::Arc<Watchdog>,
    _tx: Sender<Msg>,
}

impl Engine {
    fn new(args: EngineArgs) -> Self {
        let EngineArgs {
            mut settings,
            rx,
            tx,
            hooks,
            ui,
        } = args;
        settings.sanitize();
        let now = Instant::now();
        let audio = AudioHandle::spawn(audio_config(&settings));
        let watchdog = Watchdog::spawn(hooks.time_stopped.clone());
        let mut machine = Machine::new(settings.energy.max);
        machine.set_master(settings.master_enabled);
        let cfg = effect_config(&settings);
        let e = Self {
            cfg,
            machine,
            tracker: CursorTracker::new(),
            effects: Effects::new(),
            overlay: None,
            overlay_retry_at: None,
            batch: Batch::new(Vec2::ZERO, Vec2::ONE),
            hooks,
            audio,
            ui,
            rx,
            monitors: native::monitors(),
            dash_ready_at: now,
            recording: None,
            save_due: None,
            last_tick: now,
            next_frame: now,
            hide_at: None,
            last_status: None,
            last_render_cursor: None,
            scratch: Vec::with_capacity(256),
            cursor_sprite: None,
            auto_end_at: None,
            full_track: false,
            natural_end: false,
            // Report the loaded sounds once the audio thread has decoded them.
            sound_report_at: Some(now + Duration::from_millis(1500)),
            dump_dir: std::env::var_os("CURSORFECT_DUMP")
                .map(std::path::PathBuf::from),
            dump_count: 0,
            frames_since_activation: 0,
            cursors_locked: false,
            refresh_hz: native::max_refresh_rate(),
            motion_sent: 0.0,
            started: now,
            last_reload: Some(now),
            tw: TimeStop::new(),
            watchdog,
            _tx: tx,
            settings,
        };
        e.publish_hook_state();
        e
    }

    // ---------------------------------------------------------------- main loop

    fn run(&mut self) {
        tracing::info!("engine started");
        loop {
            let now = Instant::now();
            let timeout = self.next_timeout(now);
            match self.rx.recv_timeout(timeout) {
                Ok(msg) => {
                    if self.handle(msg) {
                        break;
                    }
                    let mut stop = false;
                    while let Ok(msg) = self.rx.try_recv() {
                        if self.handle(msg) {
                            stop = true;
                            break;
                        }
                    }
                    if stop {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.frame(Instant::now());
        }
        self.shutdown();
    }

    fn animating(&self, now: Instant) -> bool {
        self.overlay.is_some()
            && (self.effects.is_animating(now, &self.tracker) || self.tw.is_animating())
    }

    fn next_timeout(&self, now: Instant) -> Duration {
        let mut t = Duration::from_secs(3600);
        let mut consider = |d: Duration| {
            if d < t {
                t = d;
            }
        };
        if self.animating(now) {
            consider(self.next_frame.saturating_duration_since(now));
        }
        if self.machine.needs_tick(&self.settings.energy) {
            consider(Duration::from_millis(100));
        }
        if let Some((_, deadline)) = self.recording {
            consider(deadline.saturating_duration_since(now));
        }
        if let Some(due) = self.save_due {
            consider(due.saturating_duration_since(now));
        }
        if let Some(h) = self.hide_at {
            consider(h.saturating_duration_since(now));
        }
        if let Some(r) = self.overlay_retry_at {
            consider(r.saturating_duration_since(now));
        }
        if let Some(r) = self.auto_end_at {
            consider(r.saturating_duration_since(now));
        }
        if let Some(r) = self.sound_report_at {
            consider(r.saturating_duration_since(now));
        }
        t.max(Duration::from_millis(1))
    }

    fn frame_interval(&self) -> Duration {
        let mut fps = match self.settings.performance.fps_cap {
            0 => self.refresh_hz.clamp(30, 360),
            n => n.clamp(30, 360),
        };
        if self.settings.performance.performance_mode {
            fps = fps.min(60);
        }
        // Presents wait for the display refresh: wake a little early so every refresh gets a
        // frame (the present itself does the exact pacing).
        Duration::from_secs_f64(0.85 / f64::from(fps))
    }

    fn frame(&mut self, now: Instant) {
        // Display topology changes: rebuild the overlay and monitor list.
        if take_display_changed() || self.overlay.as_ref().is_some_and(|o| !o.matches_desktop()) {
            tracing::info!("display configuration changed");
            self.monitors = native::monitors();
            self.refresh_hz = native::max_refresh_rate();
            if self.overlay.is_some() {
                self.overlay = None;
                self.ensure_overlay();
            }
        }
        if let Some(o) = &self.overlay {
            o.pump();
        }
        self.ingest_cursor(now);

        // Energy / cooldown.
        let dt = now.saturating_duration_since(self.last_tick).as_secs_f32();
        self.last_tick = now;
        if let Some(tr) = self.machine.tick(dt, &self.settings.energy) {
            self.on_transition(tr, now);
        }

        self.update_time_stop(now);

        if self.auto_end_at.is_some_and(|t| now >= t) {
            self.auto_end_at = None;
            self.natural_end = true;
            if let Some(tr) = self.machine.deactivate() {
                self.on_transition(tr, now);
            }
            self.natural_end = false;
        }
        if self.sound_report_at.is_some_and(|t| now >= t) {
            self.sound_report_at = None;
            let info = self.audio.info();
            let msg = if info.custom.is_empty() {
                format!("No sound files found, using built-in sounds.\n{}", info.parts)
            } else {
                format!("Files: {}\n{}", info.custom.join(", "), info.parts)
            };
            (self.ui)(UiMsg::SoundInfo(msg));
        }
        if let Some((_, deadline)) = self.recording {
            if now >= deadline {
                self.end_recording();
                (self.ui)(UiMsg::Notice("Shortcut recording timed out".into()));
            }
        }
        if self.save_due.is_some_and(|d| now >= d) {
            self.save_now();
        }
        if self.overlay_retry_at.is_some_and(|r| now >= r) && self.machine.master() {
            self.overlay_retry_at = None;
            if self.effects.is_active() {
                self.ensure_overlay();
            }
        }

        self.render(now);
        self.send_motion(now);
        self.push_status();
    }

    /// Feed cursor speed to the audio thread's movement layer.
    fn send_motion(&mut self, now: Instant) {
        let level = if self.machine.is_active() {
            let speed = self.tracker.speed(now, Duration::from_millis(80));
            ((speed - 40.0) / 1700.0).clamp(0.0, 1.0).powf(0.75)
        } else {
            0.0
        };
        if (level - self.motion_sent).abs() > 0.02 || (level == 0.0 && self.motion_sent > 0.0) {
            self.motion_sent = level;
            self.audio.send(AudioCmd::Motion(level));
        }
    }

    /// Pull cursor samples from the hook queue and the real cursor position.
    fn ingest_cursor(&mut self, now: Instant) {
        if !self.machine.master() {
            return;
        }
        self.scratch.clear();
        self.hooks.drain_moves(&mut self.scratch);
        for (x, y, t) in self.scratch.drain(..) {
            self.tracker.push(Vec2::new(x as f32, y as f32), t, false);
        }
        // Authoritative sample at frame time (covers hook gaps and DPI/format surprises).
        if self.effects.is_active() || self.animating(now) || !self.tw.is_idle() {
            if let Some(p) = native::cursor_pos() {
                self.tracker.push(p, now, false);
            }
        }
    }

    fn render(&mut self, now: Instant) {
        let interval = self.frame_interval();
        let Some(overlay) = self.overlay.as_mut() else {
            return;
        };
        if now < self.next_frame {
            return;
        }
        let animating = self.effects.is_animating(now, &self.tracker) || self.tw.is_animating();
        let cursor = self.tracker.current();
        let glow_moved = self.effects.has_glow() && cursor != self.last_render_cursor;
        let needs_clear = overlay.has_content();
        if !(animating || glow_moved || needs_clear) {
            if overlay.is_visible() && self.hide_at.is_none() {
                self.hide_at = Some(now + OVERLAY_HIDE_DELAY);
            }
            if self.hide_at.is_some_and(|h| now >= h) {
                overlay.set_visible(false);
                self.hide_at = None;
            }
            return;
        }
        self.hide_at = None;
        self.next_frame = now + interval;
        self.cfg.scale = native::cursor_scale();
        // Afterimages are copies of the real cursor: refresh the bitmap when its shape changes.
        if let Some(h) = native::arrow_cursor_handle() {
            if h != overlay.cursor_handle() || !overlay.has_cursor_texture() {
                if let Some(img) = native::capture_cursor(h) {
                    let opaque = img.rgba.chunks(4).filter(|p| p[3] > 128).count();
                    let bright = img
                        .rgba
                        .chunks(4)
                        .filter(|p| p[3] > 128 && p[0] > 128)
                        .count();
                    tracing::debug!(
                        "cursor captured {}x{} hotspot {:?} opaque px {} bright px {}",
                        img.width,
                        img.height,
                        img.hotspot,
                        opaque,
                        bright
                    );
                    overlay.set_cursor(&img);
                    self.cursor_sprite = Some(crate::effects::CursorSprite {
                        size: Vec2::new(img.width as f32, img.height as f32),
                        hotspot: img.hotspot,
                    });
                }
            }
        }
        self.cfg.cursor = if overlay.has_cursor_texture() {
            self.cursor_sprite
        } else {
            None
        };
        self.cfg.world_supported = overlay.world_supported();
        self.cfg.hdr = overlay.hdr_available() && self.settings.effects.hdr_bloom;
        let max_radius = overlay.size().length();
        self.effects.update(now, &self.tracker, &self.cfg);
        self.batch = Batch::new(overlay.origin(), overlay.size());
        self.effects
            .draw(&mut self.batch, &self.tracker, now, &self.cfg, max_radius);
        let mut crops = Vec::new();
        let world = if !self.tw.is_idle() {
            self.tw.draw(&mut self.batch, now, &self.cfg, cursor);
            crops = self.tw.crops(now);
            if self.cfg.world_supported {
                self.tw.world(now, max_radius)
            } else {
                None
            }
        } else if self.cfg.world_supported {
            self.effects.world(now, max_radius, &self.cfg)
        } else {
            None
        };
        if world.is_none() {
            overlay.release_world();
        }
        self.last_render_cursor = cursor;
        if !overlay.is_visible() && (!self.batch.is_empty() || world.is_some()) {
            overlay.set_visible(true);
        }
        let grade = Grade {
            time: now.saturating_duration_since(self.started).as_secs_f32() % 1000.0,
            bloom: self.cfg.hdr && !self.settings.performance.performance_mode,
            bloom_strength: 0.35 + 0.25 * self.cfg.intensity,
            film: self.settings.effects.film_grade && !self.settings.performance.performance_mode,
            grain: 1.0,
        };
        let result = overlay.render(&self.batch, world.as_ref(), &crops, &grade);
        if self.effects.is_active() {
            self.frames_since_activation += 1;
            if let Some(dir) = self.dump_dir.as_ref() {
                if self.dump_count < 12 && self.frames_since_activation % 15 == 0 {
                    self.dump_count += 1;
                    let path = dir.join(format!("frame_{:02}.bmp", self.dump_count));
                    if let Err(e) = overlay.dump_bmp(&path) {
                        tracing::warn!("frame dump failed: {e:#}");
                    }
                }
            }
        }
        if result == FrameResult::DeviceLost {
            tracing::warn!("GPU device lost; recreating overlay");
            self.overlay = None;
            self.ensure_overlay();
        }
    }

    fn ensure_overlay(&mut self) {
        if self.overlay.is_some() {
            return;
        }
        match Overlay::create() {
            Ok(o) => {
                tracing::info!("overlay created ({:?})", o.screen());
                self.overlay = Some(o);
                self.overlay_retry_at = None;
                self.monitors = native::monitors();
            }
            Err(e) => {
                // Log once per attempt; retry later without popping up error dialogs.
                tracing::error!("overlay unavailable: {e:#}");
                self.overlay_retry_at = Some(Instant::now() + Duration::from_secs(10));
            }
        }
    }

    // ---------------------------------------------------------------- messages

    /// Returns true when the engine must stop.
    fn handle(&mut self, msg: Msg) -> bool {
        let now = Instant::now();
        match msg {
            Msg::Input(ev) => self.on_input(ev, now),
            Msg::Cmd(Cmd::Shutdown) => return true,
            Msg::Cmd(cmd) => self.on_cmd(cmd, now),
        }
        false
    }

    fn on_input(&mut self, ev: InputEvent, now: Instant) {
        match ev {
            InputEvent::MouseWake => {}
            InputEvent::ActivationDown if self.settings.effect == EffectKind::TheWorld => {
                self.tw_press(now);
            }
            InputEvent::ActivationUp if self.settings.effect == EffectKind::TheWorld => {
                if self.settings.the_world.mode == TimeStopMode::Hold {
                    self.tw_resume(now);
                }
            }
            InputEvent::DashDown if self.settings.effect == EffectKind::TheWorld => {}
            InputEvent::Grab { down, x, y } => {
                let p = Vec2::new(x as f32, y as f32);
                if down {
                    self.tw.grab(p, now);
                } else {
                    self.tw.drag_to(p, now);
                    self.tw.release();
                }
                self.next_frame = now;
            }
            InputEvent::GrabCancel => self.tw.cancel_drag(),
            InputEvent::GrabUndo => {
                self.tw.undo();
            }
            InputEvent::ActivationDown => {
                let tr = match self.settings.activation_mode {
                    ActivationMode::Toggle => self.machine.toggle(&self.settings.energy),
                    ActivationMode::Hold => self.machine.activate(&self.settings.energy),
                };
                if let Some(tr) = tr {
                    self.on_transition(tr, now);
                }
            }
            InputEvent::ActivationUp => {
                if self.settings.activation_mode == ActivationMode::Hold {
                    if let Some(tr) = self.machine.deactivate() {
                        self.on_transition(tr, now);
                    }
                }
            }
            InputEvent::DashDown => self.try_dash(now),
            InputEvent::Emergency => self.emergency(now),
            InputEvent::Recorded(sc) => self.on_recorded(sc),
            InputEvent::RecordCancelled => {
                self.end_recording();
                (self.ui)(UiMsg::Notice("Recording cancelled".into()));
            }
        }
    }

    fn on_cmd(&mut self, cmd: Cmd, now: Instant) {
        match cmd {
            Cmd::SetSettings(s) => self.apply_settings(*s, now, false),
            Cmd::Record(which) => {
                if which == RECORD_NONE {
                    self.end_recording();
                } else {
                    self.recording = Some((which, now + RECORD_TIMEOUT));
                    self.hooks.recording.store(which, Ordering::Release);
                    (self.ui)(UiMsg::Notice(String::new()));
                }
            }
            Cmd::Preview(part) => {
                // Re-scan the sound folders so freshly added files are picked up.
                if self
                    .last_reload
                    .is_none_or(|t| now.saturating_duration_since(t) > Duration::from_secs(3))
                {
                    self.audio.send(AudioCmd::Reload);
                    self.last_reload = Some(now);
                }
                self.audio.send(AudioCmd::Preview(part));
                self.sound_report_at = Some(now + Duration::from_millis(1200));
            }
            Cmd::TwPreview(which) => {
                let mut s = self.settings.clone();
                s.audio.enabled = true;
                let clips = match which {
                    0 => tw_plan(&s, false).clips,
                    1 => tw_plan(&s, true).clips,
                    _ => vec![(0.0, dubs::TICK)],
                };
                if clips.is_empty() {
                    (self.ui)(UiMsg::Notice(
                        "That sound is switched off or its file is missing (sounds\\theworld)".into(),
                    ));
                }
                self.audio.send(AudioCmd::TwPreview(clips));
            }
            Cmd::ResetDefaults => {
                self.end_recording();
                self.apply_settings(Settings::default(), now, true);
            }
            Cmd::Tray(action) => self.on_tray(action, now),
            Cmd::Shutdown => {}
        }
    }

    fn on_tray(&mut self, action: TrayAction, now: Instant) {
        let mut s = self.settings.clone();
        match action {
            TrayAction::ToggleMaster => s.master_enabled = !s.master_enabled,
            TrayAction::Preset(p) => s.preset = p,
            TrayAction::ToggleActive if self.settings.effect == EffectKind::TheWorld => {
                self.tw_press(now);
                return;
            }
            TrayAction::ToggleActive => {
                let tr = self.machine.toggle(&self.settings.energy);
                if let Some(tr) = tr {
                    self.on_transition(tr, now);
                }
                return;
            }
            TrayAction::OpenSettings | TrayAction::Exit => return, // handled on the UI thread
        }
        self.apply_settings(s, now, true);
    }

    /// Apply a new settings snapshot. `echo` pushes it back to the UI.
    fn apply_settings(&mut self, mut new: Settings, now: Instant, echo: bool) {
        new.sanitize();
        let old = std::mem::replace(&mut self.settings, new);
        let s = self.settings.clone();
        let s = &s;

        if old.effect != s.effect || !s.master_enabled {
            // Only one effect at a time: switching ends the other one.
            if s.effect != EffectKind::TheWorld || !s.master_enabled {
                self.tw_force_end(now, true);
            }
            if s.effect != EffectKind::Sandevistan {
                if let Some(tr) = self.machine.deactivate() {
                    self.on_transition(tr, now);
                }
            }
        }
        if old.master_enabled != s.master_enabled {
            if let Some(tr) = self.machine.set_master(s.master_enabled) {
                self.on_transition(tr, now);
            }
            if !s.master_enabled {
                self.effects.clear();
                self.tracker.clear();
                self.hooks.clear_moves();
                self.last_render_cursor = None;
            }
        }
        if old.energy != s.energy {
            self.machine.tick(0.0, &s.energy);
        }
        if old.audio != s.audio || old.preset != s.preset {
            self.audio.send(AudioCmd::SetEnabled(s.audio.enabled));
            self.audio.send(AudioCmd::SetVolume(s.audio.volume));
            self.audio.send(AudioCmd::SetMoveVolume(s.audio.move_volume));
            self.audio.send(AudioCmd::SetMode(s.audio.mode));
            self.audio.send(AudioCmd::SetSvParts(sv_parts(s)));
            let profile: SfxProfile = style_for(s).sfx;
            self.audio.send(AudioCmd::SetProfile(profile));
        }
        if old.startup != s.startup {
            if let Err(e) =
                native::set_startup(s.startup.start_with_windows, s.startup.start_minimized)
            {
                tracing::warn!("could not update startup entry: {e:#}");
            }
        }
        let (ri, ro) = (self.cfg.ramp_in, self.cfg.ramp_out);
        let lock = self.machine.is_active() && self.settings.keep_arrow_cursor;
        self.set_cursor_lock(lock);
        self.cfg = effect_config(&self.settings);
        self.cfg.ramp_in = ri;
        self.cfg.ramp_out = ro;
        self.publish_hook_state();
        self.mark_dirty(now);
        if echo {
            (self.ui)(UiMsg::Settings(Box::new(self.settings.clone())));
        }
        self.last_status = None;
        self.push_status();
    }

    fn publish_hook_state(&self) {
        let s = &self.settings;
        self.hooks.set_bindings(Bindings {
            activation: s.activation,
            dash: s.dash,
            consume_mouse: s.consume_mouse_buttons,
        });
        self.hooks.master.store(s.master_enabled, Ordering::Release);
        self.hooks
            .wake_on_move
            .store(self.machine.is_active() || !self.tw.is_idle(), Ordering::Release);
    }

    // ---------------------------------------------------------------- behaviours

    fn on_transition(&mut self, tr: Transition, now: Instant) {
        let pos = native::cursor_pos()
            .or_else(|| self.tracker.current())
            .unwrap_or(Vec2::ZERO);
        match tr {
            Transition::Activated => {
                native::boost_mouse_speed(self.settings.mouse_boost);
                self.ensure_overlay();
                self.tracker.push(pos, now, false);
                self.frames_since_activation = 0;
                self.dump_count = 0;
                let info = self.audio.info();
                let full = self.settings.audio.enabled
                    && self.settings.audio.mode == SoundMode::FullTrack
                    && info.sequence_len.is_some();
                match info.sequence_len {
                    Some(len) if full => {
                        // Whole track: the colour drains with the opening hit, the effect lasts
                        // as long as the sound and colour floods back during its fade-out tail.
                        self.cfg.ramp_in = 0.75;
                        self.cfg.ramp_out = 0.95;
                        self.auto_end_at = Some(now + len.saturating_sub(Duration::from_millis(950)));
                    }
                    Some(_) => {
                        // Split sound: drain synced to the start hit (peaks ~0.55 s in), colour
                        // returns during the power-down.
                        self.cfg.ramp_in = 0.6;
                        self.cfg.ramp_out = 0.9;
                        self.auto_end_at = None;
                    }
                    None => {
                        self.cfg.ramp_in = 0.6;
                        self.cfg.ramp_out = 0.45;
                        self.auto_end_at = None;
                    }
                }
                self.full_track = full;
                self.audio.send(AudioCmd::Start);
                self.effects.activate(pos, now, &self.cfg);
                self.next_frame = now;
            }
            Transition::Deactivated => {
                native::boost_mouse_speed(false);
                self.effects.deactivate(pos, now);
                self.auto_end_at = None;
                self.audio.send(AudioCmd::End {
                    natural: self.natural_end && self.full_track,
                });
                self.full_track = false;
                self.natural_end = false;
            }
            Transition::Overheated => {
                native::boost_mouse_speed(false);
                self.effects.deactivate(pos, now);
                self.auto_end_at = None;
                self.full_track = false;
                self.audio.send(AudioCmd::Overheat);
            }
            Transition::CooldownComplete => self.audio.play(Sfx::Cooldown),
        }
        self.hooks
            .wake_on_move
            .store(self.machine.is_active(), Ordering::Release);
        let lock = self.machine.is_active() && self.settings.keep_arrow_cursor;
        self.set_cursor_lock(lock);
        self.last_status = None;
    }

    /// Swap the hand / I-beam cursors for the arrow while active (and undo it).
    fn set_cursor_lock(&mut self, on: bool) {
        if on == self.cursors_locked {
            return;
        }
        self.cursors_locked = on;
        if on {
            native::lock_arrow_cursors();
        } else {
            native::restore_system_cursors();
        }
    }

    fn emergency(&mut self, now: Instant) {
        tracing::warn!("emergency disable triggered");
        self.end_recording();
        let was_active = self.machine.is_active();
        self.tw_force_end(now, false);
        self.machine.emergency_stop();
        native::boost_mouse_speed(false);
        self.set_cursor_lock(false);
        self.settings.master_enabled = false;
        self.effects.clear();
        self.tracker.clear();
        self.hooks.clear_moves();
        self.hooks.master.store(false, Ordering::Release);
        self.hooks.wake_on_move.store(false, Ordering::Release);
        if was_active {
            self.audio.send(AudioCmd::End { natural: false });
            self.full_track = false;
        }
        if let Some(o) = self.overlay.as_mut() {
            // One transparent frame, then hide.
            self.batch = Batch::new(o.origin(), o.size());
            let _ = o.render(&self.batch, None, &[], &Grade::default());
            o.set_visible(false);
        }
        self.mark_dirty(now);
        (self.ui)(UiMsg::Settings(Box::new(self.settings.clone())));
        (self.ui)(UiMsg::Notice("Emergency disable: system is OFF".into()));
        self.last_status = None;
        self.push_status();
    }

    // ---------------------------------------------------------------- The World

    fn tw_options(&self) -> TwOptions {
        let t = &self.settings.the_world;
        let reduced = self.settings.performance.performance_mode;
        TwOptions {
            dub: t.dub,
            custom_primary: crate::settings::presets::parse_hex(&t.custom_primary).unwrap_or([0.91, 0.36, 1.0]),
            custom_secondary: crate::settings::presets::parse_hex(&t.custom_secondary).unwrap_or([0.38, 0.22, 0.78]),
            mode: t.mode,
            duration: f32::from(t.duration_secs),
            max_secs: t.max_secs as f32,
            flash: t.flash && !reduced,
            inversion: t.inversion,
            tint: t.tint,
            ripples: t.ripples && !reduced,
            streaks: t.streaks,
            flare: t.flare && !reduced,
            rays: t.rays,
            countdown: t.countdown,
            move_windows: t.move_windows,
        }
    }

    /// The shortcut was pressed with The World selected.
    fn tw_press(&mut self, now: Instant) {
        if !self.machine.master() {
            return;
        }
        if self.tw.is_idle() {
            self.tw_start(now);
        } else if self.tw.is_running() {
            // Toggle / Timed: second press resumes. Hold resumes on release (and a press while
            // holding can't happen), so this also covers a press during the voice line.
            self.tw_resume(now);
        }
    }

    fn tw_start(&mut self, now: Instant) {
        if let Some(tr) = self.machine.deactivate() {
            self.on_transition(tr, now);
        }
        let plan = tw_plan(&self.settings, false);
        self.audio.send(AudioCmd::TwStopAll);
        if !plan.clips.is_empty() {
            self.audio.send(AudioCmd::TwPlay(plan.clips.clone()));
        }
        let center = native::cursor_pos()
            .or_else(|| self.tracker.current())
            .unwrap_or(Vec2::ZERO);
        self.monitors = native::monitors();
        let mon = self
            .monitors
            .iter()
            .find(|m| m.contains(center))
            .or_else(|| self.monitors.first())
            .map(|m| WinRect {
                l: m.left,
                t: m.top,
                r: m.right,
                b: m.bottom,
            })
            .unwrap_or(WinRect {
                l: 0.0,
                t: 0.0,
                r: 1920.0,
                b: 1080.0,
            });
        self.tw.begin(now, center, plan.moment, self.tw_options(), mon);
        self.ensure_overlay();
        if let Some(o) = self.overlay.as_mut() {
            o.prewarm();
        }
        self.tracker.push(center, now, false);
        self.hooks.wake_on_move.store(true, Ordering::Release);
        self.next_frame = now;
        self.last_status = None;
        tracing::info!("The World: time stop in {:.2}s", plan.moment);
    }

    fn tw_resume(&mut self, now: Instant) {
        if !self.tw.is_running() {
            return;
        }
        let cancelling = !self.tw.input_frozen();
        let plan = tw_plan(&self.settings, true);
        self.audio.send(AudioCmd::TwStopAll);
        if cancelling {
            // Released during the voice line: nothing froze, just stop.
            self.tw.request_resume(now, 0.0);
            self.last_status = None;
            return;
        }
        if !plan.clips.is_empty() {
            self.audio.send(AudioCmd::TwPlay(plan.clips.clone()));
        }
        if let Some(p) = native::cursor_pos() {
            self.tw.drag_to(p, now);
        }
        self.tw.request_resume(now, plan.moment);
        let hwnd = self.overlay.as_ref().map(|o| o.hwnd_raw()).unwrap_or(0);
        self.watchdog.arm(
            Some(now + Duration::from_secs_f32(plan.moment) + Duration::from_secs(4)),
            hwnd,
        );
        self.next_frame = now;
    }

    /// Advance The World and carry out what it asks for.
    fn update_time_stop(&mut self, now: Instant) {
        if self.tw.is_idle() {
            return;
        }
        if let Some(p) = self.tracker.current() {
            self.tw.drag_to(p, now);
        }
        for ev in self.tw.update(now) {
            match ev {
                TwEvent::Freeze => self.tw_freeze(now),
                TwEvent::Tick(_) => {
                    if self.settings.audio.enabled && self.settings.the_world.ticks {
                        if self.settings.the_world.dub == dubs::Dub::Custom {
                            self.audio.send(AudioCmd::TwPlay(vec![(0.0, dubs::Clip::new("custom_tick.mp3", 0.0, 3600.0))]));
                        } else { self.audio.send(AudioCmd::TwTick); }
                    }
                }
                TwEvent::AutoResume => self.tw_resume(now),
                TwEvent::Unfreeze => self.tw_unfreeze(now, true),
                TwEvent::Done => {
                    self.hooks
                        .wake_on_move
                        .store(self.machine.is_active(), Ordering::Release);
                    self.last_status = None;
                }
            }
        }
    }

    fn tw_freeze(&mut self, now: Instant) {
        self.ensure_overlay();
        let ok = match self.overlay.as_mut() {
            Some(o) if o.world_supported() => o.freeze(),
            _ => false,
        };
        if !ok {
            self.tw.abort(now);
            self.audio.send(AudioCmd::TwStopAll);
            (self.ui)(UiMsg::Notice(
                "The World needs screen capture (Windows 10 2004+, not on the secure desktop)".into(),
            ));
            return;
        }
        let windows = if self.settings.the_world.move_windows {
            wm::top_level_windows()
        } else {
            Vec::new()
        };
        tracing::info!("The World: time stopped ({} movable windows)", windows.len());
        // The animation starts from *after* the capture, so nothing jumps ahead.
        let now = Instant::now();
        self.tw.frozen(now, windows, &self.cfg);
        self.hooks.time_stopped.store(true, Ordering::Release);
        self.set_cursor_lock(true);
        let hwnd = self.overlay.as_ref().map(|o| o.hwnd_raw()).unwrap_or(0);
        let deadline = self.tw.deadline().unwrap_or(now) + Duration::from_secs(5);
        self.watchdog.arm(Some(deadline), hwnd);
        if let Some(o) = self.overlay.as_mut() {
            o.set_visible(true);
        }
        self.next_frame = now;
        self.last_status = None;
    }

    /// Time starts again: move the windows, show the live desktop, give input back.
    fn tw_unfreeze(&mut self, now: Instant, apply: bool) {
        if apply {
            if let Some(p) = native::cursor_pos() { self.tw.drag_to(p, now); }
            self.tw.release();
        }
        let moves = self.tw.pending_moves();
        // Input first: never keep the desktop blocked longer than necessary.
        self.hooks.time_stopped.store(false, Ordering::Release);
        self.watchdog.arm(None, 0);
        if apply && !moves.is_empty() {
            let n = wm::apply_moves(&moves);
            tracing::info!("The World: moved {n}/{} windows", moves.len());
            if n < moves.len() {
                (self.ui)(UiMsg::Notice(format!(
                    "{} window(s) could not be moved (closed or running as administrator)",
                    moves.len() - n
                )));
            }
        }
        if let Some(o) = self.overlay.as_mut() {
            o.unfreeze();
        }
        let center = native::cursor_pos().unwrap_or(Vec2::ZERO);
        self.tw.unfrozen(now, center);
        let lock = self.machine.is_active() && self.settings.keep_arrow_cursor;
        self.set_cursor_lock(lock);
        self.next_frame = now;
        self.last_status = None;
    }

    /// Stop The World immediately (emergency, switching effect, master off, exit).
    fn tw_force_end(&mut self, now: Instant, apply_moves: bool) {
        if self.tw.is_idle() {
            return;
        }
        self.audio.send(AudioCmd::TwStopAll);
        if self.tw.input_frozen() {
            self.tw_unfreeze(now, apply_moves);
        }
        self.tw.abort(now);
        self.hooks.time_stopped.store(false, Ordering::Release);
        self.watchdog.arm(None, 0);
        if let Some(o) = self.overlay.as_mut() {
            o.unfreeze();
        }
        self.last_status = None;
    }

    fn try_dash(&mut self, now: Instant) {
        if !self.settings.effects.warp_dash || !self.machine.can_dash() || now < self.dash_ready_at
        {
            return;
        }
        // Dragging with a held button: never teleport. (The middle button itself is ignored
        // when it is the dash trigger: it is down right now.)
        let middle_trigger = self.settings.dash.trigger == Trigger::Mouse(MouseButton::Middle);
        if native::any_mouse_button_down(middle_trigger) {
            return;
        }
        // Make sure the tracker has the freshest movement before reading the direction.
        self.ingest_cursor(now);
        let Some(start) = native::cursor_pos() else {
            return;
        };
        let Some(dir) = self.tracker.direction(now, Duration::from_millis(220), 6.0) else {
            return; // no recent movement direction: do nothing
        };
        self.monitors = native::monitors();
        let dist = self.settings.dash_settings.distance_px as f32;
        let Some(end) = dash_endpoint(start, dir, dist, &self.monitors) else {
            return;
        };
        if !self
            .machine
            .try_spend(self.settings.energy.dash_cost, &self.settings.energy)
        {
            return;
        }
        if !native::set_cursor_pos(end) {
            return;
        }
        // The teleport is recorded as a discontinuity; the follow-up hook sample at the same
        // position is de-duplicated, so a synthetic move cannot cause a runaway dash.
        self.tracker.push(end, now, true);
        self.ensure_overlay();
        self.effects.dash(start, end, now, &self.cfg);
        self.next_frame = now;
        self.audio.send(AudioCmd::Dash);
        self.dash_ready_at =
            now + Duration::from_millis(u64::from(self.settings.dash_settings.cooldown_ms));
    }

    fn on_recorded(&mut self, sc: Shortcut) {
        let which = self.recording.map(|(w, _)| w).unwrap_or(RECORD_NONE);
        self.end_recording();
        if which != RECORD_ACTIVATION && which != RECORD_DASH {
            return;
        }
        if let Err(e) = sc.validate() {
            (self.ui)(UiMsg::Notice(format!("Can't use \"{}\": {e}", sc.display())));
            return;
        }
        let (mine, other) = if which == RECORD_ACTIVATION {
            (self.settings.activation, self.settings.dash)
        } else {
            (self.settings.dash, self.settings.activation)
        };
        let mut notice = String::new();
        if sc == other {
            // Same as the other shortcut: swap them instead of refusing.
            if which == RECORD_ACTIVATION {
                self.settings.dash = mine;
            } else {
                self.settings.activation = mine;
            }
            notice = format!(
                "Swapped: {} is now {}",
                if which == RECORD_ACTIVATION {
                    "Warp Dash"
                } else {
                    "Activation"
                },
                mine.display()
            );
        }
        if which == RECORD_ACTIVATION {
            self.settings.activation = sc;
        } else {
            self.settings.dash = sc;
        }
        if sc.is_typing_key() {
            notice = format!(
                "Heads up: {} also fires while you type. Add Ctrl/Alt for a safer shortcut.",
                sc.display()
            );
        } else if notice.is_empty() {
            notice = format!("Shortcut set: {}", sc.display());
        }
        self.publish_hook_state();
        self.mark_dirty(Instant::now());
        (self.ui)(UiMsg::Settings(Box::new(self.settings.clone())));
        (self.ui)(UiMsg::Notice(notice));
    }

    fn end_recording(&mut self) {
        self.recording = None;
        self.hooks.recording.store(RECORD_NONE, Ordering::Release);
        self.last_status = None;
    }

    // ---------------------------------------------------------------- status / persistence

    fn push_status(&mut self) {
        let state = if self.tw.is_idle() {
            self.machine.state()
        } else {
            SysState::TimeStopped
        };
        let pct = if self.settings.energy.enabled {
            (self.machine.energy() / self.settings.energy.max * 100.0).clamp(0.0, 100.0)
        } else {
            100.0
        };
        let rec = self.recording.map(|(w, _)| w).unwrap_or(0);
        let key = (state, pct.round() as i32, rec);
        if self.last_status != Some(key) {
            self.last_status = Some(key);
            (self.ui)(UiMsg::Status {
                state,
                energy_pct: pct,
                recording: rec,
            });
        }
    }

    fn mark_dirty(&mut self, now: Instant) {
        self.save_due = Some(now + SAVE_DEBOUNCE);
    }

    fn save_now(&mut self) {
        self.save_due = None;
        if let Err(e) = config::save(&self.settings) {
            tracing::warn!("could not save settings: {e:#}");
        }
    }

    fn shutdown(&mut self) {
        tracing::info!("engine shutting down");
        self.tw_force_end(Instant::now(), true);
        self.end_recording();
        self.set_cursor_lock(false);
        self.hooks.master.store(false, Ordering::Release);
        self.effects.clear();
        if let Some(o) = self.overlay.as_mut() {
            self.batch = Batch::new(o.origin(), o.size());
            let _ = o.render(&self.batch, None, &[], &Grade::default());
        }
        self.overlay = None; // destroys the window and releases GPU objects
        self.save_now();
        self.audio.shutdown();
    }
}

/// Offline visual check (`cursorfect.exe --render-test <folder>`): drives the effects along a
/// synthetic cursor path, renders through the real overlay and saves frames as .bmp.
/// The overlay is invisible to screenshots, so this is the way to look at the output.
pub fn render_test(dir: &std::path::Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut overlay = Overlay::create()?;
    let settings = Settings::default();
    let mut cfg = effect_config(&settings);
    cfg.world_supported = overlay.world_supported();
    cfg.hdr = overlay.hdr_available();
    if let Some(h) = native::arrow_cursor_handle() {
        if let Some(img) = native::capture_cursor(h) {
            overlay.set_cursor(&img);
            cfg.cursor = Some(crate::effects::CursorSprite {
                size: Vec2::new(img.width as f32, img.height as f32),
                hotspot: img.hotspot,
            });
        }
    }
    cfg.scale = native::cursor_scale();
    let size = overlay.size();
    let origin = overlay.origin();
    let max_radius = size.length();
    let mut fx = Effects::new();
    let mut tracker = CursorTracker::new();
    let t0 = Instant::now();
    let dt = Duration::from_millis(16);
    let path = |f: f32| -> Vec2 {
        let c = origin + size * 0.5;
        c + Vec2::new((f * 2.1).cos() * size.x * 0.28, (f * 3.3).sin() * size.y * 0.22)
    };
    overlay.set_visible(true);
    let mut saved = Vec::new();
    let mut f = 0.0f32;
    for frame in 0..150u32 {
        let now = t0 + dt * frame;
        // A fast flick between frames 95 and 115 (dense afterimages + speed burst).
        f += 0.016 * if (95..=115).contains(&frame) { 4.0 } else { 1.0 };
        tracker.push(path(f), now, false);
        if frame == 2 {
            fx.activate(path(f), now, &cfg);
        }
        if frame == 70 {
            let from = path(f);
            let to = from + (path(f) - path(f - 0.05)).normalize_or_zero() * 350.0;
            tracker.push(to, now, true);
            fx.dash(from, to, now, &cfg);
        }
        fx.update(now, &tracker, &cfg);
        let mut batch = Batch::new(origin, size);
        fx.draw(&mut batch, &tracker, now, &cfg, max_radius);
        let world = if cfg.world_supported {
            fx.world(now, max_radius, &cfg)
        } else {
            None
        };
        let grade = Grade {
            time: f,
            ..Grade::default()
        };
        overlay.pump();
        let _ = overlay.render(&batch, world.as_ref(), &[], &grade);
        if [8u32, 20, 45, 72, 80, 104, 112, 120].contains(&frame) {
            let p = dir.join(format!("frame_{frame:03}.bmp"));
            overlay.dump_bmp(&p)?;
            saved.push(p);
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    overlay.set_visible(false);
    tracing::info!(
        "render test: {} frames saved to {} (hdr {}, world {})",
        saved.len(),
        dir.display(),
        cfg.hdr,
        cfg.world_supported
    );
    Ok(())
}

/// Visual check of The World (`cursorfect.exe --render-test-tw <folder>`): stops time through
/// the real overlay, drags the topmost window's frozen image (the real window is never moved),
/// resumes, and saves frames as .bmp.
pub fn render_test_tw(dir: &std::path::Path, measure: bool) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut overlay = Overlay::create()?;
    let settings = Settings::default();
    let mut cfg = effect_config(&settings);
    cfg.world_supported = overlay.world_supported();
    cfg.hdr = overlay.hdr_available();
    let size = overlay.size();
    let origin = overlay.origin();
    let max_radius = size.length();
    let center = origin + size * Vec2::new(0.45, 0.5);
    let mon = WinRect {
        l: origin.x,
        t: origin.y,
        r: origin.x + size.x,
        b: origin.y + size.y,
    };
    let mut tw = TimeStop::new();
    let t0 = Instant::now();
    // 1.9 s of voice-line build-up (like the DIO dub), then the freeze.
    const C: u32 = 119;
    tw.begin(t0, center, 1.9, TwOptions::default(), mon);
    overlay.set_visible(true);
    let windows = wm::top_level_windows();
    tracing::info!("render test tw: {} movable windows", windows.len());
    let grab = windows.first().map(|w| Vec2::new((w.rect.l + w.rect.r) * 0.5, w.rect.t + 15.0));
    let dt = Duration::from_millis(16);
    let mut saved = 0;
    let mut times: Vec<f32> = Vec::new();
    let mut last = Instant::now();
    let mut freeze_ms = 0.0f32;
    // In measure mode the run is real-time: the animation clock is the real clock.
    let real0 = Instant::now();
    for frame in 0..(200 + C) {
        let now = if measure {
            real0 + (Instant::now() - real0)
        } else {
            t0 + dt * frame
        };
        if measure && frame > 0 {
            times.push(last.elapsed().as_secs_f32() * 1000.0);
        }
        last = Instant::now();
        if let Some(g) = grab {
            if frame == C + 90 {
                tw.grab(g, now);
            }
            if (C + 90..C + 130).contains(&frame) {
                let k = (frame - C - 90) as f32 / 40.0;
                tw.drag_to(g + Vec2::new(-260.0, 140.0) * k, now);
            }
            if frame == C + 130 {
                tw.release();
            }
        }
        if frame == C + 150 {
            tw.request_resume(now, 0.0);
        }
        for ev in tw.update(now) {
            match ev {
                TwEvent::Freeze => {
                    let f0 = Instant::now();
                    if !overlay.freeze() {
                        anyhow::bail!("screen capture failed");
                    }
                    freeze_ms = f0.elapsed().as_secs_f32() * 1000.0;
                    tw.frozen(Instant::now(), windows.clone(), &cfg);
                }
                TwEvent::Unfreeze => {
                    overlay.unfreeze();
                    tw.unfrozen(now, center);
                }
                _ => {}
            }
        }
        let mut batch = Batch::new(origin, size);
        tw.draw(&mut batch, now, &cfg, Some(center));
        let world = tw.world(now, max_radius);
        let crops = tw.crops(now);
        let grade = Grade {
            time: frame as f32 * 0.016,
            ..Grade::default()
        };
        overlay.pump();
        let _ = overlay.render(&batch, world.as_ref(), &crops, &grade);
        if !measure && [40u32, 85, 108, 116, C + 6, C + 16, C + 80, C + 115, C + 158].contains(&frame) {
            overlay.dump_bmp(&dir.join(format!("tw_{frame:03}.bmp")))?;
            saved += 1;
        }
        if !measure {
            std::thread::sleep(Duration::from_millis(4));
        }
    }
    overlay.set_visible(false);
    if measure {
        let mut sorted = times.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let avg = times.iter().sum::<f32>() / times.len().max(1) as f32;
        let p99 = sorted[(sorted.len() as f32 * 0.99) as usize..].first().copied().unwrap_or(0.0);
        let max = sorted.last().copied().unwrap_or(0.0);
        let over = times.iter().filter(|t| **t > avg * 1.6).count();
        let report = format!(
            "frames {} avg {avg:.2} ms ({:.0} fps) p99 {p99:.2} ms max {max:.2} ms, hitches(>1.6x avg) {over}, freeze {freeze_ms:.1} ms",
            times.len(),
            1000.0 / avg
        );
        tracing::info!("render timing: {report}");
        std::fs::write(dir.join("timing.txt"), report)?;
    }
    tracing::info!("render test tw: {saved} frames saved to {}", dir.display());
    Ok(())
}

/// Load an uncompressed 24/32-bit BMP as top-down BGRA.
fn load_bmp(path: &std::path::Path) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let b = std::fs::read(path)?;
    anyhow::ensure!(b.len() > 54 && &b[0..2] == b"BM", "not a BMP");
    let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    let off = u32_at(10) as usize;
    let w = u32_at(18) as i32;
    let h = u32_at(22) as i32;
    let bpp = u16::from_le_bytes([b[28], b[29]]) as usize;
    anyhow::ensure!(bpp == 24 || bpp == 32, "unsupported BMP depth {bpp}");
    let (w, flip) = (w.unsigned_abs(), h > 0);
    let h = h.unsigned_abs();
    let row = (w as usize * bpp / 8).div_ceil(4) * 4;
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h as usize {
        let src_y = if flip { h as usize - 1 - y } else { y };
        for x in 0..w as usize {
            let s = off + src_y * row + x * bpp / 8;
            let d = (y * w as usize + x) * 4;
            out[d..d + 3].copy_from_slice(&b[s..s + 3]);
            out[d + 3] = 255;
        }
    }
    Ok((out, w, h))
}

/// README material (`cursorfect.exe --demo-shots <bg.bmp> <bg_moved.bmp> <folder>`): renders
/// both effects over a generated demo desktop (never the real screen) with the overlay
/// hidden, and saves every other frame (30 fps) as .bmp.
pub fn demo_shots(
    bg: &std::path::Path,
    bg_moved: &std::path::Path,
    dir: &std::path::Path,
) -> anyhow::Result<()> {
    use crate::effects::{Look, World};
    use crate::platform::wm::WinInfo;
    std::fs::create_dir_all(dir)?;
    let (img, w, h) = load_bmp(bg)?;
    let (img2, w2, h2) = load_bmp(bg_moved)?;
    let mut overlay = Overlay::create()?;
    overlay.set_demo_desktop(&img, w, h)?;
    let settings = Settings::default();
    let mut cfg = effect_config(&settings);
    cfg.world_supported = true;
    cfg.hdr = overlay.hdr_available();
    let mut sprite = None;
    if let Some(hc) = native::arrow_cursor_handle() {
        if let Some(ci) = native::capture_cursor(hc) {
            overlay.set_cursor(&ci);
            sprite = Some(crate::effects::CursorSprite {
                size: Vec2::new(ci.width as f32, ci.height as f32),
                hotspot: ci.hotspot,
            });
        }
    }
    cfg.cursor = sprite;
    cfg.scale = 1.0;
    let size = overlay.size();
    let origin = overlay.origin();
    let max_radius = size.length();
    let dt = Duration::from_secs_f32(1.0 / 60.0);
    let draw_cursor = |batch: &mut Batch, p: Vec2| {
        if let Some(cs) = sprite {
            batch.sprite(p - cs.hotspot * 1.35, cs.size * 1.35, [1.0, 1.0, 1.0], 1.0, 0.0);
        }
    };
    let idle_world = |c: Vec2, look: Look| World {
        center: c,
        radius: 0.0,
        feather: 1.0,
        amount: 0.002,
        invert: false,
        contrast: 1.0,
        front: 0.0,
        glitch: 0.0,
        ripple: (c, 0.0, 0.0),
        look,
    };
    let grade = |f: u32| Grade {
        time: f as f32 / 60.0,
        bloom_strength: 0.55,
        grain: 0.0,
        ..Grade::default()
    };

    // ---------------------------------------------------------------- Sandevistan
    let mut fx = Effects::new();
    let mut tracker = CursorTracker::new();
    let t0 = Instant::now();
    let path = |f: f32| -> Vec2 {
        origin
            + Vec2::new(
                960.0 + (f * 1.9).sin() * 620.0 + (f * 4.3).sin() * 60.0,
                560.0 + (f * 2.7).cos() * 230.0,
            )
    };
    for frame in 0..230u32 {
        let now = t0 + dt * frame;
        let f = frame as f32 / 60.0;
        let p = path(f);
        tracker.push(p, now, false);
        if frame == 18 {
            fx.activate(p, now, &cfg);
        }
        if frame == 132 {
            let dir = (path(f) - path(f - 0.05)).normalize_or_zero();
            let to = p + dir * 330.0;
            tracker.push(to, now, true);
            fx.dash(p, to, now, &cfg);
        }
        if frame == 196 {
            fx.deactivate(p, now);
        }
        fx.update(now, &tracker, &cfg);
        let mut batch = Batch::new(origin, size);
        fx.draw(&mut batch, &tracker, now, &cfg, max_radius);
        draw_cursor(&mut batch, tracker.current().unwrap_or(p));
        let world = fx
            .world(now, max_radius, &cfg)
            .unwrap_or_else(|| idle_world(p, Look::Sandevistan));
        let _ = overlay.render(&batch, Some(&world), &[], &grade(frame));
        if frame % 2 == 0 {
            overlay.dump_bmp(&dir.join(format!("sv_{:03}.bmp", frame / 2)))?;
        }
    }

    // ---------------------------------------------------------------- The World
    let windows = vec![
        WinInfo {
            hwnd: 1,
            rect: WinRect {
                l: 160.0,
                t: 140.0,
                r: 780.0,
                b: 570.0,
            },
            class: "Demo".into(),
        },
        WinInfo {
            hwnd: 2,
            rect: WinRect {
                l: 1040.0,
                t: 300.0,
                r: 1600.0,
                b: 680.0,
            },
            class: "Demo".into(),
        },
    ];
    let mon = WinRect {
        l: origin.x,
        t: origin.y,
        r: origin.x + size.x,
        b: origin.y + size.y,
    };
    let mut tw = TimeStop::new();
    let t0 = Instant::now();
    let rest = Vec2::new(1180.0, 470.0);
    let grab = Vec2::new(1200.0, 318.0);
    let drop = grab + Vec2::new(-340.0, 250.0);
    let mut cursor = Vec2::new(1500.0, 820.0);
    let mut swapped = false;
    for frame in 0..330u32 {
        let now = t0 + dt * frame;
        // Choreography: drift to the player, hold during the wind-up, grab the title bar,
        // drag it down-left, rest.
        cursor = match frame {
            0..=40 => cursor.lerp(rest, 0.08),
            41..=165 => cursor.lerp(rest, 0.2),
            166..=185 => cursor.lerp(grab, 0.25),
            186..=245 => {
                let k = ((frame - 186) as f32 / 59.0).clamp(0.0, 1.0);
                grab.lerp(drop, k * k * (3.0 - 2.0 * k))
            }
            _ => cursor.lerp(drop + Vec2::new(40.0, 60.0), 0.05),
        };
        if frame == 30 {
            tw.begin(now, cursor, 1.15, TwOptions::default(), mon);
        }
        if frame == 186 {
            tw.grab(grab, now);
        }
        if (186..=246).contains(&frame) {
            tw.drag_to(cursor, now);
        }
        if frame == 247 {
            tw.release();
        }
        if frame == 268 {
            tw.request_resume(now, 0.0);
        }
        for ev in tw.update(now) {
            match ev {
                TwEvent::Freeze => {
                    overlay.freeze();
                    tw.frozen(now, windows.clone(), &cfg);
                }
                TwEvent::Unfreeze => {
                    // The "real" window has moved now: show the desktop with it in place.
                    overlay.unfreeze();
                    overlay.set_demo_desktop(&img2, w2, h2)?;
                    swapped = true;
                    tw.unfrozen(now, cursor);
                }
                _ => {}
            }
        }
        let mut batch = Batch::new(origin, size);
        tw.draw(&mut batch, now, &cfg, Some(cursor));
        draw_cursor(&mut batch, cursor);
        let crops = tw.crops(now);
        let world = tw.world(now, max_radius).unwrap_or_else(|| {
            idle_world(
                cursor,
                Look::TimeStop {
                    inversion: 0.0,
                    flash: 0.0,
                    tint: 1.0,
                    chroma: 0.0,
                    pinch: 0.0,
                    primary: crate::effects::the_world::palette(crate::audio::dubs::Dub::DioJp).0,
                    secondary: crate::effects::the_world::palette(crate::audio::dubs::Dub::DioJp).1,
                },
            )
        });
        let _ = overlay.render(&batch, Some(&world), &crops, &grade(frame));
        if frame % 2 == 0 {
            overlay.dump_bmp(&dir.join(format!("tw_{:03}.bmp", frame / 2)))?;
        }
    }
    tracing::info!("demo shots saved to {} (moved window shown: {swapped})", dir.display());
    Ok(())
}

