//! Optional MIH desktop-pointer acceleration. Never changes Windows pointer settings.
//!
//! The low-level hook observes *already processed screen-space* movement, not raw HID
//! deltas. Physical events can be replaced by one bounded absolute SendInput packet.
//! Every injected event (ours or another application's) passes through unmodified.
//! This is deliberately not a driver/raw-input accelerator: games, secure desktops,
//! remote/absolute pointing devices and elevated applications may not support it.
//!
//! No rendering, channel round trips, waits or blocking locks occur here. A short
//! renewable lease drops the multiplier if the engine stalls. An insertion failure
//! fails open and disables only this component until the next activation.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use glam::Vec2;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_MOUSE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT, SendInput,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, GetCursorInfo, GetCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use super::cursor::Rect;

/// An extra-info tag is a second guard in addition to LLMHF_INJECTED.
pub const INPUT_TAG: usize = 0x4D49_4843;
const LEASE: Duration = Duration::from_millis(300);
const MAX_PHYSICAL_DELTA: f32 = 96.0;
const MAX_OUTPUT_DELTA: f32 = 128.0;

#[derive(Default)]
struct MotionScaler {
    fractional: Vec2,
    last_position: Option<Vec2>,
    /// Only one replacement may be outstanding. A fast physical packet must
    /// never subtract a synthetic target that Windows hasn't applied yet.
    pending: Option<Instant>,
}

impl MotionScaler {
    fn target(
        &mut self,
        current: Vec2,
        requested: Vec2,
        multiplier: f32,
        bounds: Rect,
    ) -> Option<Vec2> {
        let delta = requested - current;
        if !delta.is_finite()
            || !current.is_finite()
            || !multiplier.is_finite()
            || multiplier <= 1.0001
            || delta.length() > MAX_PHYSICAL_DELTA
            || bounds.right - bounds.left < 2.0
            || bounds.bottom - bounds.top < 2.0
        {
            self.fractional = Vec2::ZERO;
            return None;
        }
        // Guard programmatic warps, absolute devices, monitor-edge jumps and large packets.
        let scaled = (delta * multiplier.clamp(1.0, 6.0) + self.fractional)
            .clamp_length_max(MAX_OUTPUT_DELTA);
        let rounded = scaled.round();
        self.fractional = scaled - rounded;
        let target = bounds.clamp(current + rounded).round();
        if target == requested.round() {
            None // normal packet is already correct; don't inject a redundant event
        } else {
            Some(target)
        }
    }
}

pub struct PointerAcceleration {
    epoch: Instant,
    multiplier: AtomicU32,
    lease_until_ms: AtomicU64,
    armed: AtomicBool,
    failed: AtomicBool,
    report_failure: AtomicBool,
    scaler: Mutex<MotionScaler>,
}

