//! Application state machine and cyberware energy model (pure logic, no I/O).

use crate::settings::config::EnergySettings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SysState {
    Offline,
    Ready,
    Active,
    Overheated,
    /// The World: time is stopped (or stopping / resuming).
    TimeStopped,
}

impl SysState {
    pub fn label(self) -> &'static str {
        match self {
            SysState::Offline => "OFFLINE",
            SysState::Ready => "READY",
            SysState::Active => "ACTIVE",
            SysState::Overheated => "OVERHEATED",
            SysState::TimeStopped => "TIME STOPPED",
        }
    }
}

/// Transitions the engine must react to (play sounds, start/stop effects).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transition {
    Activated,
    Deactivated,
    /// Energy hit zero: Sandevistan was stopped and the cooldown began.
    Overheated,
    CooldownComplete,
}

#[derive(Clone, Debug)]
pub struct Machine {
    master: bool,
    active: bool,
    overheat_left: f32,
    energy: f32,
}

impl Machine {
    pub fn new(max_energy: f32) -> Self {
        Self {
            master: false,
            active: false,
            overheat_left: 0.0,
            energy: max_energy,
        }
    }

    pub fn state(&self) -> SysState {
        if !self.master {
            SysState::Offline
        } else if self.overheat_left > 0.0 {
            SysState::Overheated
        } else if self.active {
            SysState::Active
        } else {
            SysState::Ready
        }
    }

