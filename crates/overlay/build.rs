//! Embeds version info (and the main brand icon, for Explorer/Task Manager
//! — the overlay has no taskbar/Alt-Tab presence of its own by design, see
//! TODO.md Phase 6) into the Windows exe. Does nothing on other targets.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let icon = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/icon/ghostperf.ico"
        );
        println!("cargo:rerun-if-changed={icon}");
        winresource::WindowsResource::new()
            .set_icon(icon)
            .set("ProductName", "GhostPerf")
            .set("FileDescription", "GhostPerf Overlay")
            .compile()
            .expect("failed to embed Windows resources (icon/version info)");
    }
}
