//! Global hotkeys through a passive low-level keyboard hook.
//!
//! `RegisterHotKey` only reports key presses, but "effects while held" needs releases too, so we
//! listen with `WH_KEYBOARD_LL` instead. The hook never blocks or swallows keys: the game or app
//! in front still receives them. Matching is a small state machine (`Matcher`) that is plain Rust
//! and unit-tested; the Windows part only feeds it key events.
//!
//! **Two hooks, one matcher.** Windows may not call the low-level hook while this app's own window
//! is focused (observed on Windows 11), so a second, thread-local `WH_KEYBOARD` hook on the window
//! thread covers that case, and the low-level hook ignores keys while our window is in front.
//! Both feed the same `Matcher`, so each press is handled exactly once, a key pressed in our
//! window and released in a game still pairs up, and recording a new hotkey in settings works.
//!
//! Limitation (Windows rule): hooks don't see keys while an app running as administrator is
//! focused, unless this app runs as administrator too.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Action {
    ToggleEffects,
    HoldEffects,
    NormalVoice,
    NextPreset,
    PrevPreset,
    ToggleMute,
    ShowWindow,
}

impl Action {
    pub const ALL: [Action; 7] = [
        Action::ToggleEffects,
        Action::HoldEffects,
        Action::NormalVoice,
        Action::NextPreset,
        Action::PrevPreset,
        Action::ToggleMute,
        Action::ShowWindow,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Action::ToggleEffects => "Effects on/off",
            Action::HoldEffects => "Effects while held",
            Action::NormalVoice => "Panic: normal voice",
            Action::NextPreset => "Next preset",
            Action::PrevPreset => "Previous preset",
            Action::ToggleMute => "Mute virtual mic",
            Action::ShowWindow => "Show window",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Action::ToggleEffects => "Switch between the changed voice and your normal voice.",
            Action::HoldEffects => "Push-to-talk style: the changed voice only while the key is held.",
            Action::NormalVoice => "Instantly back to your normal voice (effects off).",
            Action::NextPreset => "Load the next preset in the list.",
            Action::PrevPreset => "Load the previous preset in the list.",
            Action::ToggleMute => "Silence the virtual microphone.",
            Action::ShowWindow => "Bring the Voice Changer window to the front.",
        }
    }
}

/// A key combination. `vk` is a Windows virtual-key code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub vk: u16,
}

impl Hotkey {
    pub const fn ctrl_alt(vk: u16) -> Self {
        Self { ctrl: true, alt: true, shift: false, win: false, vk }
    }

    pub fn label(&self) -> String {
        let mut s = String::new();
        for (on, name) in [(self.ctrl, "Ctrl+"), (self.alt, "Alt+"), (self.shift, "Shift+"), (self.win, "Win+")] {
            if on {
                s.push_str(name);
            }
        }
        s + &key_name(self.vk)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    pub enabled: bool,
    pub bindings: BTreeMap<Action, Hotkey>,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        let bindings = BTreeMap::from([
            (Action::ToggleEffects, Hotkey::ctrl_alt(0x56)), // V
            (Action::NormalVoice, Hotkey::ctrl_alt(0x4E)),   // N
            (Action::NextPreset, Hotkey::ctrl_alt(0x22)),    // Page Down
            (Action::PrevPreset, Hotkey::ctrl_alt(0x21)),    // Page Up
            (Action::ToggleMute, Hotkey::ctrl_alt(0x4D)),    // M
        ]);
        Self { enabled: true, bindings }
    }
}

impl HotkeyConfig {
    /// Another action already using `key`, if any (the UI warns before reassigning).
    pub fn conflict(&self, key: &Hotkey, except: Action) -> Option<Action> {
        self.bindings.iter().find(|(a, k)| **a != except && *k == key).map(|(a, _)| *a)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Pressed(Action),
    Released(Action),
    /// Result of capture mode: `Some` = new combination, `None` = cleared (Backspace/Delete).
    Captured(Option<Hotkey>),
    CaptureCancelled,
}

const VK_BACK: u16 = 0x08;
const VK_ESCAPE: u16 = 0x1B;
const VK_DELETE: u16 = 0x2E;

#[derive(Clone, Copy, Default)]
struct Mods {
    lctrl: bool,
    rctrl: bool,
    lalt: bool,
    ralt: bool,
    lshift: bool,
    rshift: bool,
    lwin: bool,
    rwin: bool,
}

impl Mods {
    /// Update from a modifier key event; returns false if `vk` isn't a modifier.
    fn update(&mut self, vk: u16, down: bool) -> bool {
        let slot = match vk {
            0xA2 | 0x11 => &mut self.lctrl,
            0xA3 => &mut self.rctrl,
            0xA4 | 0x12 => &mut self.lalt,
            0xA5 => &mut self.ralt,
            0xA0 | 0x10 => &mut self.lshift,
            0xA1 => &mut self.rshift,
            0x5B => &mut self.lwin,
            0x5C => &mut self.rwin,
            _ => return false,
        };
        *slot = down;
        true
    }

