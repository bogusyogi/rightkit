//! Package-manager-managed installs (#970): an app a package manager or a store installed must not
//! replace itself. The manager owns the files (and their integrity record), so a self-update would
//! fight it, break its uninstall, or be reverted at its next run.
//!
//! [`detect`] reads the running executable's path and the container environment; [`classify`] is the
//! same decision over a path and an environment lookup. The update host refuses a check on a managed
//! install unless the policy opts in (`UpdatePolicy::allow_managed_install`).

use std::path::Path;

/// Which manager owns this install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedInstall {
    /// Mac App Store (the bundle carries a `_MASReceipt`).
    MacAppStore,
    /// Homebrew or Linuxbrew (a Cellar or Caskroom path).
    Homebrew,
    MacPorts,
    Nix,
    /// MSIX or Microsoft Store (`WindowsApps`).
    MsixStore,
    Scoop,
    Chocolatey,
    /// A WinGet portable package.
    Winget,
    Snap,
    Flatpak,
    /// A distribution package (deb, rpm, pacman) under `/usr` (not `/usr/local`).
    SystemPackage,
}

impl ManagedInstall {
    /// A stable lowercase name, for a log line or a diagnostics field.
    pub fn name(self) -> &'static str {
        match self {
            Self::MacAppStore => "mac-app-store",
            Self::Homebrew => "homebrew",
            Self::MacPorts => "macports",
            Self::Nix => "nix",
            Self::MsixStore => "msix-store",
            Self::Scoop => "scoop",
            Self::Chocolatey => "chocolatey",
            Self::Winget => "winget",
            Self::Snap => "snap",
            Self::Flatpak => "flatpak",
            Self::SystemPackage => "system-package",
        }
    }
}

/// The manager that owns the running executable, or `None` for a self-managed install.
pub fn detect() -> Option<ManagedInstall> {
    let exe = std::env::current_exe().ok()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    classify(&exe, |name| std::env::var(name).ok())
}

/// The decision of [`detect`] over an executable path and an environment lookup. Pure apart from the
/// one file check for a Mac App Store receipt next to the bundle.
pub fn classify(exe: &Path, env: impl Fn(&str) -> Option<String>) -> Option<ManagedInstall> {
    let set = |name: &str| env(name).is_some_and(|v| !v.is_empty());
    if set("FLATPAK_ID") {
        return Some(ManagedInstall::Flatpak);
    }
    if set("SNAP") && set("SNAP_NAME") {
        return Some(ManagedInstall::Snap);
    }
    // Compare case-insensitively with `/` separators, so one table serves every platform.
    let path = exe
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let has = |part: &str| path.contains(part);
    if has("/nix/store/") {
        return Some(ManagedInstall::Nix);
    }
    if has("/cellar/") || has("/caskroom/") || has("/.linuxbrew/") || has("/linuxbrew/") {
        return Some(ManagedInstall::Homebrew);
    }
    if path.starts_with("/opt/local/") {
        return Some(ManagedInstall::MacPorts);
    }
    if has("/windowsapps/") {
        return Some(ManagedInstall::MsixStore);
    }
    if has("/scoop/apps/") || has("/scoop/shims/") {
        return Some(ManagedInstall::Scoop);
    }
    if has("/chocolatey/lib/") || has("/chocolatey/bin/") {
        return Some(ManagedInstall::Chocolatey);
    }
    if has("/winget/packages/") || has("/winget/links/") {
        return Some(ManagedInstall::Winget);
    }
    if path.starts_with("/snap/") {
        return Some(ManagedInstall::Snap);
    }
    if path.starts_with("/var/lib/flatpak/") || path.starts_with("/app/") {
        return Some(ManagedInstall::Flatpak);
    }
    if mac_app_store_bundle(exe) {
        return Some(ManagedInstall::MacAppStore);
    }
    if path.starts_with("/usr/") && !path.starts_with("/usr/local/") {
        return Some(ManagedInstall::SystemPackage);
    }
    None
}

/// `Foo.app/Contents/MacOS/foo` with `Foo.app/Contents/_MASReceipt/receipt` beside it.
fn mac_app_store_bundle(exe: &Path) -> bool {
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")))
        .is_some_and(|bundle| bundle.join("Contents/_MASReceipt/receipt").is_file())
}
