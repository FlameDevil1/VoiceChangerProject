//! Embeds the icon and version details in the Windows executables, so Explorer, the Start menu,
//! Task Manager and "Apps & features" show "Voice Changer" with its icon.

fn main() {
    println!("cargo:rerun-if-changed=assets/voicechanger.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/voicechanger.ico")
        .set("ProductName", "Voice Changer")
        .set("FileDescription", "Voice Changer")
        .set("LegalCopyright", "Copyright (c) 2026 FlameDevil1. MIT License.");
    if let Err(e) = res.compile() {
        // A missing resource compiler only costs the icon; don't fail the build over it.
        println!("cargo:warning=no icon or version info embedded: {e}");
    }
}
