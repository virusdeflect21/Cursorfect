//! Notification-area icon and menu (tray-icon / muda). Must be created and updated
//! on the UI thread; menu clicks are forwarded as [`TrayAction`] through a callback.

use anyhow::Context;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::settings::config::PresetId;
use crate::state::SysState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    OpenSettings,
    ToggleMaster,
    ToggleActive,
    Preset(PresetId),
    Exit,
}

pub struct Tray {
    _icon: TrayIcon,
    master: MenuItem,
    active: MenuItem,
    presets: Vec<(PresetId, CheckMenuItem)>,
}

impl Tray {
    pub fn create(on_action: impl Fn(TrayAction) + Send + Sync + 'static) -> anyhow::Result<Self> {
        let menu = Menu::new();
        let open = MenuItem::new("Open Settings", true, None);
        let master = MenuItem::new("Enable", true, None);
        let active = MenuItem::new("Activate", true, None);
        let preset_menu = Submenu::new("Preset", true);
        let mut presets = Vec::new();
        for p in PresetId::ALL {
            let item = CheckMenuItem::new(p.label(), true, false, None);
            preset_menu.append(&item).context("append preset item")?;
            presets.push((p, item));
        }
        let exit = MenuItem::new("Exit", true, None);
        menu.append(&open)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&master)?;
        menu.append(&active)?;
        menu.append(&preset_menu)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&exit)?;

        let ids = (
            open.id().clone(),
            master.id().clone(),
            active.id().clone(),
            exit.id().clone(),
            presets
                .iter()
                .map(|(p, i)| (*p, i.id().clone()))
                .collect::<Vec<_>>(),
        );
        let on_action = std::sync::Arc::new(on_action);
        let cb = on_action.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let (open, master, active, exit, presets) = &ids;
            let action = if e.id == *open {
                Some(TrayAction::OpenSettings)
            } else if e.id == *master {
                Some(TrayAction::ToggleMaster)
            } else if e.id == *active {
                Some(TrayAction::ToggleActive)
            } else if e.id == *exit {
                Some(TrayAction::Exit)
            } else {
                presets
                    .iter()
                    .find(|(_, id)| e.id == *id)
                    .map(|(p, _)| TrayAction::Preset(*p))
            };
            if let Some(a) = action {
                cb(a);
            }
        }));
        let cb = on_action;
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
            | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } = e
            {
                cb(TrayAction::OpenSettings);
            }
        }));

        let rgba = crate::icon::render_rgba(32);
        let icon = Icon::from_rgba(rgba, 32, 32).context("tray icon image")?;
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("CursorFect")
            .with_icon(icon)
            .build()
            .context("creating tray icon")?;
        Ok(Self {
            _icon: tray,
            master,
            active,
            presets,
        })
    }

    pub fn update(&self, state: SysState, preset: PresetId) {
        let online = state != SysState::Offline;
        self.master
            .set_text(if online { "Disable" } else { "Enable" });
        self.active.set_text(if state == SysState::Active {
            "Deactivate"
        } else {
            "Activate"
        });
        self.active
            .set_enabled(matches!(state, SysState::Ready | SysState::Active));
        for (p, item) in &self.presets {
            item.set_checked(*p == preset);
        }
        let _ = self
            ._icon
            .set_tooltip(Some(format!("CursorFect \u{2014} {}", state.label())));
    }
}