impl PointerAcceleration {
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
            multiplier: AtomicU32::new(1.0f32.to_bits()),
            lease_until_ms: AtomicU64::new(0),
            armed: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            report_failure: AtomicBool::new(false),
            scaler: Mutex::new(MotionScaler::default()),
        }
    }

    pub fn arm(&self) {
        self.disable();
        self.failed.store(false, Ordering::Release);
        self.report_failure.store(false, Ordering::Release);
        if let Ok(mut s) = self.scaler.try_lock() {
            s.fractional = Vec2::ZERO;
            s.last_position = None;
            s.pending = None;
        }
        self.armed.store(true, Ordering::Release);
    }

    /// Engine thread. Only valid while explicitly armed for this activation.
    pub fn publish(&self, multiplier: f32, now: Instant) {
        if !self.armed.load(Ordering::Acquire) || self.failed.load(Ordering::Acquire) {
            return;
        }
        let m = if multiplier.is_finite() {
            multiplier.clamp(1.0, 6.0)
        } else {
            1.0
        };
        let until = now.saturating_duration_since(self.epoch) + LEASE;
        self.lease_until_ms
            .store(until.as_millis() as u64, Ordering::Release);
        self.multiplier.store(m.to_bits(), Ordering::Release);
    }

    /// Atomic-only cancellation, also safe from the panic and emergency paths.
    pub fn disable(&self) {
        self.armed.store(false, Ordering::Release);
        self.multiplier.store(1.0f32.to_bits(), Ordering::Release);
        self.lease_until_ms.store(0, Ordering::Release);
    }

    pub fn take_failure(&self) -> bool {
        self.report_failure.swap(false, Ordering::AcqRel)
    }

    #[cfg(test)]
    pub(crate) fn test_multiplier(&self, now: Instant) -> f32 {
        self.multiplier_at(now)
    }

    fn fail_open(&self) {
        self.failed.store(true, Ordering::Release);
        self.report_failure.store(true, Ordering::Release);
        self.disable();
    }

    fn multiplier_at(&self, now: Instant) -> f32 {
        if !self.armed.load(Ordering::Acquire) || self.failed.load(Ordering::Acquire) {
            return 1.0;
        }
        let until = self.lease_until_ms.load(Ordering::Acquire);
        if until == 0 {
            return 1.0;
        }
        if now.saturating_duration_since(self.epoch).as_millis() as u64 >= until {
            // No persistent state exists to restore: simply stop producing packets.
            // Latch this component off rather than suddenly speeding up when a
            // stalled engine comes back. A new activation may explicitly retry.
            self.fail_open();
            return 1.0;
        }
        f32::from_bits(self.multiplier.load(Ordering::Acquire)).clamp(1.0, 6.0)
    }

    /// Track committed input-stream positions, including injected packets, but
    /// never scale them. This works whether GetCursorPos has already advanced at
    /// hook time or still reflects the previous position.
    pub fn observe_position(&self, position: Vec2, our_packet: bool) {
        if let Ok(mut scaler) = self.scaler.try_lock() {
            scaler.last_position = Some(position);
            if our_packet {
                scaler.pending = None;
            }
        }
    }

    /// Returns true ONLY if replacing this physical movement packet succeeded.
    /// The caller must not call this for injected mouse events.
    pub fn on_physical_move(&self, requested: Vec2, now: Instant) -> bool {
        let multiplier = self.multiplier_at(now);
        if multiplier <= 1.0001 {
            return false;
        }
        // Hidden/locked game cursors are not desktop pointers. Leave their raw
        // input paths alone rather than attempting to emulate driver acceleration.
        let mut cursor = CURSORINFO {
            cbSize: std::mem::size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: correctly sized local out-parameter, no retained pointers.
        if unsafe { GetCursorInfo(&mut cursor) }.is_err() {
            self.fail_open();
            return false;
        }
        if cursor.flags.0 & CURSOR_SHOWING.0 == 0 {
            return false;
        }
        // Try, never wait. Engine/UI cancellation can never block a mouse callback.
        let Ok(mut scaler) = self.scaler.try_lock() else {
            return false;
        };
        if scaler
            .pending
            .is_some_and(|at| now.saturating_duration_since(at) < Duration::from_millis(30))
        {
            return false; // fail open until our injected callback has committed the target
        }
        if scaler.pending.is_some() {
            scaler.last_position = None;
            scaler.pending = None;
        }
        let current = match scaler.last_position {
            Some(position) => position,
            None => {
                let mut current = POINT::default();
                // SAFETY: a local out-parameter; used only to seed/resynchronize
                // the stream, not polled for each physical packet.
                if unsafe { GetCursorPos(&mut current) }.is_err() {
                    self.fail_open();
                    return false;
                }
                Vec2::new(current.x as f32, current.y as f32)
            }
        };
        // SAFETY: value-only system metric calls; these include negative monitor origins.
        let (left, top, width, height) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        let bounds = Rect {
            left: left as f32,
            top: top as f32,
            right: (left + width) as f32,
            bottom: (top + height) as f32,
        };
        let Some(target) = scaler.target(current, requested, multiplier, bounds) else {
            return false;
        };
        // Cancellation may have arrived while the packet was being prepared.
        if !self.armed.load(Ordering::Acquire) {
            return false;
        }
        let norm = |p: f32, origin: i32, extent: i32| -> i32 {
            // Absolute input addresses pixel *centres* over [0,65536), not [0,width-1].
            (((p - origin as f32 + 0.5) * 65536.0 / extent.max(1) as f32).floor() as i32)
                .clamp(0, 65535)
        };
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: norm(target.x, left, width),
                    dy: norm(target.y, top, height),
                    dwFlags: MOUSE_EVENT_FLAGS(
                        MOUSEEVENTF_MOVE.0 | MOUSEEVENTF_ABSOLUTE.0 | MOUSEEVENTF_VIRTUALDESK.0,
                    ),
                    dwExtraInfo: INPUT_TAG,
                    ..Default::default()
                },
            },
        };
        scaler.pending = Some(now);
        // SendInput can synchronously deliver the injected callback. Release the
        // scratch lock first so its observation can clear the outstanding packet.
        drop(scaler);
        // SAFETY: one correctly sized INPUT with no borrowed pointers. SendInput
        // inserts a packet; we never wait for its injected callback or suppress it.
        if unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) } == 1 {
            true
        } else {
            if let Ok(mut scaler) = self.scaler.try_lock() {
                scaler.pending = None;
            }
            self.fail_open(); // UIPI / secure desktop etc.: let the real packet through
            false
        }
    }
}

