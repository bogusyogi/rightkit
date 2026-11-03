fn main() {
    println!("cargo:rerun-if-changed=src/coreml_prediction_bridge.m");

    #[cfg(target_os = "macos")]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("src/coreml_prediction_bridge.m")
            .flag("-fobjc-arc")
            .compile("rightkit_dictation_coreml_bridge");
    }
}
