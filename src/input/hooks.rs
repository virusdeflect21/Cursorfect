//! Global low-level keyboard/mouse hooks (`WH_MOUSE_LL`, `WH_KEYBOARD_LL`).
//!
//! # Design / safety invariants
//! * Hook callbacks run on this module's dedicated thread, which owns a Win32
//!   message loop (a requirement for low-level hooks). The callbacks do **no**
//!   rendering, disk I/O or audio: they match shortcuts against a small
//!   in-memory table, push lightweight events through an `mpsc` channel and
//!   return immediately.
//! * The callbacks are plain `extern "system"` functions, so they reach their
//!   state through the process-wide [`SHARED`] `OnceLock`. The pointer passed in
//!   `lParam` is only valid for the duration of the callback and is never stored.
//! * Panics must not unwind across the FFI boundary, so every callback body runs
//!   under `catch_unwind`.
//! * Hooks are always removed (`UnhookWindowsHookEx`) before the thread exits.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::{Context, anyhow};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentThread, GetCurrentThreadId, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey,
    UnregisterHotKey, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, MSG,
    MSLLHOOKSTRUCT, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_HOTKEY, WM_QUIT,
};

use super::shortcuts::{
    Modifiers, MouseButton, Shortcut, Trigger, VK_ESCAPE, VK_F12, is_emergency, is_modifier_vk,
};
use crate::app::Msg;

/// Events produced by the hooks for the application engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    ActivationDown,
    ActivationUp,
    DashDown,
    /// Ctrl+Alt+Shift+F12.
    Emergency,
    /// A shortcut was captured while recording.
    Recorded(Shortcut),
    RecordCancelled,
    /// The cursor moved while the engine was idle: wake up and drain samples.
    MouseWake,
    /// Time is stopped: left button pressed / released at a screen position (swallowed).
    Grab { down: bool, x: i32, y: i32 },
    /// Time is stopped: Esc (cancel the current drag).
    GrabCancel,
    /// Time is stopped: Backspace (undo the last queued move).
    GrabUndo,
}

#[derive(Clone, Copy, Debug)]
pub struct Bindings {
    pub activation: Shortcut,
    pub dash: Shortcut,
    pub consume_mouse: bool,
}

pub const RECORD_NONE: u8 = 0;
pub const RECORD_ACTIVATION: u8 = 1;
pub const RECORD_DASH: u8 = 2;

pub struct HookShared {
    /// Master switch: when false, no activation/dash events are produced and no input is swallowed.
    pub master: AtomicBool,
    /// Non-zero while the UI is recording a shortcut (see `RECORD_*`).
    pub recording: AtomicU8,
    /// Wake the engine on mouse movement (only needed while effects are active).
    pub wake_on_move: AtomicBool,
    /// The World: time is stopped. Clicks and the wheel are swallowed (apps underneath must
    /// not react) and turned into grab events. Shared with the watchdog, which can clear it.
    pub time_stopped: Arc<AtomicBool>,
    bindings: RwLock<Bindings>,
    act_down: AtomicBool,
    dash_down: AtomicBool,
    /// Bit 0: X1, bit 1: X2, bit 2: middle swallowed (so the matching button-up is swallowed too).
    swallowed: AtomicU8,
    swallowed_vk: AtomicU32,
    moves: Mutex<VecDeque<(i32, i32, Instant)>>,
    wake_pending: AtomicBool,
    tx: Sender<Msg>,
}

const MOVE_QUEUE_CAP: usize = 1024;

impl HookShared {
    pub fn new(tx: Sender<Msg>, bindings: Bindings) -> Arc<Self> {
        Arc::new(Self {
            master: AtomicBool::new(false),
            recording: AtomicU8::new(RECORD_NONE),
            wake_on_move: AtomicBool::new(false),
            time_stopped: Arc::new(AtomicBool::new(false)),
            bindings: RwLock::new(bindings),
            act_down: AtomicBool::new(false),
            dash_down: AtomicBool::new(false),
            swallowed: AtomicU8::new(0),
            swallowed_vk: AtomicU32::new(0),
            moves: Mutex::new(VecDeque::with_capacity(MOVE_QUEUE_CAP)),
            wake_pending: AtomicBool::new(false),
            tx,
        })
    }

