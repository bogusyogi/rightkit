//! Declarative model manifest: a model has ordered revisions, each optionally
//! a quantization variant, each a list of (platform-filtered) files.

use crate::hardware::{Measurement, Requirements};
use crate::platform::Platform;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelSpec {
    pub id: String,
    #[serde(default)]
    pub label: String,
    /// Oldest first; "latest" means the last matching entry.
    pub revisions: Vec<RevisionSpec>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileSpec {
    /// Destination path, forward slashes: inside the revision directory, or,
    /// when [`FileSpec::shared`] is set, inside that shared-runtime root.
    /// Always a relative path with no `.`/`..`/empty components.
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
    /// Shared-runtime kind (e.g. `onnxruntime`): the file lives at
    /// `<shared root for kind>/<path>` instead of inside the revision, where
    /// several models (and apps) can load it. The root is configured by the
    /// app with [`crate::Store::with_shared_root`]; an unconfigured kind is
    /// rejected. Shared files must be hash- and size-pinned, are never
    /// overwritten when present with other bytes (`Conflict`), and are left in
    /// place by uninstall. `extract: TarGz` is not allowed for them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<String>,
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
        self.checked_key()
            .map(|k| k.as_str().to_string())
            .unwrap_or_default()
    }

    pub fn files_for<'a>(&'a self, platform: &'a Platform) -> impl Iterator<Item = &'a FileSpec> {
        self.files
            .iter()
            .filter(move |f| platform.allows(&f.platforms))
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
        self.revisions
            .iter()
            .find(|r| !key.is_empty() && r.key() == key)
    }
}

/// Validate a shared-runtime kind: one safe path component, same alphabet as
/// a model id (`[A-Za-z0-9._-]`, no leading/trailing `.`).
pub fn parse_shared_kind(kind: &str) -> crate::error::Result<String> {
    crate::ids::ModelId::parse(kind)
        .map(|k| k.as_str().to_string())
        .map_err(|_| {
            crate::error::Error::new(
                crate::error::ErrorCode::InvalidSpec,
                format!("invalid shared-runtime kind {kind:?}"),
            )
        })
}

pub(crate) fn safe_rel_path(path: &str) -> Option<std::path::PathBuf> {
    let mut out = std::path::PathBuf::new();
    for part in path.split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.contains('\\')
            || part.contains(':')
        {
            return None;
        }
        out.push(part);
    }
    Some(out)
}