    fn matches(&self, k: &Hotkey) -> bool {
        (self.lctrl || self.rctrl) == k.ctrl
            && (self.lalt || self.ralt) == k.alt
            && (self.lshift || self.rshift) == k.shift
            && (self.lwin || self.rwin) == k.win
    }

    fn any(&self) -> bool {
        self.lctrl || self.rctrl || self.lalt || self.ralt || self.lshift || self.rshift || self.lwin || self.rwin
    }

    fn combo(&self, vk: u16) -> Hotkey {
        Hotkey {
            ctrl: self.lctrl || self.rctrl,
            alt: self.lalt || self.ralt,
            shift: self.lshift || self.rshift,
            win: self.lwin || self.rwin,
            vk,
        }
    }
}

/// Key-state machine: tracks modifiers, ignores auto-repeat, pairs presses with releases.
pub struct Matcher {
    pub config: HotkeyConfig,
    /// Next key combination is captured for the settings UI instead of triggering actions.
    pub capture: bool,
    mods: Mods,
    down: [bool; 256],
    held: Vec<(Action, u16)>,
}

impl Matcher {
    pub fn new(config: HotkeyConfig) -> Self {
        Self { config, capture: false, mods: Mods::default(), down: [false; 256], held: Vec::with_capacity(8) }
    }

    pub fn key(&mut self, vk: u16, down: bool, emit: &mut dyn FnMut(Event)) {
        if self.mods.update(vk, down) {
            return;
        }
        let i = (vk & 0xFF) as usize;
        if down {
            if self.down[i] {
                return; // auto-repeat
            }
            self.down[i] = true;
            if self.capture {
                self.capture = false;
                emit(match vk {
                    VK_ESCAPE if !self.mods.any() => Event::CaptureCancelled,
                    VK_BACK | VK_DELETE if !self.mods.any() => Event::Captured(None),
                    _ => Event::Captured(Some(self.mods.combo(vk))),
                });
                return;
            }
            if !self.config.enabled {
                return;
            }
            let hit = self.config.bindings.iter().find(|(_, k)| k.vk == vk && self.mods.matches(k)).map(|(a, _)| *a);
            if let Some(action) = hit {
                self.held.push((action, vk));
                emit(Event::Pressed(action));
            }
        } else {
            self.down[i] = false;
            let mut j = 0;
            while j < self.held.len() {
                if self.held[j].1 == vk {
                    let (action, _) = self.held.swap_remove(j);
                    emit(Event::Released(action));
                } else {
                    j += 1;
                }
            }
        }
    }
}

/// Human-readable name for a virtual-key code (US layout labels for punctuation).
pub fn key_name(vk: u16) -> String {
    let named = match vk {
        0x08 => "Backspace",
        0x09 => "Tab",
        0x0D => "Enter",
        0x13 => "Pause",
        0x14 => "Caps Lock",
        0x1B => "Esc",
        0x20 => "Space",
        0x21 => "Page Up",
        0x22 => "Page Down",
        0x23 => "End",
        0x24 => "Home",
        0x25 => "Left",
        0x26 => "Up",
        0x27 => "Right",
        0x28 => "Down",
        0x2C => "Print Screen",
        0x2D => "Insert",
        0x2E => "Delete",
        0x6A => "Num *",
        0x6B => "Num +",
        0x6D => "Num -",
        0x6E => "Num .",
        0x6F => "Num /",
        0x91 => "Scroll Lock",
        0xAD => "Mute",
        0xAE => "Volume Down",
        0xAF => "Volume Up",
        0xB0 => "Next Track",
        0xB1 => "Previous Track",
        0xB2 => "Stop",
        0xB3 => "Play/Pause",
        0xBA => ";",
        0xBB => "=",
        0xBC => ",",
        0xBD => "-",
        0xBE => ".",
        0xBF => "/",
        0xC0 => "`",
        0xDB => "[",
        0xDC => "\\",
        0xDD => "]",
        0xDE => "'",
        _ => "",
    };
    if !named.is_empty() {
        return named.to_string();
    }
    match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
        0x60..=0x69 => format!("Num {}", vk - 0x60),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        _ => format!("Key 0x{vk:02X}"),
    }
}

