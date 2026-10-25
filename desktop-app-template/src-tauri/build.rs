fn main() {
    // A plain `cargo check` or `cargo test` (Tauri dev mode) and the QA build (`qa-native`, which launches the engine from
    // RIGHTAPP_ENGINE_BIN) must not require the staged sidecar binary. This only writes TAURI_CONFIG; it reads no override.
    if tauri_build::is_dev() || std::env::var_os("CARGO_FEATURE_QA_NATIVE").is_some() {
        let config = r#"{"bundle":{"externalBin":null}}"#;
        std::env::set_var("TAURI_CONFIG", config);
        println!("cargo:rustc-env=TAURI_CONFIG={config}");
    }
    tauri_build::build()
}
