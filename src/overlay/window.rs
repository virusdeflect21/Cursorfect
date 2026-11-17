//! The transparent, borderless, click-through, topmost overlay window.
//!
//! # Safety notes
//! * The `HWND` is created and destroyed on the engine thread only, and is owned by
//!   [`OverlayWindow`]; `Drop` destroys it exactly once.
//! * The window procedure is a plain function that touches only process-wide atomics.
//! * Click-through is guaranteed twice: `WS_EX_TRANSPARENT | WS_EX_LAYERED` and a
//!   `WM_NCHITTEST -> HTTRANSPARENT` reply. `WS_EX_NOACTIVATE` + `WM_MOUSEACTIVATE ->
//!   MA_NOACTIVATE` guarantee the overlay can never take focus.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, anyhow};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetSystemMetrics,
    HTTRANSPARENT, HWND_TOPMOST, LWA_ALPHA, MSG, PM_REMOVE, PeekMessageW, RegisterClassExW,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOREDRAW, SetLayeredWindowAttributes,
    SetWindowDisplayAffinity, SetWindowPos, ShowWindow, TranslateMessage, UnregisterClassW,
    WDA_EXCLUDEFROMCAPTURE, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_MOUSEACTIVATE, WM_NCHITTEST,
    WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

static DISPLAY_CHANGED: AtomicBool = AtomicBool::new(false);

const CLASS_NAME: PCWSTR = w!("CursorFectOverlay");

/// Bounds of the whole virtual desktop in physical pixels (may start at negative coordinates).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VirtualScreen {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl VirtualScreen {
    pub fn query() -> Self {
        // SAFETY: GetSystemMetrics has no preconditions.
        unsafe {
            Self {
                x: GetSystemMetrics(SM_XVIRTUALSCREEN),
                y: GetSystemMetrics(SM_YVIRTUALSCREEN),
                w: GetSystemMetrics(SM_CXVIRTUALSCREEN).clamp(1, 16384),
                // One pixel short: a swap chain exactly covering a monitor can be promoted to
                // independent flip, and the switch back to composition flashes black.
                h: (GetSystemMetrics(SM_CYVIRTUALSCREEN) - 1).clamp(1, 16384),
            }
        }
    }
}

/// Returns (and clears) the "display configuration changed" flag set by the window procedure.
pub fn take_display_changed() -> bool {
    DISPLAY_CHANGED.swap(false, Ordering::AcqRel)
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(3), // MA_NOACTIVATE
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            DISPLAY_CHANGED.store(true, Ordering::Release);
            // SAFETY: forwarding unchanged arguments to the default procedure.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        // SAFETY: forwarding unchanged arguments to the default procedure.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

pub struct OverlayWindow {
    hwnd: HWND,
    hinstance: HINSTANCE,
    pub screen: VirtualScreen,
    visible: bool,
    /// True if the window is excluded from screen capture (needed for the world pass).
    pub capture_excluded: bool,
}

impl OverlayWindow {
    pub fn create() -> anyhow::Result<Self> {
        let screen = VirtualScreen::query();
        // SAFETY: standard window-class registration and window creation on the calling thread.
        // `wnd_proc` is `extern "system"` with the correct signature and `'static` lifetime.
        unsafe {
            let hinstance = HINSTANCE(GetModuleHandleW(None).context("GetModuleHandleW")?.0);
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wnd_proc),
                hInstance: hinstance,
                lpszClassName: CLASS_NAME,
                ..Default::default()
            };
            // Registering twice (after a device-lost recreation) harmlessly fails; ignore the atom.
            let _ = RegisterClassExW(&class);

            let ex = WS_EX_TOPMOST
                | WS_EX_LAYERED
                | WS_EX_TRANSPARENT
                | WS_EX_NOACTIVATE
                | WS_EX_TOOLWINDOW
                | WS_EX_NOREDIRECTIONBITMAP;
            let hwnd = CreateWindowExW(
                ex,
                CLASS_NAME,
                w!("CursorFect Overlay"),
                WS_POPUP,
                screen.x,
                screen.y,
                screen.w,
                screen.h,
                None,
                None,
                Some(hinstance),
                None,
            )
            .context("CreateWindowExW")?;
            if hwnd.is_invalid() {
                return Err(anyhow!("overlay window handle is invalid"));
            }
            // Layered windows need an alpha setting to be shown; opaque (255) because the
            // per-pixel alpha comes from the DirectComposition swap chain.
            let _ = SetLayeredWindowAttributes(hwnd, Default::default(), 255, LWA_ALPHA);
            // Hide the overlay from Desktop Duplication / screen capture so the black & white
            // world pass never re-captures its own output (Windows 10 2004+).
            let capture_excluded = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)
                .map_err(|e| {
                    tracing::warn!(
                        "WDA_EXCLUDEFROMCAPTURE unavailable ({e}); world desaturation disabled"
                    )
                })
                .is_ok();
            Ok(Self {
                hwnd,
                hinstance,
                screen,
                visible: false,
                capture_excluded,
            })
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn set_visible(&mut self, show: bool) {
        if show == self.visible {
            return;
        }
        self.visible = show;
        // SAFETY: valid HWND owned by `self`.
        unsafe {
            if show {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                self.raise();
            } else {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }

    /// Re-assert topmost without activating (other topmost windows may have been shown since).
    pub fn raise(&self) {
        // SAFETY: valid HWND owned by `self`.
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                self.screen.x,
                self.screen.y,
                self.screen.w,
                self.screen.h,
                SWP_NOACTIVATE | SWP_NOREDRAW,
            );
        }
    }

    /// Process pending messages for this thread's windows (never blocks).
    pub fn pump(&self) {
        // SAFETY: standard non-blocking message pump on the thread that owns the window.
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        // SAFETY: destroys the window we created, on the thread that created it.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            let _ = UnregisterClassW(CLASS_NAME, Some(self.hinstance));
        }
    }
}
