//! Notices when an app the hotkeys can't reach comes to the front.
//!
//! Windows keeps input aimed at a higher-integrity app (one running as administrator) away from
//! hooks in a lower one, so global hotkeys silently stop working while such an app is focused.
//! A WinEvent hook reports each foreground change (no polling); when the new foreground process
//! runs at a higher integrity level than ours, or hides its token from us (which elevated apps
//! do), the callback gets its executable name.

/// Windows tools that run elevated but where nobody needs voice hotkeys: not worth a warning.
const IGNORED: [&str; 4] = ["taskmgr.exe", "mmc.exe", "regedit.exe", "consent.exe"];

/// Start watching on a small thread of its own. `on_blocked` runs on that thread.
#[cfg(windows)]
pub fn watch(on_blocked: impl FnMut(String) + Send + 'static) {
    imp::watch(Box::new(on_blocked));
}

#[cfg(not(windows))]
pub fn watch(_: impl FnMut(String) + Send + 'static) {}

#[cfg(windows)]
mod imp {
    use super::IGNORED;
    use std::cell::RefCell;
    use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, HANDLE, HWND};
    use windows::Win32::Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
        TokenIntegrityLevel,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, EVENT_SYSTEM_FOREGROUND, GetMessageW, GetWindowThreadProcessId, MSG, WINEVENT_OUTOFCONTEXT,
        WINEVENT_SKIPOWNPROCESS,
    };
    use windows::core::PWSTR;

    type Callback = Box<dyn FnMut(String) + Send>;

    thread_local! {
        /// (our integrity level, callback), owned by the watcher thread.
        static STATE: RefCell<Option<(u32, Callback)>> = const { RefCell::new(None) };
    }

    pub fn watch(callback: Callback) {
        let spawned =
            std::thread::Builder::new().name("elevation-watch".into()).stack_size(64 * 1024).spawn(move || {
                // SAFETY: reading our own token.
                let own = unsafe { integrity(GetCurrentProcess()) }.unwrap_or(0);
                STATE.with(|s| *s.borrow_mut() = Some((own, callback)));
                // SAFETY: an out-of-context hook delivered through this thread's message loop.
                let hook = unsafe {
                    SetWinEventHook(
                        EVENT_SYSTEM_FOREGROUND,
                        EVENT_SYSTEM_FOREGROUND,
                        None,
                        Some(on_foreground),
                        0,
                        0,
                        WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                    )
                };
                if hook.is_invalid() {
                    log::warn!("foreground hook failed; no warning for apps running as administrator");
                    return;
                }
                let mut msg = MSG::default();
                // SAFETY: standard message loop; runs for the life of the process.
                while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
                    unsafe { DispatchMessageW(&msg) };
                }
            });
        if let Err(e) = spawned {
            log::warn!("elevation watcher: {e}");
        }
    }

    unsafe extern "system" fn on_foreground(_: HWINEVENTHOOK, _: u32, hwnd: HWND, _: i32, _: i32, _: u32, _: u32) {
        let mut pid = 0;
        // SAFETY: `hwnd` comes from the event; a stale handle just yields pid 0.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == 0 {
            return;
        }
        STATE.with(|s| {
            if let Some((own, callback)) = s.borrow_mut().as_mut()
                && let Some(name) = blocked(pid, *own)
            {
                callback(name);
            }
        });
    }

    /// The executable name of `pid` if it runs above `own` integrity, else `None`.
    pub fn blocked(pid: u32, own: u32) -> Option<String> {
        // SAFETY: handles are checked and closed; buffers are sized as passed.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let higher = match integrity(process) {
                Ok(level) => level > own,
                // Elevated apps' tokens are off limits to us; other failures prove nothing.
                Err(denied) => denied,
            };
            let name = higher.then(|| exe_name(process)).flatten();
            let _ = CloseHandle(process);
            name.filter(|n| !IGNORED.iter().any(|i| n.eq_ignore_ascii_case(i)))
        }
    }

    /// The process's integrity level RID (0x2000 medium, 0x3000 high, ...). `Err(true)` when
    /// access to its token was denied.
    unsafe fn integrity(process: HANDLE) -> Result<u32, bool> {
        unsafe {
            let mut token = HANDLE::default();
            if let Err(e) = OpenProcessToken(process, TOKEN_QUERY, &mut token) {
                return Err(e.code() == E_ACCESSDENIED);
            }
            let mut buf = [0u64; 16]; // aligned room for the label and its SID
            let mut len = 0;
            let ok = GetTokenInformation(
                token,
                TokenIntegrityLevel,
                Some(buf.as_mut_ptr().cast()),
                size_of_val(&buf) as u32,
                &mut len,
            );
            let _ = CloseHandle(token);
            ok.map_err(|_| false)?;
            let label = &*buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
            let sid = label.Label.Sid;
            let count = *GetSidSubAuthorityCount(sid);
            Ok(*GetSidSubAuthority(sid, u32::from(count).saturating_sub(1)))
        }
    }

    unsafe fn exe_name(process: HANDLE) -> Option<String> {
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        // SAFETY: buffer and length match.
        unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit('\\').next().map(str::to_string)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn own_process_is_not_blocked() {
            // SAFETY: reading our own token.
            let own = unsafe { integrity(GetCurrentProcess()) }.expect("own integrity level");
            assert!(own >= 0x1000, "at least low integrity: {own:#x}");
            assert_eq!(blocked(std::process::id(), own), None);
            // Seen from below (as an unelevated copy would), this process counts as blocked.
            let name = blocked(std::process::id(), own - 1).expect("higher integrity is reported");
            assert!(name.to_ascii_lowercase().ends_with(".exe"), "{name}");
        }
    }
}
