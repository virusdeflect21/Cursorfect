//! Real desktop windows for The World: which ones can be grabbed during a time stop, and
//! moving them (all at once) when time resumes.
//! Dropped windows are raised in drop order to match the frozen preview.
//!
//! # HWND ownership
//! The HWNDs belong to other processes. They are only stored as plain integers, re-validated
//! with `IsWindow` before use, and only ever passed to `SetWindowPos` with
//! `SWP_ASYNCWINDOWPOS`, so a hung or closed window can never block or crash us. Windows of
//! elevated processes reject the move (UIPI); that failure is reported and skipped.

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowPlacement, ShowWindowAsync, SW_RESTORE, WINDOWPLACEMENT,
    EnumWindows, GWL_EXSTYLE, GetClassNameW, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextLengthW, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible,
    IsZoomed, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOSIZE, HWND_TOP,
    SetWindowPos, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};
use windows::core::BOOL;

/// Rectangle in virtual-desktop pixels: left, top, right, bottom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WinRect {
    pub l: f32,
    pub t: f32,
    pub r: f32,
    pub b: f32,
}

impl WinRect {
    pub fn w(&self) -> f32 {
        self.r - self.l
    }

    pub fn h(&self) -> f32 {
        self.b - self.t
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }

    pub fn offset(&self, dx: f32, dy: f32) -> WinRect {
        WinRect {
            l: self.l + dx,
            t: self.t + dy,
            r: self.r + dx,
            b: self.b + dy,
        }
    }

    fn from_rect(r: RECT) -> Self {
        Self {
            l: r.left as f32,
            t: r.top as f32,
            r: r.right as f32,
            b: r.bottom as f32,
        }
    }
}

/// A grabbable window as it looked when time stopped.
#[derive(Clone, Debug)]
pub struct WinInfo {
    pub hwnd: isize,
    /// Visible frame (without the invisible resize borders), used for hit-testing and drawing.
    pub rect: WinRect,
    pub class: String,
}

/// What kind of window this is, reduced to the facts the filter needs (testable).
#[derive(Clone, Debug, Default)]
pub struct WindowFacts {
    pub visible: bool,
    pub minimized: bool,
    pub maximized: bool,
    pub cloaked: bool,
    pub own_process: bool,
    pub tool_window: bool,
    pub app_window: bool,
    pub no_activate: bool,
    pub has_title: bool,
    pub class: String,
    pub width: f32,
    pub height: f32,
}

/// Shell surfaces that must never be dragged.
const SHELL_CLASSES: [&str; 5] = [
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "XamlExplorerHostIslandWindow",
];

/// Ordinary, visible, movable application windows only.
pub fn eligible(f: &WindowFacts) -> bool {
    if !f.visible || f.minimized || f.cloaked || f.own_process {
        return false;
    }
    if f.tool_window && !f.app_window {
        return false;
    }
    if f.no_activate && !f.app_window {
        return false;
    }
    if !f.has_title || SHELL_CLASSES.contains(&f.class.as_str()) {
        return false;
    }
    f.width >= 60.0 && f.height >= 40.0
}

struct EnumCtx {
    pid: u32,
    out: Vec<WinInfo>,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam is the &mut EnumCtx passed by `top_level_windows`, alive for the call.
    let ctx = unsafe { &mut *(lparam.0 as *mut EnumCtx) };
    // SAFETY: plain queries on a window handle supplied by EnumWindows; out-params are locals.
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            std::mem::size_of::<u32>() as u32,
        );
        let mut frame = RECT::default();
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut frame as *mut RECT).cast(),
            std::mem::size_of::<RECT>() as u32,
        )
        .is_err()
        {
            let _ = GetWindowRect(hwnd, &mut frame);
        }
        let mut class_buf = [0u16; 128];
        let n = GetClassNameW(hwnd, &mut class_buf).max(0) as usize;
        let class = String::from_utf16_lossy(&class_buf[..n.min(class_buf.len())]);
        let rect = WinRect::from_rect(frame);
        let facts = WindowFacts {
            visible: IsWindowVisible(hwnd).as_bool(),
            minimized: IsIconic(hwnd).as_bool(),
            maximized: IsZoomed(hwnd).as_bool(),
            cloaked: cloaked != 0,
            own_process: pid == ctx.pid,
            tool_window: ex & WS_EX_TOOLWINDOW.0 != 0,
            app_window: ex & WS_EX_APPWINDOW.0 != 0,
            no_activate: ex & WS_EX_NOACTIVATE.0 != 0,
            has_title: GetWindowTextLengthW(hwnd) > 0,
            class: class.clone(),
            width: rect.w(),
            height: rect.h(),
        };
        if eligible(&facts) {
            ctx.out.push(WinInfo {
                hwnd: hwnd.0 as isize,
                rect,
                class,
            });
        }
    }
    true.into()
}