impl Default for PointerAcceleration {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> Rect {
        Rect {
            left: -1920.0,
            top: -200.0,
            right: 2560.0,
            bottom: 1440.0,
        }
    }

    #[test]
    fn real_delta_is_multiplied_without_chasing_previous_synthetic_motion() {
        let mut s = MotionScaler::default();
        let p = Vec2::new(100.0, 200.0);
        assert_eq!(
            s.target(p, p + Vec2::new(3.0, -2.0), 4.0, bounds()),
            Some(p + Vec2::new(12.0, -8.0))
        );
        let p = p + Vec2::new(12.0, -8.0); // committed synthetic position
        assert_eq!(
            s.target(p, p + Vec2::X, 4.0, bounds()),
            Some(p + Vec2::X * 4.0)
        );
    }

    #[test]
    fn fractional_gain_does_not_get_rounded_away() {
        let mut s = MotionScaler::default();
        let mut p = Vec2::ZERO;
        for _ in 0..10 {
            let physical = p + Vec2::X;
            p = s.target(p, physical, 1.3, bounds()).unwrap_or(physical);
        }
        assert!((p.x - 13.0).abs() <= 1.0, "{p:?}");
    }

    #[test]
    fn normal_speed_warps_and_invalid_values_fail_open() {
        let mut s = MotionScaler::default();
        assert_eq!(s.target(Vec2::ZERO, Vec2::X, 1.0, bounds()), None);
        assert_eq!(s.target(Vec2::ZERO, Vec2::X * 500.0, 6.0, bounds()), None);
        assert_eq!(s.target(Vec2::ZERO, Vec2::NAN, 4.0, bounds()), None);
        assert_eq!(s.target(Vec2::ZERO, Vec2::X, f32::INFINITY, bounds()), None);
    }

    #[test]
    fn packets_are_bounded_and_work_with_negative_monitor_origins() {
        let mut s = MotionScaler::default();
        let p = Vec2::new(-1800.0, 50.0);
        let q = s
            .target(p, p + Vec2::new(70.0, 50.0), 6.0, bounds())
            .unwrap();
        assert!(p.distance(q) <= MAX_OUTPUT_DELTA + 1.0);
        let p = Vec2::new(-1910.0, 50.0);
        let q = s.target(p, p - Vec2::X * 5.0, 4.0, bounds()).unwrap();
        assert_eq!(q.x, -1920.0);
    }

    #[test]
    fn expired_lease_and_every_disable_path_restore_one_x() {
        let a = PointerAcceleration::new();
        let now = Instant::now();
        a.arm();
        a.publish(4.0, now);
        assert_eq!(a.multiplier_at(now), 4.0);
        assert_eq!(a.multiplier_at(now + LEASE + Duration::from_millis(1)), 1.0);
        a.disable();
        a.publish(6.0, now);
        assert_eq!(
            a.multiplier_at(now),
            1.0,
            "publishing cannot re-arm cancellation"
        );
        a.arm();
        a.publish(4.0, now);
        a.fail_open();
        a.publish(6.0, now);
        assert_eq!(a.multiplier_at(now), 1.0);
        assert!(a.take_failure());
        assert!(!a.take_failure());
        a.arm();
        a.publish(2.0, now);
        assert_eq!(a.multiplier_at(now), 2.0, "next activation can retry");
    }
}
