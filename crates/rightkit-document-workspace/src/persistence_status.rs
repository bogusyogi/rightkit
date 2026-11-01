//! How the opening of the workspace database recovered, as the renderer reads it.

use rightkit_db::Degraded;
use serde::Serialize;

/// What the open of the local database had to do (`Degraded`, kept as the `Option<Degraded>`
/// returned by [`crate::store::Db::open_path`]). `None` is a normal open.
///
/// `degraded` is true whenever recovery acted at all. The only recovery allowed is quarantine: the
/// unreadable file is kept, its bytes and sidecars intact, at `quarantined_to`, and a fresh
/// persistent database replaced it, so recents, pins and the session restored from the old file
/// are gone from the UI but not from disk. There is no in-memory fallback.
#[derive(Debug, Serialize)]
pub struct PersistenceStatus {
    /// True whenever recovery acted.
    pub degraded: bool,
    /// Where the unreadable database was moved. Present whenever `degraded` is true.
    pub quarantined_to: Option<String>,
}

/// The status for an open outcome.
pub fn status_of(degraded: Option<&Degraded>) -> PersistenceStatus {
    PersistenceStatus {
        degraded: degraded.is_some(),
        quarantined_to: degraded
            .and_then(|d| d.quarantined.as_ref())
            .map(|path| path.to_string_lossy().into_owned()),
    }
}
