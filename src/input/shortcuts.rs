//! Shortcut model: what a binding is, how it matches, how it is displayed and
//! which bindings are rejected as invalid / reserved / conflicting.

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const VK_ESCAPE: u32 = 0x1B;
pub const VK_F1: u32 = 0x70;
pub const VK_F12: u32 = 0x7B;
pub const VK_F24: u32 = 0x87;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

impl Modifiers {
    pub const NONE: Modifiers = Modifiers {
        ctrl: false,
        alt: false,
        shift: false,
        win: false,
    };

    pub fn any(&self) -> bool {
        self.ctrl || self.alt || self.shift || self.win
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    /// "Mouse Button 4" (back).
    X1,
    /// "Mouse Button 5" (forward).
    X2,
    /// Middle click (wheel button).
    Middle,
}

impl MouseButton {
    /// Bit used to remember that this button's down event was swallowed.
    pub fn bit(self) -> u8 {
        match self {
            MouseButton::X1 => 1,
            MouseButton::X2 => 2,
            MouseButton::Middle => 4,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            MouseButton::X1 => "Mouse Button 4",
            MouseButton::X2 => "Mouse Button 5",
            MouseButton::Middle => "Middle Click",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Trigger {
    /// A Win32 virtual-key code (never a modifier key).
    Key(u32),
    Mouse(MouseButton),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shortcut {
    pub trigger: Trigger,
    #[serde(default)]
    pub mods: Modifiers,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ShortcutError {
    #[error("Ctrl+Alt+Shift+F12 is reserved as the emergency disable shortcut")]
    Reserved,
    #[error("The Windows key can only be combined with F-keys or mouse buttons")]
    WinNotAllowed,
    #[error("That combination is used by Windows or by common editing commands")]
    SystemConflict,
    #[error("That shortcut is already used by {0}")]
    Duplicate(&'static str),
}

impl Shortcut {
    pub const fn mouse(button: MouseButton, mods: Modifiers) -> Self {
        Self {
            trigger: Trigger::Mouse(button),
            mods,
        }
    }

    pub const fn key(vk: u32, mods: Modifiers) -> Self {
        Self {
            trigger: Trigger::Key(vk),
            mods,
        }
    }

    /// Default activation shortcut: middle click.
    pub const fn default_activation() -> Self {
        Self::mouse(MouseButton::Middle, Modifiers::NONE)
    }

    /// Default dash shortcut: Ctrl + middle click.
    pub const fn default_dash() -> Self {
        Self::mouse(
            MouseButton::Middle,
            Modifiers {
                ctrl: true,
                alt: false,
                shift: false,
                win: false,
            },
        )
    }

    pub fn matches_key(&self, vk: u32, mods: Modifiers) -> bool {
        self.trigger == Trigger::Key(vk) && self.mods == mods
    }

    pub fn matches_mouse(&self, button: MouseButton, mods: Modifiers) -> bool {
        self.trigger == Trigger::Mouse(button) && self.mods == mods
    }

    pub fn is_emergency(&self) -> bool {
        match self.trigger {
            Trigger::Key(vk) => is_emergency(vk, self.mods),
            Trigger::Mouse(_) => false,
        }
    }

    pub fn display(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.mods.ctrl {
            parts.push("Ctrl".into());
        }
        if self.mods.alt {
            parts.push("Alt".into());
        }
        if self.mods.shift {
            parts.push("Shift".into());
        }
        if self.mods.win {
            parts.push("Win".into());
        }
        parts.push(match self.trigger {
            Trigger::Key(vk) => key_name(vk),
            Trigger::Mouse(b) => b.name().to_string(),
        });
        parts.join(" + ")
    }

    /// Checks that this shortcut is acceptable on its own.
    pub fn validate(&self) -> Result<(), ShortcutError> {
        if self.is_emergency() {
            return Err(ShortcutError::Reserved);
        }
        if let Trigger::Key(vk) = self.trigger {
            let is_fkey = (VK_F1..=VK_F24).contains(&vk);
            if self.mods.win && !is_fkey {
                return Err(ShortcutError::WinNotAllowed);
            }
            if vk == VK_ESCAPE && !self.mods.any() {
                return Err(ShortcutError::SystemConflict);
            }
            if is_system_combo(vk, self.mods) {
                return Err(ShortcutError::SystemConflict);
            }
        }
        Ok(())
    }

    /// A plain typing key (letter, digit, space, ...) without Ctrl/Alt/Win: allowed, but it
    /// will also fire while typing, so the UI shows a heads-up.
    pub fn is_typing_key(&self) -> bool {
        match self.trigger {
            Trigger::Key(vk) => {
                !(self.mods.ctrl || self.mods.alt || self.mods.win)
                    && matches!(vk, 0x08 | 0x09 | 0x0D | 0x20 | 0x30..=0x39 | 0x41..=0x5A | 0xBA..=0xC0 | 0xDB..=0xDE)
            }
            Trigger::Mouse(_) => false,
        }
    }

    /// Validates against another binding (`other_name` describes it for the message).
    pub fn validate_against(
        &self,
        other: &Shortcut,
        other_name: &'static str,
    ) -> Result<(), ShortcutError> {
        self.validate()?;
        if self == other {
            return Err(ShortcutError::Duplicate(other_name));
        }
        Ok(())
    }
}

/// The emergency-disable chord: Ctrl + Alt + Shift + F12 (Win key ignored).
pub fn is_emergency(vk: u32, mods: Modifiers) -> bool {
    vk == VK_F12 && mods.ctrl && mods.alt && mods.shift
}

fn is_system_combo(vk: u32, m: Modifiers) -> bool {
    const TAB: u32 = 0x09;
    const SPACE: u32 = 0x20;
    const F4: u32 = 0x73;
    const DELETE: u32 = 0x2E;
    // Alt+F4, Alt+Tab, Alt+Esc, Alt+Space
    if m.alt && !m.ctrl && (vk == F4 || vk == TAB || vk == VK_ESCAPE || vk == SPACE) {
        return true;
    }
    // Ctrl+Esc, Ctrl+Shift+Esc
    if m.ctrl && vk == VK_ESCAPE {
        return true;
    }
    // Ctrl+Alt+Del
    if m.ctrl && m.alt && vk == DELETE {
        return true;
    }
    // Common editing commands.
    if m.ctrl && !m.alt && !m.shift && !m.win {
        return [b'C', b'V', b'X', b'A', b'Z', b'Y', b'S']
            .iter()
            .any(|c| vk == u32::from(*c));
    }
    false
}

pub fn is_modifier_vk(vk: u32) -> bool {
    matches!(vk, 0x10 | 0x11 | 0x12 | 0xA0..=0xA5 | 0x5B | 0x5C)
}

pub fn key_name(vk: u32) -> String {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => (vk as u8 as char).to_string(),
        v if (VK_F1..=VK_F24).contains(&v) => format!("F{}", v - VK_F1 + 1),
        0x08 => "Backspace".into(),
        0x09 => "Tab".into(),
        0x0D => "Enter".into(),
        0x13 => "Pause".into(),
        0x14 => "Caps Lock".into(),
        0x1B => "Esc".into(),
        0x20 => "Space".into(),
        0x21 => "Page Up".into(),
        0x22 => "Page Down".into(),
        0x23 => "End".into(),
        0x24 => "Home".into(),
        0x25 => "Left".into(),
        0x26 => "Up".into(),
        0x27 => "Right".into(),
        0x28 => "Down".into(),
        0x2C => "Print Screen".into(),
        0x2D => "Insert".into(),
        0x2E => "Delete".into(),
        0x60..=0x69 => format!("Num {}", vk - 0x60),
        0x6A => "Num *".into(),
        0x6B => "Num +".into(),
        0x6D => "Num -".into(),
        0x6E => "Num .".into(),
        0x6F => "Num /".into(),
        0x90 => "Num Lock".into(),
        0x91 => "Scroll Lock".into(),
        0xBA => ";".into(),
        0xBB => "=".into(),
        0xBC => ",".into(),
        0xBD => "-".into(),
        0xBE => ".".into(),
        0xBF => "/".into(),
        0xC0 => "`".into(),
        0xDB => "[".into(),
        0xDC => "\\".into(),
        0xDD => "]".into(),
        0xDE => "'".into(),
        other => format!("Key 0x{other:02X}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: Modifiers = Modifiers {
        ctrl: true,
        alt: false,
        shift: false,
        win: false,
    };
    const CAS: Modifiers = Modifiers {
        ctrl: true,
        alt: true,
        shift: true,
        win: false,
    };

    #[test]
    fn exact_modifier_matching() {
        let act = Shortcut::default_activation();
        let dash = Shortcut::default_dash();
        assert!(act.matches_mouse(MouseButton::Middle, Modifiers::NONE));
        assert!(
            !act.matches_mouse(MouseButton::Middle, CTRL),
            "Ctrl+Middle must not trigger plain Middle"
        );
        assert!(dash.matches_mouse(MouseButton::Middle, CTRL));
        assert!(!dash.matches_mouse(MouseButton::Middle, Modifiers::NONE));
        assert!(!act.matches_mouse(MouseButton::X1, Modifiers::NONE));
        assert!(!act.matches_mouse(MouseButton::X2, Modifiers::NONE));
    }

    #[test]
    fn key_matching() {
        let s = Shortcut::key(0x47, CTRL); // Ctrl+G
        assert!(s.matches_key(0x47, CTRL));
        assert!(!s.matches_key(0x47, Modifiers::NONE));
        assert!(!s.matches_key(0x48, CTRL));
        assert!(!s.matches_mouse(MouseButton::X1, CTRL));
    }

    #[test]
    fn validation_rules() {
        assert_eq!(
            Shortcut::key(VK_F12, CAS).validate(),
            Err(ShortcutError::Reserved)
        );
        // Plain keys are allowed (with a typing heads-up), Esc alone is not.
        assert!(Shortcut::key(0x41, Modifiers::NONE).validate().is_ok());
        assert!(Shortcut::key(0x41, Modifiers::NONE).is_typing_key());
        assert!(!Shortcut::key(VK_F1, Modifiers::NONE).is_typing_key());
        assert!(Shortcut::key(VK_ESCAPE, Modifiers::NONE).validate().is_err());
        assert!(Shortcut::key(VK_F1 + 8, Modifiers::NONE).validate().is_ok());
        assert_eq!(
            Shortcut::key(0x43, CTRL).validate(),
            Err(ShortcutError::SystemConflict)
        );
        let win = Modifiers {
            win: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            Shortcut::key(0x4C, win).validate(),
            Err(ShortcutError::WinNotAllowed)
        );
        assert!(Shortcut::mouse(MouseButton::X1, win).validate().is_ok());
        assert!(Shortcut::default_activation().validate().is_ok());
    }

    #[test]
    fn duplicates_rejected() {
        let a = Shortcut::default_activation();
        assert_eq!(
            a.validate_against(&a, "activation"),
            Err(ShortcutError::Duplicate("activation"))
        );
        assert!(
            Shortcut::default_dash()
                .validate_against(&a, "activation")
                .is_ok()
        );
    }

    #[test]
    fn display_names() {
        assert_eq!(Shortcut::default_activation().display(), "Middle Click");
        assert_eq!(Shortcut::default_dash().display(), "Ctrl + Middle Click");
        assert_eq!(
            Shortcut::mouse(MouseButton::X2, Modifiers::NONE).display(),
            "Mouse Button 5"
        );
        assert_eq!(Shortcut::key(0x47, CTRL).display(), "Ctrl + G");
        assert_eq!(Shortcut::key(VK_F1 + 8, Modifiers::NONE).display(), "F9");
    }

    #[test]
    fn serde_roundtrip() {
        let s = Shortcut::default_dash();
        let j = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Shortcut>(&j).unwrap(), s);
    }
}