    pub fn energy(&self) -> f32 {
        self.energy
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn master(&self) -> bool {
        self.master
    }

    /// Master switch. Turning OFF stops everything immediately.
    pub fn set_master(&mut self, on: bool) -> Option<Transition> {
        if self.master == on {
            return None;
        }
        self.master = on;
        if !on {
            self.overheat_left = 0.0;
            if self.active {
                self.active = false;
                return Some(Transition::Deactivated);
            }
        }
        None
    }

    /// Hard stop used by the emergency shortcut: master OFF, no activation possible.
    pub fn emergency_stop(&mut self) -> Option<Transition> {
        self.set_master(false)
    }

    pub fn activate(&mut self, cfg: &EnergySettings) -> Option<Transition> {
        if !self.master || self.active || self.overheat_left > 0.0 {
            return None;
        }
        if cfg.enabled && self.energy <= 0.0 {
            return None;
        }
        self.active = true;
        Some(Transition::Activated)
    }

    pub fn deactivate(&mut self) -> Option<Transition> {
        if self.active {
            self.active = false;
            Some(Transition::Deactivated)
        } else {
            None
        }
    }

    pub fn toggle(&mut self, cfg: &EnergySettings) -> Option<Transition> {
        if self.active {
            self.deactivate()
        } else {
            self.activate(cfg)
        }
    }

    /// Dashing is possible while READY or ACTIVE.
    pub fn can_dash(&self) -> bool {
        self.master && self.overheat_left <= 0.0
    }

    /// Pay the dash energy cost. Returns false (and spends nothing) if too low.
    pub fn try_spend(&mut self, cost: f32, cfg: &EnergySettings) -> bool {
        if !cfg.enabled {
            return true;
        }
        if self.energy + 1e-4 >= cost {
            self.energy = (self.energy - cost).max(0.0);
            true
        } else {
            false
        }
    }

    /// Advance by `dt` seconds of real elapsed time.
    pub fn tick(&mut self, dt: f32, cfg: &EnergySettings) -> Option<Transition> {
        let dt = dt.clamp(0.0, 1.0);
        let mut out = None;
        if !cfg.enabled {
            self.energy = cfg.max;
            self.overheat_left = 0.0;
            return None;
        }
        if self.energy > cfg.max {
            self.energy = cfg.max;
        }
        if self.active {
            self.energy -= cfg.drain_per_sec * dt;
            if self.energy <= 0.0 {
                self.energy = 0.0;
                self.active = false;
                self.overheat_left = cfg.overheat_cooldown_secs.max(0.001);
                out = Some(Transition::Overheated);
            }
        } else {
            self.energy = (self.energy + cfg.regen_per_sec * dt).min(cfg.max);
        }
        if self.overheat_left > 0.0 && out.is_none() {
            self.overheat_left -= dt;
            if self.overheat_left <= 0.0 {
                self.overheat_left = 0.0;
                out = Some(Transition::CooldownComplete);
            }
        }
        out
    }

    /// True if `tick` has something to do (energy changing or timer running).
    pub fn needs_tick(&self, cfg: &EnergySettings) -> bool {
        cfg.enabled && (self.active || self.overheat_left > 0.0 || self.energy < cfg.max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> EnergySettings {
        EnergySettings {
            enabled: true,
            ..EnergySettings::default()
        }
    }

    #[test]
    fn offline_cannot_activate() {
        let mut m = Machine::new(100.0);
        assert_eq!(m.state(), SysState::Offline);
        assert_eq!(m.activate(&cfg()), None);
        assert_eq!(m.state(), SysState::Offline);
    }

    #[test]
    fn ready_active_toggle_cycle() {
        let mut m = Machine::new(100.0);
        m.set_master(true);
        assert_eq!(m.state(), SysState::Ready);
        assert_eq!(m.toggle(&cfg()), Some(Transition::Activated));
        assert_eq!(m.state(), SysState::Active);
        assert_eq!(m.toggle(&cfg()), Some(Transition::Deactivated));
        assert_eq!(m.state(), SysState::Ready);
    }

    #[test]
    fn master_off_stops_everything() {
        let mut m = Machine::new(100.0);
        m.set_master(true);
        m.activate(&cfg());
        assert_eq!(m.set_master(false), Some(Transition::Deactivated));
        assert_eq!(m.state(), SysState::Offline);
        assert!(!m.is_active());
        assert_eq!(m.activate(&cfg()), None);
    }

    #[test]
    fn energy_drains_then_overheats() {
        let c = cfg();
        let mut m = Machine::new(c.max);
        m.set_master(true);
        m.activate(&c);
        // 12/s drain: after 5s -> 40 left.
        for _ in 0..50 {
            assert_eq!(m.tick(0.1, &c), None);
        }
        assert!((m.energy() - 40.0).abs() < 0.01, "energy={}", m.energy());
        // Run the rest: 100/12 = 8.33s total.
        let mut overheated = false;
        for _ in 0..40 {
            if m.tick(0.1, &c) == Some(Transition::Overheated) {
                overheated = true;
                break;
            }
        }
        assert!(overheated);
        assert_eq!(m.state(), SysState::Overheated);
        assert_eq!(m.energy(), 0.0);
        assert_eq!(m.activate(&c), None, "cannot activate during cooldown");
    }

    #[test]
    fn overheat_recovers_to_ready() {
        let c = cfg();
        let mut m = Machine::new(c.max);
        m.set_master(true);
        m.activate(&c);
        while m.tick(0.1, &c) != Some(Transition::Overheated) {}
        let mut ticks = 0;
        let mut done = false;
        while ticks < 100 {
            ticks += 1;
            if m.tick(0.1, &c) == Some(Transition::CooldownComplete) {
                done = true;
                break;
            }
        }
        assert!(done);
        assert_eq!(m.state(), SysState::Ready);
        assert!(
            m.energy() > 50.0,
            "regenerated during cooldown: {}",
            m.energy()
        );
        assert_eq!(m.activate(&c), Some(Transition::Activated));
    }

    #[test]
    fn regen_caps_at_max() {
        let c = cfg();
        let mut m = Machine::new(c.max);
        m.set_master(true);
        m.try_spend(20.0, &c);
        for _ in 0..100 {
            m.tick(0.1, &c);
        }
        assert_eq!(m.energy(), c.max);
    }

    #[test]
    fn dash_cost_and_insufficient_energy() {
        let c = cfg();
        let mut m = Machine::new(c.max);
        m.set_master(true);
        for _ in 0..5 {
            assert!(m.try_spend(c.dash_cost, &c));
        }
        assert!(!m.try_spend(c.dash_cost, &c));
        assert_eq!(m.energy(), 0.0);
    }

    #[test]
    fn disabled_energy_never_overheats() {
        let mut c = cfg();
        c.enabled = false;
        let mut m = Machine::new(c.max);
        m.set_master(true);
        m.activate(&c);
        for _ in 0..1000 {
            assert_eq!(m.tick(0.1, &c), None);
        }
        assert_eq!(m.state(), SysState::Active);
        assert!(m.try_spend(1000.0, &c));
    }

    #[test]
    fn emergency_stop_blocks_activation() {
        let mut m = Machine::new(100.0);
        m.set_master(true);
        m.activate(&cfg());
        assert_eq!(m.emergency_stop(), Some(Transition::Deactivated));
        assert_eq!(m.activate(&cfg()), None);
    }
}
