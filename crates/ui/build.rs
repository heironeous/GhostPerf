//! Embeds the UI's icon and version info into the Windows exe, so it shows
//! the right icon and "GhostPerf" in Explorer and Task Manager. Does nothing
//! on other hosts/targets.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let icon = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/icon/ghostperf-ui.ico"
        );
        println!("cargo:rerun-if-changed={icon}");
        winresource::WindowsResource::new()
            .set_icon(icon)
            .set("ProductName", "GhostPerf")
            .set("FileDescription", "GhostPerf")
            .compile()
            .expect("failed to embed Windows resources (icon/version info)");
    }
}
