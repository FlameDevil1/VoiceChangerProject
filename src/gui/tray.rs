//! System tray icon: show the window, toggle effects/mute, switch presets, quit.
//!
//! The tray's hidden window is pumped by the app's main event loop, so it must be created on the
//! main thread. Events arrive through handlers that also wake the app (`request_repaint`), so they
//! are handled even while the window is hidden.

use eframe::egui;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

pub enum TrayCommand {
    Show,
    ToggleEffects,
    ToggleMute,
    Preset(String),
    Quit,
}

impl TrayCommand {
    pub fn describe(&self) -> String {
        match self {
            TrayCommand::Show => "show".into(),
            TrayCommand::ToggleEffects => "toggle effects".into(),
            TrayCommand::ToggleMute => "toggle mute".into(),
            TrayCommand::Preset(p) => format!("preset {p}"),
            TrayCommand::Quit => "quit".into(),
        }
    }
}

const ID_SHOW: &str = "show";
const ID_EFFECTS: &str = "effects";
const ID_MUTE: &str = "mute";
const ID_QUIT: &str = "quit";
const PRESET_PREFIX: &str = "preset:";

pub struct Tray {
    icon: TrayIcon,
    effects: CheckMenuItem,
    mute: CheckMenuItem,
    voice: Submenu,
    presets: Vec<(String, CheckMenuItem)>,
    rx: Receiver<TrayCommand>,
    last: (bool, bool, Option<String>),
}

impl Tray {
    pub fn new(ctx: &egui::Context) -> Option<Self> {
        let menu = Menu::new();
        let show = MenuItem::with_id(ID_SHOW, "Show Voice Changer", true, None);
        let effects = CheckMenuItem::with_id(ID_EFFECTS, "Effects on", true, true, None);
        let mute = CheckMenuItem::with_id(ID_MUTE, "Mute virtual mic", true, false, None);
        let voice = Submenu::new("Voice", true);
        let quit = MenuItem::with_id(ID_QUIT, "Quit", true, None);
        menu.append_items(&[
            &show,
            &PredefinedMenuItem::separator(),
            &effects,
            &mute,
            &voice,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .ok()?;

        let icon = Icon::from_rgba(super::icon::rgba(32), 32, 32).ok()?;
        let tray = TrayIconBuilder::new()
            .with_tooltip("Voice Changer")
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| log::warn!("tray icon unavailable: {e}"))
            .ok()?;

        let (tx, rx) = mpsc::channel::<TrayCommand>();
        let menu_tx = Mutex::new(tx.clone());
        let wake = ctx.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let id = e.id.0.as_str();
            let cmd = match id {
                ID_SHOW => Some(TrayCommand::Show),
                ID_EFFECTS => Some(TrayCommand::ToggleEffects),
                ID_MUTE => Some(TrayCommand::ToggleMute),
                ID_QUIT => Some(TrayCommand::Quit),
                _ => id.strip_prefix(PRESET_PREFIX).map(|n| TrayCommand::Preset(n.to_string())),
            };
            send(&menu_tx, cmd, &wake);
        }));
        // Left click on the icon shows the window (right click opens the menu).
        let click_tx = Mutex::new(tx);
        let wake = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            let cmd = match e {
                TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } => {
                    Some(TrayCommand::Show)
                }
                _ => None,
            };
            send(&click_tx, cmd, &wake);
        }));
        Some(Self { icon: tray, effects, mute, voice, presets: Vec::new(), rx, last: (true, false, None) })
    }

    /// Commands since the last call.
    pub fn poll(&self) -> Vec<TrayCommand> {
        self.rx.try_iter().collect()
    }

    /// Reflect the app state in the menu (cheap when nothing changed).
    pub fn sync(&mut self, names: &[String], current: Option<&str>, effects_on: bool, muted: bool) {
        let state = (effects_on, muted, current.map(str::to_string));
        let names_changed =
            self.presets.len() != names.len() || self.presets.iter().zip(names).any(|((n, _), m)| n != m);
        if names_changed {
            for (_, item) in self.presets.drain(..) {
                let _ = self.voice.remove(&item);
            }
            for name in names {
                let item = CheckMenuItem::with_id(format!("{PRESET_PREFIX}{name}"), name, true, false, None);
                let _ = self.voice.append(&item);
                self.presets.push((name.clone(), item));
            }
        }
        if names_changed || state != self.last {
            self.effects.set_checked(effects_on);
            self.mute.set_checked(muted);
            for (name, item) in &self.presets {
                item.set_checked(Some(name.as_str()) == current);
            }
            let tip = match (current, effects_on, muted) {
                (_, _, true) => "Voice Changer: muted".to_string(),
                (_, false, _) => "Voice Changer: effects off".to_string(),
                (Some(p), true, _) => format!("Voice Changer: {p}"),
                (None, true, _) => "Voice Changer".to_string(),
            };
            let _ = self.icon.set_tooltip(Some(tip));
            self.last = state;
        }
    }
}

fn send(tx: &Mutex<Sender<TrayCommand>>, cmd: Option<TrayCommand>, wake: &egui::Context) {
    if let (Some(cmd), Ok(tx)) = (cmd, tx.lock()) {
        let _ = tx.send(cmd);
        wake.request_repaint();
    }
}
