//! Schema-2 intake; callers must retain recovery state until roots verify and completion succeeds.
//! Transport is shared with schema 1; owner-side signed-runner admission remains authoritative.
use super::*;
use std::io::Read;

pub(in crate::snapshot) const SCHEMA: &str = "rightkit.qa.credential-owner/2";
const MANIFEST_SCHEMA: &str = "rightkit.qa.credential-snapshot/2";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::snapshot) enum Operation {
    Snapshot,
    Restore,
    Complete,
}

impl Operation {
    fn name(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Restore => "restore",
            Self::Complete => "complete",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::snapshot) enum CleanupCode {
    CleanupOk,
    CleanupIncomplete,
    CleanupUnknown,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::snapshot) struct Cleanup {
    pub(in crate::snapshot) code: CleanupCode,
    pub(in crate::snapshot) items_not_removed: usize,
    pub(in crate::snapshot) generations_pending: usize,
}

impl Cleanup {
    pub(in crate::snapshot) fn is_clean(&self) -> bool {
        matches!(self.code, CleanupCode::CleanupOk)
            && self.items_not_removed == 0
            && self.generations_pending == 0
    }

    fn valid(&self) -> bool {
        match self.code {
            CleanupCode::CleanupOk => self.is_clean(),
            CleanupCode::CleanupIncomplete => {
                self.items_not_removed > 0 || self.generations_pending > 0
            }
            CleanupCode::CleanupUnknown => {
                self.items_not_removed == 0 && self.generations_pending == 0
            }
        }
    }
}

/// Success of an order, completion receipt, and stale-store cleanup are separate facts.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::snapshot) struct Metadata {
    schema: String,
    operation: String,
    snapshot_id: String,
    pub(in crate::snapshot) item_count: usize,
    pub(in crate::snapshot) credentials_sha256: String,
    verified: bool,
    pub(in crate::snapshot) already_completed: bool,
    pub(in crate::snapshot) cleanup: Cleanup,
}

pub(in crate::snapshot) enum Completion {
    Accepted(Metadata),
    RestoreIncomplete,
}

fn valid_sha256(sha: &str) -> bool {
    sha.len() == 64
        && sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Verify the recorded executable and private transport binding without launching the owner.
/// A live exchange separately proves the installed owner's process identity.
pub(in crate::snapshot) fn verify_owner(marker: &Marker) -> Result<()> {
    let owner = marker
        .credential_owner
        .as_ref()
        .ok_or_else(|| Error("schema-2 credential owner binding is missing".into()))?;
    owner.verify()?;
    verify_location(marker)
}

/// Never falls back to schema 1 or in-process credential access on refusal.
/// Complete is a separate launch, ordered only after coordinator verifies restored roots.
pub(in crate::snapshot) fn operate(marker: &Marker, operation: Operation) -> Result<Metadata> {
    match owner_request(marker, operation)? {
        Completion::Accepted(metadata) => Ok(metadata),
        Completion::RestoreIncomplete => {
            err("credential owner requires a verified restore before completion")
        }
    }
}

pub(in crate::snapshot) fn complete(marker: &Marker) -> Result<Completion> {
    owner_request(marker, Operation::Complete)
}

fn owner_request(marker: &Marker, operation: Operation) -> Result<Completion> {
    if operation != Operation::Snapshot
        && !marker
            .credentials_sha256
            .as_deref()
            .is_some_and(valid_sha256)
    {
        return err("schema-2 credential operation lacks a manifest digest; marker retained");
    }
    operate_with(marker, operation.name(), SCHEMA, |response| {
        let OwnerReply::Metadata(response) = response else {
            return Ok(Completion::RestoreIncomplete);
        };
        let metadata: Metadata = serde_json::from_value(response)
            .map_err(|_| Error("credential owner returned invalid schema-2 metadata".into()))?;
        if metadata.schema != SCHEMA
            || metadata.operation != operation.name()
            || metadata.snapshot_id != marker.snapshot_id
            || !metadata.verified
            || metadata.item_count
                != marker
                    .credentials
                    .iter()
                    .map(|c| c.accounts.len())
                    .sum::<usize>()
            || !valid_sha256(&metadata.credentials_sha256)
            || !metadata.cleanup.valid()
            || (operation == Operation::Snapshot && metadata.already_completed)
            || (operation != Operation::Snapshot
                && marker.credentials_sha256.as_deref()
                    != Some(metadata.credentials_sha256.as_str()))
        {
            return err(
                "credential owner metadata does not prove this schema-2 operation; marker retained",
            );
        }
        Ok(Completion::Accepted(metadata))
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::snapshot) struct ManifestItem {
    set: usize,
    account: String,
    pub(in crate::snapshot) present: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::snapshot) struct Manifest {
    schema: String,
    app: String,
    snapshot_id: String,
    pub(in crate::snapshot) items: Vec<ManifestItem>,
}

/// Read metadata only, through one bounded no-follow handle. No CrewFile/plaintext parser.
pub(in crate::snapshot) fn load_manifest(marker: &Marker, expected_sha: &str) -> Result<Manifest> {
    verify_location(marker)?;
    if !valid_sha256(expected_sha) {
        return err("schema-2 credential manifest digest is invalid; marker retained");
    }
    let path = Path::new(&marker.snapshot_dir).join(CREDENTIALS_FILE);
    let options = rightkit_fs::ReadOptions::new()
        .require_single_link(true)
        .share_read_only(true);
    let mut file = rightkit_fs::open_read_nofollow(&path, &options)
        .map_err(|_| Error("cannot open schema-2 credential manifest; marker retained".into()))?;
    fs_ctx(
        rightkit_fs::private::verify_owner_only(&path),
        "credential manifest is not private",
        &path,
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let held = ctx(
            file.metadata(),
            "cannot stat held credential manifest",
            &path,
        )?;
        let named = ctx(
            fs::symlink_metadata(&path),
            "cannot stat credential manifest",
            &path,
        )?;
        if held.dev() != named.dev() || held.ino() != named.ino() {
            return err("schema-2 credential manifest identity changed; marker retained");
        }
    }
    let mut bytes = Vec::new();
    ctx(
        (&mut file)
            .take(MAX_MANIFEST_BYTES + 1)
            .read_to_end(&mut bytes),
        "cannot read credential manifest",
        &path,
    )?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES || sha256_hex(&bytes) != expected_sha {
        return err("schema-2 credential manifest size/digest mismatch; marker retained");
    }
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .map_err(|_| Error("invalid metadata-only credential manifest; marker retained".into()))?;
    let expected: Vec<_> = marker
        .credentials
        .iter()
        .enumerate()
        .flat_map(|(set, record)| record.accounts.iter().map(move |account| (set, account)))
        .collect();
    if manifest.schema != MANIFEST_SCHEMA
        || manifest.app != marker.app
        || manifest.snapshot_id != marker.snapshot_id
        || manifest.items.len() != expected.len()
        || manifest
            .items
            .iter()
            .zip(expected)
            .any(|(item, (set, account))| item.set != set || &item.account != account)
    {
        return err("schema-2 credential manifest does not name declared items; marker retained");
    }
    Ok(manifest)
}