/// Grabbable top-level windows, topmost first (EnumWindows walks the Z-order top-down).
pub fn top_level_windows() -> Vec<WinInfo> {
    let mut ctx = EnumCtx {
        pid: std::process::id(),
        out: Vec::new(),
    };
    // SAFETY: the callback only uses `ctx`, which outlives the call.
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut EnumCtx as isize));
    }
    ctx.out
}

/// Move windows by (dx, dy) pixels each, restoring maximized windows and raising dropped windows to match the preview. Returns how many moved.
/// Invalid (closed) or protected windows are skipped.
pub fn apply_moves(moves: &[(isize, f32, f32)]) -> usize {
    let mut done = 0;
    for &(raw, dx, dy) in moves {
        if dx.abs() < 0.5 && dy.abs() < 0.5 {
            continue;
        }
        let hwnd = HWND(raw as *mut core::ffi::c_void);
        // SAFETY: the handle is validated with IsWindow first; SetWindowPos with
        // SWP_ASYNCWINDOWPOS never waits for the target thread.
        unsafe {
            if !IsWindow(Some(hwnd)).as_bool() {
                tracing::info!("window {raw:#x} closed during the time stop; skipped");
                continue;
            }
            let mut r = RECT::default();
            if GetWindowRect(hwnd, &mut r).is_err() {
                continue;
            }
            let x = r.left + dx.round() as i32;
            let y = r.top + dy.round() as i32;
            let mut flags = SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS;
            let (mut width, mut height) = (0, 0);
            if IsZoomed(hwnd).as_bool() {
                let mut placement = WINDOWPLACEMENT {
                    length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                    ..Default::default()
                };
                if GetWindowPlacement(hwnd, &mut placement).is_err() {
                    continue;
                }
                width = placement.rcNormalPosition.right - placement.rcNormalPosition.left;
                height = placement.rcNormalPosition.bottom - placement.rcNormalPosition.top;
                // Queue restoration before positioning on the owning thread. A maximized
                // window otherwise ignores the requested desktop position.
                if !ShowWindowAsync(hwnd, SW_RESTORE).as_bool() {
                    continue;
                }
            } else {
                flags |= SWP_NOSIZE;
            }
            match SetWindowPos(
                hwnd,
                Some(HWND_TOP),
                x,
                y,
                width,
                height,
                flags,
            ) {
                Ok(()) => done += 1,
                Err(e) => tracing::warn!("could not move window {raw:#x}: {e}"),
            }
        }
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normal() -> WindowFacts {
        WindowFacts {
            visible: true,
            has_title: true,
            class: "Notepad".into(),
            width: 800.0,
            height: 600.0,
            ..Default::default()
        }
    }

    #[test]
    fn ordinary_windows_are_eligible() {
        assert!(eligible(&normal()));
    }

    #[test]
    fn hidden_minimized_cloaked_and_own_windows_are_not() {
        for f in [
            WindowFacts {
                visible: false,
                ..normal()
            },
            WindowFacts {
                minimized: true,
                ..normal()
            },
            WindowFacts {
                cloaked: true,
                ..normal()
            },
            WindowFacts {
                own_process: true,
                ..normal()
            },
        ] {
            assert!(!eligible(&f), "{f:?}");
        }
    }

    #[test]
    fn shell_tool_and_tiny_windows_are_not() {
        assert!(!eligible(&WindowFacts {
            class: "Shell_TrayWnd".into(),
            ..normal()
        }));
        assert!(!eligible(&WindowFacts {
            class: "Progman".into(),
            ..normal()
        }));
        assert!(!eligible(&WindowFacts {
            tool_window: true,
            ..normal()
        }));
        // A tool window that asks to be an app window is fine.
        assert!(eligible(&WindowFacts {
            tool_window: true,
            app_window: true,
            ..normal()
        }));
        assert!(!eligible(&WindowFacts {
            width: 30.0,
            ..normal()
        }));
        assert!(!eligible(&WindowFacts {
            has_title: false,
            ..normal()
        }));
    }

    #[test]
    fn maximized_and_uwp_apps_are_grabbable() {
        assert!(eligible(&WindowFacts { maximized: true, ..normal() }));
        assert!(eligible(&WindowFacts { class: "Windows.UI.Core.CoreWindow".into(), ..normal() }));
    }

    #[test]
    fn rect_math() {
        let r = WinRect {
            l: -100.0,
            t: 10.0,
            r: 100.0,
            b: 110.0,
        };
        assert!(r.contains(-100.0, 10.0));
        assert!(!r.contains(100.0, 50.0));
        let o = r.offset(50.0, -10.0);
        assert_eq!((o.l, o.t, o.w(), o.h()), (-50.0, 0.0, 200.0, 100.0));
    }
}

