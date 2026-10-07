//! One app instance at a time.
//!
//! Two copies would fight over the audio devices and the config file. The first instance owns a
//! named mutex and listens on a named event; a second launch sets that event (so the running
//! copy shows its window, even from the tray) and exits.

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent, WaitForSingleObject,
};
use windows::core::{PCWSTR, w};

const MUTEX: PCWSTR = w!("Local\\VoiceChanger.SingleInstance");
const SHOW_EVENT: PCWSTR = w!("Local\\VoiceChanger.Show");

pub enum Instance {
    /// We are the only instance; keep the guard alive for the app's lifetime.
    Primary(Guard),
    /// Another instance is running and has been asked to show itself.
    Secondary,
}

pub struct Guard {
    mutex: HANDLE,
    event: HANDLE,
}

// SAFETY: kernel object handles can be used from any thread.
unsafe impl Send for Guard {}

pub fn acquire() -> Instance {
    // SAFETY: plain Win32 calls with static, NUL-terminated names.
    unsafe {
        let Ok(mutex) = CreateMutexW(None, false, MUTEX) else {
            // Can't tell; don't block the user from starting the app.
            return Instance::Primary(Guard { mutex: HANDLE::default(), event: HANDLE::default() });
        };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(mutex);
            if let Ok(ev) = OpenEventW(EVENT_MODIFY_STATE, false, SHOW_EVENT) {
                let _ = SetEvent(ev);
                let _ = CloseHandle(ev);
            }
            return Instance::Secondary;
        }
        let event = CreateEventW(None, false, false, SHOW_EVENT).unwrap_or_default();
        Instance::Primary(Guard { mutex, event })
    }
}

impl Guard {
    /// Call `on_show` whenever another launch asks this instance to come to the front.
    pub fn listen(&self, on_show: impl Fn() + Send + 'static) {
        if self.event.is_invalid() {
            return;
        }
        let event = self.event.0 as usize;
        let _ = std::thread::Builder::new().name("single-instance".into()).spawn(move || {
            let event = HANDLE(event as *mut _);
            // SAFETY: the event handle stays open for the process lifetime (the guard is never
            // dropped before exit), and waiting on it from another thread is allowed.
            while unsafe { WaitForSingleObject(event, INFINITE) } == WAIT_OBJECT_0 {
                on_show();
            }
        });
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // SAFETY: handles were created by us; closing an invalid handle is ignored.
        unsafe {
            let _ = CloseHandle(self.event);
            let _ = CloseHandle(self.mutex);
        }
    }
}
