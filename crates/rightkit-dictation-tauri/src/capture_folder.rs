//! The captured-audio folder used by the ASR compatibility check: its layout (`<root>/audio`),
//! the one-time migration from earlier folder names, and the "any capture present" probe.
//! The host supplies the three resolved folder paths.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use rightkit_fs::{rename_exclusive, FsError};

/// The host's folder names, already resolved to paths.
#[derive(Debug, Clone)]
pub struct Folders {
    /// Where captures live now.
    pub current: PathBuf,
    /// The earlier folder name, renamed to `current` (or to `neutral_legacy`) when found.
    pub legacy: PathBuf,
    /// Where a legacy folder is kept when `current` already exists; its WAVs are moved into
    /// `current/audio`.
    pub neutral_legacy: PathBuf,
}

/// Migrate, create `<current>/audio` and return `current`.
pub fn prepare_root(folders: &Folders) -> Result<PathBuf, String> {
    let root = folders.current.clone();
    let legacy = &folders.legacy;
    let neutral_legacy = &folders.neutral_legacy;
    if legacy.is_dir() && !root.exists() {
        std::fs::rename(legacy, &root)
            .map_err(|error| format!("migrate captured audio folder: {error}"))?;
    } else if legacy.is_dir() && !neutral_legacy.exists() {
        std::fs::rename(legacy, neutral_legacy)
            .map_err(|error| format!("migrate legacy captured audio folder: {error}"))?;
    }
    let audio = root.join("audio");
    std::fs::create_dir_all(&audio)
        .map_err(|error| format!("create captured audio folder: {error}"))?;
    move_captured_wavs(&neutral_legacy.join("audio"), &audio)?;
    Ok(root)
}

fn move_captured_wavs(source: &Path, destination: &Path) -> Result<(), String> {
    let Ok(entries) = std::fs::read_dir(source) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry.map_err(|error| format!("read legacy capture: {error}"))?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("wav") {
            continue;
        }
        let file_name = entry.file_name();
        let mut target = destination.join(&file_name);
        for collision in 0.. {
            match rename_exclusive(&path, &target) {
                Ok(()) => break,
                Err(FsError::AlreadyExists(_)) => {
                    let mut candidate = if collision == 0 {
                        OsString::from("legacy-")
                    } else {
                        OsString::from(format!("legacy-{collision}-"))
                    };
                    candidate.push(&file_name);
                    target = destination.join(candidate);
                }
                Err(error) => return Err(format!("migrate captured WAV: {error}")),
            }
        }
    }
    Ok(())
}

/// True when `<root>/audio` holds at least one `.wav` file.
pub fn audio_present(root: &Path) -> bool {
    let audio = root.join("audio");
    std::fs::read_dir(audio).ok().is_some_and(|mut entries| {
        entries.any(|entry| {
            entry.is_ok_and(|entry| {
                entry.path().is_file()
                    && entry.path().extension().and_then(|value| value.to_str()) == Some("wav")
            })
        })
    })
}
