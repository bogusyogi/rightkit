//! The layout the shipped `membrane` runs in, shared by the end-to-end suites that need a
//! resolved state root.
//!
//! The release binary reads no environment override for its stores (`qa_env` returns `None` in
//! every build). It resolves its state root from the per-user data folders (`HOME`,
//! `LOCALAPPDATA`, `XDG_DATA_HOME`), and it opens the Cortex store only when it runs from the
//! host app's `Helpers` folder (`runtime_from_installed_exe`). So a suite isolates itself the way
//! a user's machine is laid out: its own home folder, and a copy of the binary placed in
//! `<home>/Contents/Helpers/`, exactly where the host app puts it.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// The product state root under `home`: `<data base>/Orthic Labs/Membrane/state`, where the data
/// base is `~/Library/Application Support` (macOS), `%LOCALAPPDATA%` (Windows) or
/// `$XDG_DATA_HOME` (Linux, pinned by [`isolate_home`] to `~/.local/share`).
pub fn state_root_under(home: &Path) -> PathBuf {
    let base = if cfg!(windows) {
        home.to_path_buf()
    } else if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support")
    } else {
        home.join(".local").join("share")
    };
    base.join("Orthic Labs").join("Membrane").join("state")
}

/// Where the installed copy of the binary lives for `home`: `<home>/Contents/Helpers/membrane`.
pub fn installed_binary(home: &Path) -> PathBuf {
    let built = Path::new(env!("CARGO_BIN_EXE_membrane"));
    home.join("Contents")
        .join("Helpers")
        .join(built.file_name().expect("the built binary has a file name"))
}

/// Place a copy of the built binary at [`installed_binary`], the host app's `Helpers` layout.
pub fn install_binary(home: &Path) -> PathBuf {
    let target = installed_binary(home);
    std::fs::create_dir_all(target.parent().expect("Helpers folder"))
        .expect("create the Helpers folder");
    std::fs::copy(env!("CARGO_BIN_EXE_membrane"), &target)
        .expect("copy the built binary into Helpers");
    target
}

/// Point every per-user data folder the binary resolves at `home`, so the state root is
/// [`state_root_under`] and nothing outside the fixture is read or written.
pub fn isolate_home(command: &mut Command, home: &Path) {
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("APPDATA", home)
        .env("LOCALAPPDATA", home)
        .env("XDG_DATA_HOME", home.join(".local").join("share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_CONFIG_HOME", home.join(".config"));
}
