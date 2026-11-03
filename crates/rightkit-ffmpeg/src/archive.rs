//! [`ArchiveRef`]: where a catalog row's retained bytes can be fetched. Shared by the FFmpeg catalog
//! ([`crate::Artifact`]) and the native-library catalog (`native`, feature `native`), so it is compiled
//! without that feature too.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArchiveRef {
    pub url: String,
    pub sha256: String,
    /// Path inside the archive of the shipped file (for example `bin/x64-win/DirectML.dll`).
    /// Needed where the archive holds several files with the same base name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
}
