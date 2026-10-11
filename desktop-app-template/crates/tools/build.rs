fn main() {
    // The host triple this binary was built for. stage-engine compares it with TAURI_ENV_TARGET_TRIPLE
    // at run time: the engine build is not given --target, so its output lives under this triple only.
    let target = std::env::var("TARGET").expect("cargo sets TARGET for build scripts");
    println!("cargo:rustc-env=RIGHTAPP_HOST_TRIPLE={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
