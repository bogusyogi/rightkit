#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn main() -> anyhow::Result<()> {
    rightkit_dictation_runtime::entry::run()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn main() {
    eprintln!("rightkit-dictation-engine runs on macOS and Windows only");
    std::process::exit(1);
}
