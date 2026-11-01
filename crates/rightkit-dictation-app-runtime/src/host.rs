//! Names the host app gives its engine, configured once at startup.

use std::sync::OnceLock;

/// How the host app names and packages its speech engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineHost {
    /// Engine executable name without extension (`.exe` is added on Windows).
    pub binary_stem: &'static str,
    /// macOS bundle identifier of the app (its ANE compile-cache namespace); empty for none.
    pub bundle_identifier: &'static str,
    /// Directory name of the bundled engine app payload under `app/` in the runtime resources.
    pub app_resource_dir: &'static str,
}

/// Defaults until [`configure`]: the `rightkit-dictation-engine` binary, no bundle identifier.
pub const DEFAULT: EngineHost = EngineHost {
    binary_stem: "rightkit-dictation-engine",
    bundle_identifier: "",
    app_resource_dir: "rightkit-dictation",
};

static HOST: OnceLock<EngineHost> = OnceLock::new();

/// Configure the engine names once. Returns false when already set.
pub fn configure(host: EngineHost) -> bool {
    HOST.set(host).is_ok()
}

/// The configured engine names ([`DEFAULT`] until [`configure`]).
pub fn engine_host() -> &'static EngineHost {
    HOST.get().unwrap_or(&DEFAULT)
}

/// The engine executable file name for this platform.
pub fn engine_bin_name() -> String {
    let stem = engine_host().binary_stem;
    if cfg!(target_os = "windows") {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}