#[cfg(windows)]
pub use hook::HotkeyService;

#[cfg(windows)]
mod hook {
    use super::{Event, HotkeyConfig, Matcher};
    use std::sync::Mutex;
    use std::sync::mpsc;
    use std::thread::JoinHandle;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, HC_ACTION, HHOOK,
        KBDLLHOOKSTRUCT, MSG, PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
        WH_KEYBOARD, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    type Emit = Box<dyn FnMut(Event) + Send>;

    /// Shared with the hook procedure (a plain function pointer, so state must be global).
    static STATE: Mutex<Option<(Matcher, Emit)>> = Mutex::new(None);

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            // SAFETY: for WH_KEYBOARD_LL with HC_ACTION, lparam points to a KBDLLHOOKSTRUCT.
            let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            let msg = wparam.0 as u32;
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
            if (down || up)
                && !own_window_in_front()
                && let Ok(mut guard) = STATE.lock()
                && let Some((matcher, emit)) = guard.as_mut()
            {
                matcher.key(kb.vkCode as u16, down, emit.as_mut());
            }
        }
        // Always pass the key on: we only listen.
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    /// Keyboard messages for our own window (installed on the window thread; see module docs).
    unsafe extern "system" fn thread_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // HC_NOREMOVE repeats a message that is only being peeked; handle each key once.
        if code == HC_ACTION as i32 {
            let vk = wparam.0 as u16;
            let released = (lparam.0 >> 31) & 1 == 1;
            if let Ok(mut guard) = STATE.lock()
                && let Some((matcher, emit)) = guard.as_mut()
            {
                matcher.key(vk, !released, emit.as_mut());
            }
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    /// Our window has focus: the thread hook handles keys (see module docs).
    fn own_window_in_front() -> bool {
        // SAFETY: plain queries; a null window yields pid 0.
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid));
            pid == GetCurrentProcessId()
        }
    }

    /// Runs the hook on its own thread (a low-level hook needs a message loop on that thread).
    pub struct HotkeyService {
        thread_id: u32,
        thread: Option<JoinHandle<()>>,
        /// Thread hook on the window thread (keys while our own window is focused).
        window_hook: Option<HHOOK>,
    }

    impl HotkeyService {
        /// Call from the window (UI) thread: the thread-local hook is installed on the caller.
        pub fn start(config: HotkeyConfig, emit: impl FnMut(Event) + Send + 'static) -> Option<Self> {
            *STATE.lock().ok()? = Some((Matcher::new(config), Box::new(emit)));
            let (tx, rx) = mpsc::channel();
            let thread = std::thread::Builder::new()
                .name("hotkeys".into())
                .spawn(move || {
                    // SAFETY: standard hook + message loop on this thread; the hook is removed
                    // before the thread exits.
                    unsafe {
                        let module = GetModuleHandleW(None).ok();
                        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), module.map(Into::into), 0);
                        let _ = tx.send(hook.as_ref().ok().map(|_| GetCurrentThreadId()));
                        let Ok(hook) = hook else { return };
                        let mut msg = MSG::default();
                        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                            let _ = TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                        let _ = UnhookWindowsHookEx(hook);
                    }
                })
                .ok()?;
            match rx.recv() {
                Ok(Some(thread_id)) => {
                    // SAFETY: thread hook for the calling (window) thread, removed in Drop.
                    let window_hook =
                        unsafe { SetWindowsHookExW(WH_KEYBOARD, Some(thread_proc), None, GetCurrentThreadId()) }.ok();
                    if window_hook.is_none() {
                        log::warn!("hotkeys won't work while this window is focused (thread hook failed)");
                    }
                    Some(Self { thread_id, thread: Some(thread), window_hook })
                }
                _ => {
                    log::warn!("global hotkeys unavailable: keyboard hook could not be installed");
                    let _ = thread.join();
                    None
                }
            }
        }

        pub fn set_config(&self, config: HotkeyConfig) {
            if let Ok(mut g) = STATE.lock()
                && let Some((m, _)) = g.as_mut()
            {
                m.config = config;
            }
        }

        /// Capture the next key combination for the settings UI (reported as `Event::Captured`).
        pub fn capture(&self, on: bool) {
            if let Ok(mut g) = STATE.lock()
                && let Some((m, _)) = g.as_mut()
            {
                m.capture = on;
            }
        }
    }

    impl Drop for HotkeyService {
        fn drop(&mut self) {
            // SAFETY: removing our own hook; posting WM_QUIT to our hook thread ends its loop.
            unsafe {
                if let Some(h) = self.window_hook.take() {
                    let _ = UnhookWindowsHookEx(h);
                }
                let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
            if let Ok(mut g) = STATE.lock() {
                *g = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: u16 = 0xA2;
    const ALT: u16 = 0xA4;
    const SHIFT: u16 = 0xA0;
    const V: u16 = 0x56;

    fn feed(m: &mut Matcher, keys: &[(u16, bool)]) -> Vec<Event> {
        let mut out = Vec::new();
        for &(vk, down) in keys {
            m.key(vk, down, &mut |e| out.push(e));
        }
        out
    }

    #[test]
    fn combo_fires_once_despite_auto_repeat_and_releases() {
        let mut m = Matcher::new(HotkeyConfig::default());
        let ev = feed(&mut m, &[(CTRL, true), (ALT, true), (V, true), (V, true), (V, true), (V, false), (ALT, false)]);
        assert_eq!(ev, vec![Event::Pressed(Action::ToggleEffects), Event::Released(Action::ToggleEffects)]);
    }

    #[test]
    fn modifiers_must_match_exactly() {
        let mut m = Matcher::new(HotkeyConfig::default());
        // Ctrl+Alt+Shift+V is not Ctrl+Alt+V; plain V is not either.
        assert!(feed(&mut m, &[(CTRL, true), (ALT, true), (SHIFT, true), (V, true), (V, false)]).is_empty());
        assert!(feed(&mut m, &[(SHIFT, false), (CTRL, false), (ALT, false), (V, true), (V, false)]).is_empty());
        // Right-hand modifiers count too.
        let ev = feed(&mut m, &[(0xA3, true), (0xA5, true), (V, true)]);
        assert_eq!(ev, vec![Event::Pressed(Action::ToggleEffects)]);
    }

    #[test]
    fn hold_reports_release_even_if_modifiers_go_first() {
        let mut cfg = HotkeyConfig::default();
        cfg.bindings
            .insert(Action::HoldEffects, Hotkey { ctrl: false, alt: false, shift: false, win: false, vk: 0x14 });
        let mut m = Matcher::new(cfg);
        let ev = feed(&mut m, &[(0x14, true), (0x14, true), (0x14, false)]);
        assert_eq!(ev, vec![Event::Pressed(Action::HoldEffects), Event::Released(Action::HoldEffects)]);
    }

    #[test]
    fn capture_mode_records_cancels_and_clears() {
        let mut m = Matcher::new(HotkeyConfig::default());
        m.capture = true;
        let ev = feed(&mut m, &[(SHIFT, true), (0x70, true)]);
        assert_eq!(
            ev,
            vec![Event::Captured(Some(Hotkey { ctrl: false, alt: false, shift: true, win: false, vk: 0x70 }))]
        );
        assert!(!m.capture, "capture ends after one combination");
        m.capture = true;
        assert_eq!(feed(&mut m, &[(SHIFT, false), (0x1B, true)]), vec![Event::CaptureCancelled]);
        m.capture = true;
        assert_eq!(feed(&mut m, &[(0x1B, false), (0x08, true)]), vec![Event::Captured(None)]);
    }

    #[test]
    fn disabled_config_fires_nothing() {
        let mut m = Matcher::new(HotkeyConfig { enabled: false, ..Default::default() });
        assert!(feed(&mut m, &[(CTRL, true), (ALT, true), (V, true)]).is_empty());
    }

    #[test]
    fn labels_and_conflicts() {
        assert_eq!(Hotkey::ctrl_alt(0x22).label(), "Ctrl+Alt+Page Down");
        assert_eq!(Hotkey { ctrl: false, alt: false, shift: true, win: false, vk: 0x7B }.label(), "Shift+F12");
        assert_eq!(key_name(0x65), "Num 5");
        let cfg = HotkeyConfig::default();
        assert_eq!(cfg.conflict(&Hotkey::ctrl_alt(0x56), Action::NextPreset), Some(Action::ToggleEffects));
        assert_eq!(cfg.conflict(&Hotkey::ctrl_alt(0x56), Action::ToggleEffects), None);
        let json = serde_json::to_string(&cfg).unwrap();
        assert_eq!(serde_json::from_str::<HotkeyConfig>(&json).unwrap(), cfg);
    }
}
