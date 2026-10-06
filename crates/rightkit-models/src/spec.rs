//! Declarative model manifest: a model has ordered revisions, each optionally
//! a quantization variant, each a list of (platform-filtered) files.

use crate::hardware::{Measurement, Requirements};
use crate::platform::Platform;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSpec {
    pub id: String,
    #[serde(default)]
    pub label: String,
    /// Oldest first; "latest" means the last matching entry.
    pub revisions: Vec<RevisionSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionSpec {
    pub revision: String,
    /// Quantization label (`q4_k_m`, `int8`, `fp16`, ...). `None` = unquantized.
    #[serde(default)]
    pub quant: Option<String>,
    pub files: Vec<FileSpec>,
    #[serde(default)]
    pub requirements: Requirements,
    #[serde(default)]
    pub measured: Vec<Measurement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Extract {
    /// The download is a tar(.gz) or zip archive; unpack this one member to
    /// `FileSpec::path`.
    Member(String),
    /// The download is a tar.gz with one wrapper directory; unpack its
    /// contents (wrapper stripped) into the directory `FileSpec::path`.
    TarGz,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSpec {
    /// Destination path inside the revision directory, forward slashes.
    pub path: String,
    pub url: String,
    /// Size of the downloaded artifact.
    #[serde(default)]
    pub bytes: Option<u64>,
    /// sha256 of the downloaded artifact.
    #[serde(default)]
    pub sha256: Option<String>,
    /// Platform filters (see [`Platform`]); empty = every platform.
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(default)]
    pub extract: Option<Extract>,
    /// Pins for the *installed* payload when `extract` is `Member`.
    #[serde(default)]
    pub installed_bytes: Option<u64>,
    #[serde(default)]
    pub installed_sha256: Option<String>,
}

impl RevisionSpec {
    /// Directory-safe key unique per (revision, quant): an injective encoding
    /// (see [`crate::ids::encode_revision_key`]). Fails for an empty or
    /// over-long revision/quant.
    pub fn checked_key(&self) -> crate::error::Result<crate::ids::RevisionKey> {
        crate::ids::encode_revision_key(&self.revision, self.quant.as_deref())
    }

    /// String form of [`Self::checked_key`]; an unencodable revision yields an
    /// empty string, which every store operation rejects.
    pub fn key(&self) -> String {
        self.checked_key().map(|k| k.as_str().to_string()).unwrap_or_default()
    }

    pub fn files_for<'a>(&'a self, platform: &'a Platform) -> impl Iterator<Item = &'a FileSpec> {
        self.files.iter().filter(move |f| platform.allows(&f.platforms))
    }

    /// Bytes that must be downloaded plus unpacked for `platform`.
    pub fn required_disk_bytes(&self, platform: &Platform) -> u64 {
        self.files_for(platform)
            .map(|f| f.bytes.unwrap_or(0) + f.installed_bytes.unwrap_or(0))
            .sum()
    }
}

impl ModelSpec {
    /// Pick a revision. `revision = None` means newest; `quant = None` means
    /// any quantization (newest match).
    pub fn select(&self, revision: Option<&str>, quant: Option<&str>) -> Option<&RevisionSpec> {
        self.revisions.iter().rev().find(|r| {
            revision.is_none_or(|v| r.revision == v)
                && quant.is_none_or(|q| r.quant.as_deref() == Some(q))
        })
    }

    pub fn by_key(&self, key: &str) -> Option<&RevisionSpec> {
        self.revisions.iter().find(|r| !key.is_empty() && r.key() == key)
    }
}

pub(crate) fn safe_rel_path(path: &str) -> Option<std::path::PathBuf> {
    let mut out = std::path::PathBuf::new();
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains('\\') || part.contains(':') {
            return None;
        }
        out.push(part);
    }
    Some(out)
}
