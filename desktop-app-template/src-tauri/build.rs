fn main() {
    // Plain `cargo check` in dev mode must not require the staged sidecar binary.
    if tauri_build::is_dev() {
        let mut config: serde_json::Value =
            serde_json::from_str(&std::env::var("TAURI_CONFIG").unwrap_or_else(|_| "{}".into()))
                .expect("valid TAURI_CONFIG");
        if config.get("bundle").is_none() {
            config["bundle"] = serde_json::json!({});
        }
        config["bundle"]["externalBin"] = serde_json::Value::Null;
        let compact = serde_json::to_string(&config).expect("serialize TAURI_CONFIG");
        std::env::set_var("TAURI_CONFIG", &compact);
        println!("cargo:rustc-env=TAURI_CONFIG={compact}");
    }
    tauri_build::build()
}
