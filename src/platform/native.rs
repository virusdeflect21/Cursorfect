//! Small, safe wrappers around the Win32 calls the engine needs.
//! Each `unsafe` block is a plain FFI call with no pointer lifetime hazards beyond
//! the local out-parameters it passes.

use glam::Vec2;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, LPARAM, RECT,
};
use windows::Win32::Graphics::Gdi::{
    DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplayDevicesW, EnumDisplayMonitors,
    EnumDisplaySettingsW, DISPLAY_DEVICEW, HDC, HMONITOR,
};
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW, RegOpenKeyExW,
    RegSetValueExW,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent,
    WaitForSingleObject,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SM_CXCURSOR, SetCursorPos,
};
use windows::core::{BOOL, PCWSTR, w};

use crate::input::cursor::Rect;

/// Opt in to per-monitor-v2 DPI awareness so every coordinate is a physical pixel.
/// (Also requested by the embedded manifest; this is the fallback.)
pub fn set_dpi_awareness() {
    // SAFETY: no preconditions; fails harmlessly if awareness is already set.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

pub fn cursor_pos() -> Option<Vec2> {
    let mut p = Default::default();
    // SAFETY: `p` is a valid out-pointer.
    unsafe {
        GetCursorPos(&mut p)
            .ok()
            .map(|_| Vec2::new(p.x as f32, p.y as f32))
    }
}

pub fn set_cursor_pos(p: Vec2) -> bool {
    // SAFETY: plain call with value arguments.
    unsafe { SetCursorPos(p.x.round() as i32, p.y.round() as i32).is_ok() }
}

/// True while the left or right button (or the middle one, unless it is the button that
/// triggered the dash: `ignore_middle`) is physically held, e.g. during a drag.
pub fn any_mouse_button_down(ignore_middle: bool) -> bool {
    let buttons: &[_] = if ignore_middle {
        &[VK_LBUTTON, VK_RBUTTON]
    } else {
        &[VK_LBUTTON, VK_RBUTTON, VK_MBUTTON]
    };
    // SAFETY: no preconditions.
    unsafe {
        buttons
            .iter()
            .any(|vk| GetAsyncKeyState(i32::from(vk.0)) as u16 & 0x8000 != 0)
    }
}

/// Highest refresh rate among the attached displays (Hz), used for the "auto" frame cap.
pub fn max_refresh_rate() -> u32 {
    let mut best = 0u32;
    // SAFETY: DISPLAY_DEVICEW / DEVMODEW are local out-structs with their size fields set.
    unsafe {
        let mut i = 0;
        loop {
            let mut dd = DISPLAY_DEVICEW {
                cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };
            if !EnumDisplayDevicesW(PCWSTR::null(), i, &mut dd, 0).as_bool() {
                break;
            }
            i += 1;
            let mut dm = DEVMODEW {
                dmSize: std::mem::size_of::<DEVMODEW>() as u16,
                ..Default::default()
            };
            if EnumDisplaySettingsW(
                PCWSTR(dd.DeviceName.as_ptr()),
                ENUM_CURRENT_SETTINGS,
                &mut dm,
            )
            .as_bool()
            {
                best = best.max(dm.dmDisplayFrequency);
            }
            if i > 32 {
                break;
            }
        }
    }
    if best <= 1 { 60 } else { best }
}

/// Current cursor width relative to the 32px default (DPI / accessibility cursor size).
pub fn cursor_scale() -> f32 {
    // SAFETY: no preconditions.
    let w = unsafe { GetSystemMetrics(SM_CXCURSOR) };
    if w <= 0 {
        1.0
    } else {
        (w as f32 / 32.0).clamp(0.75, 4.0)
    }
}

unsafe extern "system" fn monitor_proc(
    _m: HMONITOR,
    _dc: HDC,
    rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` is the `&mut Vec<Rect>` passed by `monitors()`, alive for the whole enumeration;
    // `rect` is valid for the duration of the callback.
    unsafe {
        let list = &mut *(data.0 as *mut Vec<Rect>);
        let r = &*rect;
        list.push(Rect {
            left: r.left as f32,
            top: r.top as f32,
            right: r.right as f32,
            bottom: r.bottom as f32,
        });
    }
    true.into()
}

/// All monitor rectangles in virtual-desktop pixels (negative coordinates possible).
pub fn monitors() -> Vec<Rect> {
    let mut list: Vec<Rect> = Vec::new();
    // SAFETY: the callback only dereferences the pointer to `list`, which outlives the call.
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(monitor_proc),
            LPARAM(&mut list as *mut _ as isize),
        );
    }
    list
}

/// The real Windows cursor image, extracted so afterimages are copies of *your* cursor.
#[derive(Clone)]
pub struct CursorImage {
    /// Straight-alpha RGBA, row-major, top-down.
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub hotspot: Vec2,
    pub handle: isize,
}

/// Handle of the cursor currently shown (changes when the shape changes: arrow, I-beam, hand...).
pub fn current_cursor_handle() -> Option<isize> {
    use windows::Win32::UI::WindowsAndMessaging::{CURSOR_SHOWING, CURSORINFO, GetCursorInfo};
    let mut ci = CURSORINFO {
        cbSize: std::mem::size_of::<CURSORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `ci` is a valid, correctly sized out-struct.
    unsafe { GetCursorInfo(&mut ci).ok()? };
    if ci.flags.0 & CURSOR_SHOWING.0 == 0 || ci.hCursor.is_invalid() {
        return None;
    }
    Some(ci.hCursor.0 as isize)
}

/// Rasterize the cursor `handle` into RGBA. Works for colour, alpha and monochrome cursors:
/// the cursor is drawn once over black and once over white, and alpha is recovered from the
/// difference (`a = 1 - (white - black)`, `colour = black / a`).
pub fn capture_cursor(handle: isize) -> Option<CursorImage> {
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, GetObjectW, HGDIOBJ, SelectObject,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DI_NORMAL, DrawIconEx, GetIconInfo, HICON, ICONINFO,
    };

    // SAFETY: GDI objects created here are all released before returning; the DIB bits pointer
    // is only used while the DIB section is alive and selected.
    unsafe {
        let hicon = HICON(handle as *mut core::ffi::c_void);
        let mut ii = ICONINFO::default();
        GetIconInfo(hicon, &mut ii).ok()?;
        let has_color = !ii.hbmColor.is_invalid();
        let src = if has_color { ii.hbmColor } else { ii.hbmMask };
        let mut bm = BITMAP::default();
        GetObjectW(
            HGDIOBJ(src.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut BITMAP as *mut core::ffi::c_void),
        );
        let hotspot = Vec2::new(ii.xHotspot as f32, ii.yHotspot as f32);
        if !ii.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(ii.hbmColor.0));
        }
        if !ii.hbmMask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(ii.hbmMask.0));
        }
        let w = bm.bmWidth.clamp(1, 256);
        let h = (if has_color {
            bm.bmHeight
        } else {
            bm.bmHeight / 2
        })
        .clamp(1, 256);

        let dc = CreateCompatibleDC(None);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let Ok(dib) = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(dc);
            return None;
        };
        let old = SelectObject(dc, HGDIOBJ(dib.0));
        let n = (w * h) as usize;
        let px = std::slice::from_raw_parts_mut(bits.cast::<u8>(), n * 4);
        let mut render = |bg: u8| -> Vec<u8> {
            px.fill(bg);
            let _ = DrawIconEx(dc, 0, 0, hicon, w, h, 0, None, DI_NORMAL);
            let _ = GdiFlush();
            px.to_vec()
        };
        let on_black = render(0);
        let on_white = render(255);
        SelectObject(dc, old);
        let _ = DeleteObject(HGDIOBJ(dib.0));
        let _ = DeleteDC(dc);

        let mut rgba = vec![0u8; n * 4];
        for i in 0..n {
            let b = &on_black[i * 4..i * 4 + 3]; // BGR
            let wv = &on_white[i * 4..i * 4 + 3];
            let diff = (i32::from(wv[1]) - i32::from(b[1])).clamp(0, 255);
            let a = 255 - diff;
            if a <= 0 {
                continue;
            }
            let un = |c: u8| ((u32::from(c) * 255) / a as u32).min(255) as u8;
            rgba[i * 4] = un(b[2]);
            rgba[i * 4 + 1] = un(b[1]);
            rgba[i * 4 + 2] = un(b[0]);
            rgba[i * 4 + 3] = a as u8;
        }
        if rgba.chunks(4).all(|p| p[3] == 0) {
            return None;
        }
        Some(CursorImage {
            rgba,
            width: w as u32,
            height: h as u32,
            hotspot,
            handle,
        })
    }
}

/// Handle of the user's normal arrow cursor (respects their cursor scheme). Afterimages are
/// always copies of this arrow, never of the hand / I-beam.
pub fn arrow_cursor_handle() -> Option<isize> {
    use windows::Win32::UI::WindowsAndMessaging::{IDC_ARROW, LoadCursorW};
    // SAFETY: loading a shared system cursor; the handle must not be destroyed (we never do).
    unsafe { LoadCursorW(None, IDC_ARROW).ok().map(|h| h.0 as isize) }
}

/// Temporarily make the hand ("link") and I-beam cursors look like the arrow, so the real
/// cursor stays an arrow while the Sandevistan is active. Undone by [`restore_system_cursors`].
pub fn lock_arrow_cursors() {
    use windows::Win32::UI::WindowsAndMessaging::{
        CopyIcon, HCURSOR, HICON, IDC_ARROW, LoadCursorW, OCR_HAND, OCR_IBEAM, SetSystemCursor,
    };
    // SAFETY: SetSystemCursor takes ownership of (and destroys) the copied handle we pass.
    unsafe {
        for id in [OCR_HAND, OCR_IBEAM] {
            let Ok(arrow) = LoadCursorW(None, IDC_ARROW) else {
                return;
            };
            if let Ok(copy) = CopyIcon(HICON(arrow.0)) {
                let _ = SetSystemCursor(HCURSOR(copy.0), id);
            }
        }
    }
}

/// Reload the user's cursor scheme (undoes [`lock_arrow_cursors`]; harmless otherwise).
pub fn restore_system_cursors() {
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_SETCURSORS, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
    };
    // SAFETY: SPI_SETCURSORS takes no parameter buffer.
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_SETCURSORS,
            0,
            None,
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
}

/// Raises the system timer resolution to 1 ms so frame pacing sleeps are accurate.
pub struct TimerResolution;

impl TimerResolution {
    pub fn new() -> Self {
        // SAFETY: paired with `timeEndPeriod` in `Drop`.
        unsafe {
            timeBeginPeriod(1);
        }
        Self
    }
}

impl Drop for TimerResolution {
    fn drop(&mut self) {
        // SAFETY: matches the `timeBeginPeriod(1)` above.
        unsafe {
            timeEndPeriod(1);
        }
    }
}

/// Holds the named mutex that enforces a single running instance.
pub struct SingleInstance(HANDLE);

impl SingleInstance {
    /// Returns `None` if another instance already holds the mutex.
    pub fn acquire() -> Option<Self> {
        // SAFETY: standard named-mutex creation; the handle is closed in `Drop`.
        unsafe {
            let h = CreateMutexW(None, true, w!("Local\\CursorFect.SingleInstance")).ok()?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(h);
                return None;
            }
            Some(Self(h))
        }
    }
}

const QUIT_EVENT: PCWSTR = w!("Local\\CursorFect.Quit");

impl SingleInstance {
    pub fn acquire_or_show() -> Option<Self> {
        if let Some(me) = Self::acquire() { return Some(me); }
        // The existing instance may still be starting; retry until its listener exists.
        for _ in 0..30 {
            unsafe {
                if let Ok(ev) = OpenEventW(EVENT_MODIFY_STATE, false, QUIT_EVENT) {
                    let _ = SetEvent(ev);
                    let _ = CloseHandle(ev);
                    return None;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        None
    }
}

pub fn listen_for_show(on_show: impl Fn() + Send + 'static) {
    let Ok(ev) = (unsafe { CreateEventW(None, false, false, QUIT_EVENT) }) else { return; };
    let raw = ev.0 as isize;
    let _ = std::thread::Builder::new().name("cursorfect-show".into()).spawn(move || {
        let ev = HANDLE(raw as *mut core::ffi::c_void);
        loop {
            unsafe { let _ = WaitForSingleObject(ev, u32::MAX); }
            on_show();
        }
    });
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this value.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("CursorFect");

/// Add/remove the HKCU Run entry ("Start with Windows"). No admin rights needed.
pub fn set_startup(enabled: bool, minimized: bool) -> anyhow::Result<()> {
    // SAFETY: registry calls with local buffers that outlive each call; the key is closed.
    unsafe {
        let mut key = HKEY::default();
        RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, KEY_SET_VALUE, &mut key).ok()?;
        let result = if enabled {
            let exe = std::env::current_exe()?;
            let mut cmd = format!("\"{}\"", exe.display());
            if minimized {
                cmd.push_str(" --minimized");
            }
            let wide: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2);
            RegSetValueExW(key, RUN_VALUE, None, REG_SZ, Some(bytes)).ok()
        } else {
            // Missing value is fine.
            let _ = RegDeleteValueW(key, RUN_VALUE);
            Ok(())
        };
        let _ = RegCloseKey(key);
        result?;
    }
    Ok(())
}

