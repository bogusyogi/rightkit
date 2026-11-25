//! The profile's file store: `profile.jsonl` (the neutral export) in a folder the host names, replaced atomically and
//! private to the owner, with one readable Markdown view per area beside it. The views are never read back.
//!
//! Needs `rightkit-fs` with its `fs` feature. The host decides the folder and the wording of the views' first line.

use crate::area::Area;
use crate::neutral;
use crate::profile::ProfileState;
use std::fmt;
use std::path::Path;

/// The file the profile is kept in, inside the host's folder.
pub const STATE_FILE: &str = "profile.jsonl";

/// Why the store could not read or write. Neither variant carries an entry value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The file could not be read, or a line in it was refused. The file was not changed.
    Unreadable(String),
    /// The profile could not be written.
    Write(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Unreadable(why) | StoreError::Write(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for StoreError {}

fn io(error: impl fmt::Display) -> StoreError {
    StoreError::Write(error.to_string())
}

/// The saved profile; a missing file is an empty profile, and a file that cannot be read, or that has a line the crate
/// refuses, is an error and is never overwritten.
pub fn load(dir: &Path) -> Result<ProfileState, StoreError> {
    let path = dir.join(STATE_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProfileState::default());
        }
        Err(error) => return Err(StoreError::Unreadable(error.to_string())),
    };
    let mut state = ProfileState::default();
    let report = neutral::import(&mut state, &text);
    if !report.rejected.is_empty() {
        return Err(StoreError::Unreadable(format!(
            "{} line(s) refused",
            report.rejected.len()
        )));
    }
    Ok(state)
}

/// Replaces the saved profile and rewrites the area views. `view_intro` is the host's sentence after each view's title.
pub fn save(dir: &Path, state: &ProfileState, view_intro: &str) -> Result<(), StoreError> {
    rightkit_fs::ensure_private_dir(dir).map_err(io)?;
    let text = neutral::export(state).map_err(io)?;
    rightkit_fs::atomic_write_private(&dir.join(STATE_FILE), text.as_bytes()).map_err(io)?;
    write_views(dir, state, view_intro)
}

/// One readable Markdown file per area, under `<dir>/areas/`. A view only: nothing reads it back.
pub fn write_views(dir: &Path, state: &ProfileState, view_intro: &str) -> Result<(), StoreError> {
    let areas = dir.join("areas");
    rightkit_fs::ensure_private_dir(&areas).map_err(io)?;
    for area in Area::ALL {
        let mut text = format!("# {}\n\n{}\n", area.slug(), view_intro);
        for entry in state.entries.values().filter(|e| e.area == area) {
            text.push_str(&format!("\n## {}\n\n{}\n\n", entry.key, entry.value));
            text.push_str(&format!(
                "- state: {:?}, source: {:?}\n",
                entry.state, entry.source
            ));
            if let Some(from) = &entry.true_from {
                text.push_str(&format!("- true from: {from}\n"));
            }
            if let Some(due) = &entry.review_by {
                text.push_str(&format!("- review by: {due}\n"));
            }
            for old in entry.history.iter().rev() {
                text.push_str(&format!("- earlier: {}\n", old.value.replace('\n', " ")));
            }
        }
        rightkit_fs::atomic_write_private(
            &areas.join(format!("{}.md", area.slug())),
            text.as_bytes(),
        )
        .map_err(io)?;
    }
    Ok(())
}
