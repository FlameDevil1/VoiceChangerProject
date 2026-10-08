//! "Start with Windows": a value under `HKCU\...\CurrentVersion\Run`, the same one the
//! installer's checkbox writes and its uninstaller removes. The registry is the only record (no
//! config field), so the app, the installer and Task Manager's Startup page never disagree.
//!
//! Task Manager can disable a startup entry without deleting it: it then marks the value in
//! `Explorer\StartupApproved\Run` (first byte odd = disabled). That counts as off here, and
//! switching it on in the app clears the mark.

/// Command-line flag the startup entry passes: start hidden in the tray.
pub const ARG: &str = "--startup";

const RUN: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const APPROVED: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";
const VALUE: &str = "Voice Changer";

/// Is the app set to start with Windows (and not disabled in Task Manager)?
pub fn is_enabled() -> bool {
    imp::enabled(RUN, APPROVED, VALUE)
}

/// Add or remove the startup entry for this executable.
pub fn set(enabled: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    imp::set(RUN, APPROVED, VALUE, enabled.then(|| command(&exe)).as_deref())
}

/// If the entry points at an executable that no longer exists (the app was moved, or a
/// portable copy replaced the installed one), point it at this one.
pub fn repair() {
    let Some(old) = imp::read(RUN, VALUE) else { return };
    let Some(path) = exe_of(&old) else { return };
    if !std::path::Path::new(&path).exists()
        && let Ok(exe) = std::env::current_exe()
    {
        log::info!("startup entry pointed at a missing {path}; repointing it");
        let _ = imp::write(RUN, VALUE, &command(&exe));
    }
}

fn command(exe: &std::path::Path) -> String {
    format!("\"{}\" {ARG}", exe.display())
}

/// The executable path from a Run command line (`"C:\x\app.exe" --flag` or `C:\x\app.exe`).
fn exe_of(cmd: &str) -> Option<String> {
    let cmd = cmd.trim();
    let path = match cmd.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => cmd.split(" --").next()?,
    };
    (!path.is_empty()).then(|| path.to_string())
}

#[cfg(windows)]
mod imp {
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    };
    use windows::core::HSTRING;

    pub fn read(key: &str, value: &str) -> Option<String> {
        let mut buf = [0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: buffer and length describe valid writable memory.
        let r = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(key),
                &HSTRING::from(value),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut len),
            )
        };
        r.is_ok().then(|| String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]))
    }

    /// Task Manager's "Disabled" mark: an odd first byte.
    fn disabled(key: &str, value: &str) -> bool {
        let mut buf = [0u8; 16];
        let mut len = buf.len() as u32;
        // SAFETY: as above.
        let r = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(key),
                &HSTRING::from(value),
                RRF_RT_REG_BINARY,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut len),
            )
        };
        r.is_ok() && len > 0 && buf[0] & 1 == 1
    }

    pub fn enabled(run: &str, approved: &str, value: &str) -> bool {
        read(run, value).is_some() && !disabled(approved, value)
    }

    pub fn write(key: &str, value: &str, data: &str) -> Result<(), String> {
        let wide: Vec<u16> = data.encode_utf16().chain([0]).collect();
        // SAFETY: `wide` is a NUL-terminated UTF-16 string of the given byte length.
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(key),
                &HSTRING::from(value),
                REG_SZ.0,
                Some(wide.as_ptr().cast()),
                (wide.len() * 2) as u32,
            )
        }
        .ok()
        .map_err(|e| e.message())
    }

    fn delete(key: &str, value: &str) {
        // SAFETY: plain registry call; a missing key or value is fine.
        let _ = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(value)) };
    }

    pub fn set(run: &str, approved: &str, value: &str, command: Option<&str>) -> Result<(), String> {
        // Clearing the mark also re-enables an entry Task Manager had disabled.
        delete(approved, value);
        match command {
            Some(cmd) => write(run, value, cmd),
            None => {
                delete(run, value);
                Ok(())
            }
        }
    }

    #[cfg(test)]
    pub fn delete_tree(key: &str) {
        use windows::Win32::System::Registry::RegDeleteTreeW;
        // SAFETY: plain registry call on a test-only key.
        let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key)) };
    }

    #[cfg(test)]
    pub fn write_binary(key: &str, value: &str, data: &[u8]) {
        use windows::Win32::System::Registry::REG_BINARY;
        // SAFETY: `data` is valid for its length.
        let _ = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(key),
                &HSTRING::from(value),
                REG_BINARY.0,
                Some(data.as_ptr().cast()),
                data.len() as u32,
            )
        };
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn read(_: &str, _: &str) -> Option<String> {
        None
    }
    pub fn enabled(_: &str, _: &str, _: &str) -> bool {
        false
    }
    pub fn write(_: &str, _: &str, _: &str) -> Result<(), String> {
        Err("only supported on Windows".into())
    }
    pub fn set(_: &str, _: &str, _: &str, _: Option<&str>) -> Result<(), String> {
        Err("only supported on Windows".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_from_command_line() {
        assert_eq!(exe_of(r#""C:\Program Files\VC\vc.exe" --startup"#).as_deref(), Some(r"C:\Program Files\VC\vc.exe"));
        assert_eq!(exe_of(r"C:\VC\vc.exe --startup").as_deref(), Some(r"C:\VC\vc.exe"));
        assert_eq!(exe_of(r"C:\VC\vc.exe").as_deref(), Some(r"C:\VC\vc.exe"));
        assert_eq!(exe_of(""), None);
    }

    /// Round trip on a scratch key (never the real Run key).
    #[cfg(windows)]
    #[test]
    fn enable_disable_and_task_manager_mark() {
        let base = format!("Software\\VoiceChangerTest{}", std::process::id());
        let (run, approved) = (format!("{base}\\Run"), format!("{base}\\Approved"));
        let cmd = r#""C:\VC\vc.exe" --startup"#;

        assert!(!imp::enabled(&run, &approved, VALUE));
        imp::set(&run, &approved, VALUE, Some(cmd)).unwrap();
        assert!(imp::enabled(&run, &approved, VALUE));
        assert_eq!(imp::read(&run, VALUE).as_deref(), Some(cmd));

        // Disabled in Task Manager: off, until switched on again here.
        imp::write_binary(&approved, VALUE, &[3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(!imp::enabled(&run, &approved, VALUE));
        imp::set(&run, &approved, VALUE, Some(cmd)).unwrap();
        assert!(imp::enabled(&run, &approved, VALUE));

        imp::set(&run, &approved, VALUE, None).unwrap();
        assert!(!imp::enabled(&run, &approved, VALUE));
        imp::delete_tree(&base);
    }
}
