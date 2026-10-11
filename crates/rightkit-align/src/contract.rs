//! The `rightkit.alignment` v1 document for one alignment.
//!
//! The document uses the same keys as the sibling contracts in
//! `rightkit/contracts/`: `contract` names the contract and `contractVersion`
//! its version. The JSON Schema is `rightkit/contracts/alignment.v1.schema.json`.
//! Build a value with [`crate::Alignment::to_contract`].
//!
//! CutRight's proposed `schema: "rightkit.alignment.v1"` maps onto these two
//! keys: `contract: "rightkit.alignment"` plus `contractVersion: "1.0.0"`.
//! Reading any other contract name or any other major or minor version is an
//! error, so a consumer cannot mistake a future shape for this one.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{Anchor, Clause, Coverage};

/// The contract name written to and required from every document.
pub const CONTRACT: &str = "rightkit.alignment";
/// The contract version this build writes. It reads only major 1, minor 0.
pub const CONTRACT_VERSION: &str = "1.0.0";

/// Which engine path produced the alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    /// Local alignment of a script against ASR words (RK4, this crate).
    Local,
}

/// Serialises as [`CONTRACT`] and accepts only that string on read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContractTag;

impl Serialize for ContractTag {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(CONTRACT)
    }
}

impl<'de> Deserialize<'de> for ContractTag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let found = String::deserialize(deserializer)?;
        if found == CONTRACT {
            Ok(ContractTag)
        } else {
            Err(serde::de::Error::custom(format!(
                "unsupported contract {found:?}, expected {CONTRACT:?}"
            )))
        }
    }
}

/// Serialises as [`CONTRACT_VERSION`] and accepts only major 1, minor 0 (any patch) on read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionTag;

impl Serialize for VersionTag {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(CONTRACT_VERSION)
    }
}

impl<'de> Deserialize<'de> for VersionTag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let found = String::deserialize(deserializer)?;
        let mut parts = found.split('.');
        let supported = matches!(
            (parts.next(), parts.next(), parts.next(), parts.next()),
            (Some("1"), Some("0"), Some(patch), None)
                if !patch.is_empty() && patch.bytes().all(|b| b.is_ascii_digit())
        );
        if supported {
            Ok(VersionTag)
        } else {
            Err(serde::de::Error::custom(format!(
                "unsupported contractVersion {found:?}, this build reads 1.0.x"
            )))
        }
    }
}

/// The `rightkit.alignment` v1 document. Field names are the wire names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlignmentContract {
    pub contract: ContractTag,
    #[serde(rename = "contractVersion")]
    pub contract_version: VersionTag,
    pub route: Route,
    /// One row per script token, in script order.
    pub tokens: Vec<Anchor>,
    pub clauses: Vec<Clause>,
    pub coverage: Coverage,
    pub timing_tolerance_ms: u64,
}