    pub fn set_bindings(&self, b: Bindings) {
        if let Ok(mut w) = self.bindings.write() {
            *w = b;
        }
    }

    /// Drain queued cursor samples (engine thread).
    pub fn drain_moves(&self, out: &mut Vec<(i32, i32, Instant)>) {
        self.wake_pending.store(false, Ordering::Release);
        if let Ok(mut q) = self.moves.lock() {
            out.extend(q.drain(..));
        }
    }

    pub fn clear_moves(&self) {
        if let Ok(mut q) = self.moves.lock() {
            q.clear();
        }
    }

    fn send(&self, ev: InputEvent) {
        // A closed channel just means the app is shutting down.
        let _ = self.tx.send(Msg::Input(ev));
    }

    fn bindings(&self) -> Option<Bindings> {
        // Never block the hook: if the engine is mid-update, fall back to "no match".
        self.bindings.try_read().ok().map(|b| *b)
    }

    /// Returns true if the event must be swallowed.
    fn on_mouse(&self, msg: u32, info: &MSLLHOOKSTRUCT) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::{
            WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
            WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDBLCLK, WM_RBUTTONDOWN,
            WM_RBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
        };
        match msg {
            WM_MOUSEMOVE => {
                if self.master.load(Ordering::Relaxed) {
                    if let Ok(mut q) = self.moves.try_lock() {
                        if q.len() == MOVE_QUEUE_CAP {
                            q.pop_front();
                        }
                        q.push_back((info.pt.x, info.pt.y, Instant::now()));
                    }
                    if self.wake_on_move.load(Ordering::Relaxed)
                        && !self.wake_pending.swap(true, Ordering::AcqRel)
                    {
                        self.send(InputEvent::MouseWake);
                    }
                }
                false
            }
            WM_XBUTTONDOWN | WM_XBUTTONUP => {
                let button = match (info.mouseData >> 16) & 0xFFFF {
                    1 => MouseButton::X1,
                    2 => MouseButton::X2,
                    _ => return false,
                };
                if msg == WM_XBUTTONDOWN {
                    self.mouse_down(button)
                } else {
                    self.mouse_up(button)
                }
            }
            WM_MBUTTONDOWN => self.mouse_down(MouseButton::Middle),
            WM_MBUTTONUP => self.mouse_up(MouseButton::Middle),
            WM_LBUTTONDOWN | WM_LBUTTONUP if self.time_stopped.load(Ordering::Acquire) => {
                self.send(InputEvent::Grab {
                    down: msg == WM_LBUTTONDOWN,
                    x: info.pt.x,
                    y: info.pt.y,
                });
                true
            }
            WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MOUSEWHEEL | WM_MOUSEHWHEEL | WM_LBUTTONDBLCLK
            | WM_RBUTTONDBLCLK
                if self.time_stopped.load(Ordering::Acquire) =>
            {
                true // frozen world: nothing underneath may react
            }
            _ => false,
        }
    }

    fn mouse_down(&self, button: MouseButton) -> bool {
        let bit = button.bit();
        let mods = current_modifiers();
        if self.recording.load(Ordering::Acquire) != RECORD_NONE {
            self.send(InputEvent::Recorded(Shortcut::mouse(button, mods)));
            self.swallowed.fetch_or(bit, Ordering::AcqRel);
            return true;
        }
        if !self.master.load(Ordering::Relaxed) {
            return false;
        }
        let Some(b) = self.bindings() else {
            return false;
        };
        if b.activation.matches_mouse(button, mods) {
            self.act_down.store(true, Ordering::Release);
            self.send(InputEvent::ActivationDown);
        } else if b.dash.matches_mouse(button, mods) {
            self.send(InputEvent::DashDown);
        } else {
            return false;
        }
        if b.consume_mouse {
            self.swallowed.fetch_or(bit, Ordering::AcqRel);
            return true;
        }
        false
    }

    fn mouse_up(&self, button: MouseButton) -> bool {
        let bit = button.bit();
        if self.act_down.load(Ordering::Acquire) {
            if let Some(b) = self.bindings() {
                if b.activation.trigger == Trigger::Mouse(button) {
                    self.act_down.store(false, Ordering::Release);
                    self.send(InputEvent::ActivationUp);
                }
            }
        }
        let prev = self.swallowed.fetch_and(!bit, Ordering::AcqRel);
        prev & bit != 0
    }

    fn on_key(&self, msg: u32, info: &KBDLLHOOKSTRUCT) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::{
            WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
        };
        let vk = info.vkCode;
        if is_modifier_vk(vk) {
            return false; // modifiers are never swallowed
        }
        match msg {
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let mods = current_modifiers();
                if is_emergency(vk, mods) {
                    self.send(InputEvent::Emergency);
                    return false;
                }
                if self.recording.load(Ordering::Acquire) != RECORD_NONE {
                    if vk == VK_ESCAPE && !mods.any() {
                        self.send(InputEvent::RecordCancelled);
                    } else {
                        self.send(InputEvent::Recorded(Shortcut::key(vk, mods)));
                    }
                    self.swallowed_vk.store(vk, Ordering::Release);
                    return true;
                }
                if !self.master.load(Ordering::Relaxed) {
                    return false;
                }
                if self.time_stopped.load(Ordering::Acquire) && !mods.any() {
                    const VK_BACK: u32 = 0x08;
                    if vk == VK_ESCAPE {
                        self.send(InputEvent::GrabCancel);
                        self.swallowed_vk.store(vk, Ordering::Release);
                        return true;
                    }
                    if vk == VK_BACK {
                        self.send(InputEvent::GrabUndo);
                        self.swallowed_vk.store(vk, Ordering::Release);
                        return true;
                    }
                }
                let Some(b) = self.bindings() else {
                    return false;
                };
                // `swap` makes auto-repeat harmless: only the first key-down fires.
                if b.activation.matches_key(vk, mods) {
                    if !self.act_down.swap(true, Ordering::AcqRel) {
                        self.send(InputEvent::ActivationDown);
                    }
                } else if b.dash.matches_key(vk, mods)
                    && !self.dash_down.swap(true, Ordering::AcqRel)
                {
                    self.send(InputEvent::DashDown);
                }
                false
            }
            WM_KEYUP | WM_SYSKEYUP => {
                if let Some(b) = self.bindings() {
                    if b.activation.trigger == Trigger::Key(vk)
                        && self.act_down.swap(false, Ordering::AcqRel)
                    {
                        self.send(InputEvent::ActivationUp);
                    }
                    if b.dash.trigger == Trigger::Key(vk) {
                        self.dash_down.store(false, Ordering::Release);
                    }
                }
                self.swallowed_vk
                    .compare_exchange(vk, 0, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            }
            _ => false,
        }
    }
}

