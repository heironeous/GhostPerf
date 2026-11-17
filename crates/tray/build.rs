//! Embeds the tray's icons and version info into the Windows exe. Does
//! nothing on other hosts/targets.
//!
//! Resource 1 (the idle icon) is also the exe's own icon in Explorer.
//! Resource 2 is the recording icon. `src/icons.rs` loads both by these IDs at
//! runtime, so Windows can pick the hand-tuned 16/20/24px frames for the
//! notification area instead of scaling one bitmap.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let idle = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/icon/ghostperf.ico"
        );
        let recording = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/icon/ghostperf-recording.ico"
        );
        println!("cargo:rerun-if-changed={idle}");
        println!("cargo:rerun-if-changed={recording}");
        winresource::WindowsResource::new()
            .set_icon(idle) // resource ID 1, must match icons::IDLE_RESOURCE_ID
            .set_icon_with_id(recording, "2") // must match icons::RECORDING_RESOURCE_ID
            .set("ProductName", "GhostPerf")
            .set("FileDescription", "GhostPerf Tray")
            .compile()
            .expect("failed to embed Windows resources (icons/version info)");
    }
}
