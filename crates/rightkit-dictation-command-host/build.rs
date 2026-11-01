fn main() {
    println!("cargo:rerun-if-changed=src/screencapture_bridge.m");

    #[cfg(target_os = "macos")]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("src/screencapture_bridge.m")
            .flag("-fobjc-arc")
            .flag("-fblocks")
            .compile("rightkit_dictation_screencapture_bridge");
        println!("cargo:rustc-link-lib=framework=AppKit");
        println!("cargo:rustc-link-lib=framework=ScreenCaptureKit");
    }
}
