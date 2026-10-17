//! Test and bench binaries that link Tauri need the Common Controls v6 side-by-side manifest on Windows.
//!
//! Tauri's window stack (tao, wry, muda) imports `TaskDialogIndirect` from comctl32, which exists only
//! in version 6. An application gets the manifest from `tauri-build`; a Cargo test binary has none, so
//! the loader binds comctl32 v5 and the process dies before `main` with `STATUS_ENTRYPOINT_NOT_FOUND`
//! (0xC0000139). On an interactive session the loader also shows a modal error dialog, which hangs
//! an unattended build host. Embedding the dependency in the test binaries fixes the cause. The
//! build host should additionally run tests with `SEM_FAILCRITICALERRORS` set (`SetErrorMode`), which
//! children inherit and which turns any other loader failure into a plain exit code instead of a dialog.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let windows_msvc = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|v| v == "windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|v| v == "msvc");
    if windows_msvc {
        // `tests` covers integration tests; `benches` covers the budget bench, which runs a real Wry
        // event loop and so links the same comctl32 imports.
        for target in ["tests", "benches"] {
            println!("cargo:rustc-link-arg-{target}=/MANIFEST:EMBED");
            println!(
                "cargo:rustc-link-arg-{target}=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
            );
        }
    }
}