fn key_down(vk: u16) -> bool {
    // SAFETY: GetAsyncKeyState has no preconditions.
    unsafe { GetAsyncKeyState(i32::from(vk)) as u16 & 0x8000 != 0 }
}

pub fn current_modifiers() -> Modifiers {
    Modifiers {
        ctrl: key_down(VK_CONTROL.0),
        alt: key_down(VK_MENU.0),
        shift: key_down(VK_SHIFT.0),
        win: key_down(VK_LWIN.0) || key_down(VK_RWIN.0),
    }
}

static SHARED: OnceLock<Arc<HookShared>> = OnceLock::new();

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        if let Some(shared) = SHARED.get() {
            // SAFETY: for WH_MOUSE_LL with HC_ACTION, lParam points to a valid MSLLHOOKSTRUCT
            // that lives for the duration of this call.
            let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            let swallow = catch_unwind(AssertUnwindSafe(|| shared.on_mouse(wparam.0 as u32, info)))
                .unwrap_or(false);
            if swallow {
                return LRESULT(1);
            }
        }
    }
    // SAFETY: forwarding the unchanged hook arguments.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        if let Some(shared) = SHARED.get() {
            // SAFETY: for WH_KEYBOARD_LL with HC_ACTION, lParam points to a valid KBDLLHOOKSTRUCT.
            let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            let swallow = catch_unwind(AssertUnwindSafe(|| shared.on_key(wparam.0 as u32, info)))
                .unwrap_or(false);
            if swallow {
                return LRESULT(1);
            }
        }
    }
    // SAFETY: forwarding the unchanged hook arguments.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Owns the hook thread. Dropping (or calling [`HookThread::shutdown`]) removes the hooks.
