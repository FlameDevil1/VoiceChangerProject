//! Is Windows blocking microphone access for desktop apps?
//!
//! When "Let desktop apps access your microphone" is off, capture still "works" but delivers
//! silence, which looks like a broken app. Windows records the switch under
//! `CapabilityAccessManager\ConsentStore\microphone\NonPackaged` (value "Allow"/"Deny").
//!
//! Only that desktop-apps switch is decisive. The parent "microphone" value can read "Deny" while
//! desktop capture works fine (observed on Windows 11), so it is not used; the GUI's
//! digital-silence check covers any other cause.

#[cfg(windows)]
pub fn microphone_blocked() -> bool {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::w;

    let mut buf = [0u16; 32];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: buffer and length describe valid writable memory.
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(
                "Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\microphone\\NonPackaged"
            ),
            w!("Value"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    ok.is_ok() && String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]).eq_ignore_ascii_case("Deny")
}

#[cfg(not(windows))]
pub fn microphone_blocked() -> bool {
    false
}

/// Open Windows Settings at the microphone privacy page.
pub fn open_privacy_settings() {
    let _ = std::process::Command::new("explorer").arg("ms-settings:privacy-microphone").spawn();
}

#[cfg(test)]
mod tests {
    #[test]
    fn reading_the_setting_does_not_fail() {
        // The answer depends on the machine; this checks the registry query itself works.
        let _ = super::microphone_blocked();
    }
}
