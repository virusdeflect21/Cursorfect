//! Independent safety net for The World.
//!
//! While time is stopped, mouse clicks are swallowed and a frozen image covers the desktop.
//! The engine thread normally ends that. If it stalls past the deadline, this thread:
//! 1. clears the "time stopped" flag the input hook reads (clicks reach apps again), and
//! 2. hides the overlay with `ShowWindowAsync` (never blocks on the stalled thread).
//!
//! It cannot help if the whole process is frozen, but a stuck engine thread can no longer
//! leave the desktop unusable.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindowAsync};

pub struct Watchdog {
    deadline: Mutex<Option<Instant>>,
    overlay: AtomicIsize,
    /// The flag the input hook checks before swallowing clicks.
    time_stopped: Arc<AtomicBool>,
}

impl Watchdog {
    pub fn spawn(time_stopped: Arc<AtomicBool>) -> Arc<Self> {
        let w = Arc::new(Self {
            deadline: Mutex::new(None),
            overlay: AtomicIsize::new(0),
            time_stopped,
        });
        let me = w.clone();
        let _ = std::thread::Builder::new()
            .name("cursorfect-watchdog".into())
            .spawn(move || me.run());
        w
    }

    /// Arm (Some) or disarm (None). `hwnd`: the overlay window to hide on expiry.
    pub fn arm(&self, deadline: Option<Instant>, hwnd: isize) {
        self.overlay.store(hwnd, Ordering::Release);
        if let Ok(mut d) = self.deadline.lock() {
            *d = deadline;
        }
    }

    fn run(&self) {
        loop {
            std::thread::sleep(Duration::from_millis(200));
            let due = self
                .deadline
                .lock()
                .ok()
                .and_then(|d| *d)
                .is_some_and(|d| Instant::now() >= d);
            if !due {
                continue;
            }
            tracing::error!("watchdog: time stop overran its deadline; restoring input");
            self.time_stopped.store(false, Ordering::Release);
            let raw = self.overlay.load(Ordering::Acquire);
            if raw != 0 {
                // SAFETY: ShowWindowAsync only posts a message; a stale handle fails harmlessly.
                unsafe {
                    let _ = ShowWindowAsync(HWND(raw as *mut core::ffi::c_void), SW_HIDE);
                }
            }
            if let Ok(mut d) = self.deadline.lock() {
                *d = None;
            }
        }
    }
}