pub struct HookThread {
    thread_id: u32,
    join: Option<JoinHandle<()>>,
}

impl HookThread {
    pub fn spawn(shared: Arc<HookShared>) -> anyhow::Result<Self> {
        SHARED
            .set(shared.clone())
            .map_err(|_| anyhow!("hooks already installed"))?;
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<anyhow::Result<u32>>();
        let join = std::thread::Builder::new()
            .name("cursorfect-hooks".into())
            .spawn(move || hook_thread_main(shared, ready_tx))
            .context("spawning hook thread")?;
        let thread_id = ready_rx
            .recv()
            .context("hook thread died during startup")??;
        Ok(Self {
            thread_id,
            join: Some(join),
        })
    }

    pub fn shutdown(&mut self) {
        if let Some(join) = self.join.take() {
            // SAFETY: posting WM_QUIT to a thread id we created; failure only means it already exited.
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            let _ = join.join();
        }
    }
}

impl Drop for HookThread {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn hook_thread_main(shared: Arc<HookShared>, ready: std::sync::mpsc::Sender<anyhow::Result<u32>>) {
    // SAFETY: all calls below are plain Win32 calls made on this thread; the hook handles are
    // unhooked before returning, and the message structs are local.
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
        // Create the thread's message queue before announcing the thread id.
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);

        let hinst = GetModuleHandleW(None).map(|h| windows::Win32::Foundation::HINSTANCE(h.0));
        let hinst = match hinst {
            Ok(h) => h,
            Err(e) => {
                let _ = ready.send(Err(anyhow!("GetModuleHandleW failed: {e}")));
                return;
            }
        };
        let mouse: HHOOK = match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(hinst), 0) {
            Ok(h) => h,
            Err(e) => {
                let _ = ready.send(Err(anyhow!("mouse hook failed: {e}")));
                return;
            }
        };
        let keyboard: HHOOK =
            match SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(hinst), 0) {
                Ok(h) => h,
                Err(e) => {
                    let _ = UnhookWindowsHookEx(mouse);
                    let _ = ready.send(Err(anyhow!("keyboard hook failed: {e}")));
                    return;
                }
            };
        // Redundant emergency path that survives a silently-dropped low-level hook.
        let hotkey_ok = RegisterHotKey(
            None,
            1,
            MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_NOREPEAT,
            VK_F12,
        )
        .map_err(|e| {
            tracing::warn!("emergency RegisterHotKey failed (hook path still active): {e}")
        })
        .is_ok();
        let _ = ready.send(Ok(GetCurrentThreadId()));

        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_HOTKEY {
                shared.send(InputEvent::Emergency);
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        if hotkey_ok {
            let _ = UnregisterHotKey(None, 1);
        }
        let _ = UnhookWindowsHookEx(keyboard);
        let _ = UnhookWindowsHookEx(mouse);
    }
    tracing::info!("input hooks released");
}
