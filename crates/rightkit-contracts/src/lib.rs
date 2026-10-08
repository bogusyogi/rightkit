//! rightkit-contracts: the versioned contracts shared by the content pipeline
//! (CutRight, GenRight, PostRight), the recorder, and the daemon.
//!
//! The JSON Schema files live in `tools/rightkit/contracts/` and are embedded
//! here at compile time, so every installed engine validates against the same
//! bytes. The pipeline is:
//!
//! 1. [`negotiate`] reads the document's `contract` and `contractVersion` and
//!    rejects an incompatible major version, or a minor newer than this build
//!    knows, before any schema or semantic work starts.
//! 2. The document is validated against the embedded Draft 2020-12 schema.
//! 3. Semantic rules that JSON Schema cannot express (unique ids, resolvable
//!    references, contiguous time maps) run in [`semantic`].
//! 4. [`parse`] additionally deserialises into the typed structs in [`types`].
//!
//! Version policy: a document with major `M` and minor `m` is accepted by a
//! build that supports `M.n` when `m <= n`. Patch numbers never gate
//! acceptance. A different major is rejected, and so is a newer minor, because
//! this build cannot promise to understand fields it does not know.

#![forbid(unsafe_code)]

mod error;
pub mod fixtures;
mod schema;
mod semantic;
pub mod types;
mod version;

use serde::de::DeserializeOwned;
use serde_json::Value;

pub use error::ContractError;
pub use version::{negotiate, Version};

/// The six contracts defined in `tools/rightkit/contracts/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Contract {
    ContentBrief,
    DeliveryEnvelope,
    RecordSession,
    RecordEvent,
    DemoPlan,
    DaemonApi,
}

impl Contract {
    /// Every contract, in a fixed order.
    pub const ALL: [Contract; 6] = [
        Contract::ContentBrief,
        Contract::DeliveryEnvelope,
        Contract::RecordSession,
        Contract::RecordEvent,
        Contract::DemoPlan,
        Contract::DaemonApi,
    ];

    /// The name used in documents' `contract` field and on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Contract::ContentBrief => "content-brief",
            Contract::DeliveryEnvelope => "delivery-envelope",
            Contract::RecordSession => "record-session",
            Contract::RecordEvent => "record-event",
            Contract::DemoPlan => "demo-plan",
            Contract::DaemonApi => "daemon-api",
        }
    }

    /// Looks a contract up by its document name, such as `content-brief`.
    pub fn from_name(name: &str) -> Option<Contract> {
        Contract::ALL
            .into_iter()
            .find(|contract| contract.name() == name)
    }

    /// The highest version of this contract that this build understands.
    pub fn supported_version(self) -> Version {
        Version::new(1, 0, 0)
    }

    /// The `$id` declared inside the schema file.
    pub fn schema_id(self) -> &'static str {
        match self {
            Contract::ContentBrief => "urn:rightkit:contract:content-brief:v1",
            Contract::DeliveryEnvelope => "urn:rightkit:contract:delivery-envelope:v1",
            Contract::RecordSession => "urn:rightkit:contract:record-session:v1",
            Contract::RecordEvent => "urn:rightkit:contract:record-event:v1",
            Contract::DemoPlan => "urn:rightkit:contract:demo-plan:v1",
            Contract::DaemonApi => "urn:rightkit:contract:daemon-api:v1",
        }
    }

    /// The embedded schema source text.
    pub fn schema_source(self) -> &'static str {
        match self {
            Contract::ContentBrief => {
                include_str!("../../../contracts/content-brief.v1.schema.json")
            }
            Contract::DeliveryEnvelope => {
                include_str!("../../../contracts/delivery-envelope.v1.schema.json")
            }
            Contract::RecordSession => {
                include_str!("../../../contracts/record-session.v1.schema.json")
            }
            Contract::RecordEvent => include_str!("../../../contracts/record-event.v1.schema.json"),
            Contract::DemoPlan => include_str!("../../../contracts/demo-plan.v1.schema.json"),
            Contract::DaemonApi => include_str!("../../../contracts/daemon-api.v1.schema.json"),
        }
    }

    pub(crate) fn index(self) -> usize {
        Contract::ALL
            .iter()
            .position(|contract| *contract == self)
            .unwrap_or(0)
    }
}

/// Validates a parsed document: contract name, version negotiation, JSON
/// Schema, then semantic rules. Negotiation runs first, so an incompatible
/// major version never reaches schema validation.
pub fn validate(contract: Contract, document: &Value) -> Result<(), ContractError> {
    check_contract_name(contract, document)?;
    let offered = document
        .get("contractVersion")
        .and_then(Value::as_str)
        .ok_or(ContractError::MissingVersion { contract })?;
    negotiate(contract, offered)?;
    schema::check(contract, document)?;
    semantic::check(contract, document)
}

/// Parses JSON text and validates it with [`validate`].
pub fn validate_str(contract: Contract, text: &str) -> Result<Value, ContractError> {
    let document: Value = serde_json::from_str(text).map_err(|source| ContractError::Parse {
        message: source.to_string(),
    })?;
    validate(contract, &document)?;
    Ok(document)
}

/// Validates the text and deserialises it into the contract's typed struct.
pub fn parse<T: DeserializeOwned>(contract: Contract, text: &str) -> Result<T, ContractError> {
    let document = validate_str(contract, text)?;
    serde_json::from_value(document).map_err(|source| ContractError::Parse {
        message: source.to_string(),
    })
}

/// Validates the text, then deserialises it into the typed struct for `contract`.
/// The typed view is checked as well as the schema, so a fixture that validates
/// but cannot be read by the Rust types fails the suite.
pub fn check_typed(contract: Contract, text: &str) -> Result<(), ContractError> {
    match contract {
        Contract::ContentBrief => parse::<types::ContentBrief>(contract, text).map(drop),
        Contract::DeliveryEnvelope => parse::<types::DeliveryEnvelope>(contract, text).map(drop),
        Contract::RecordSession => parse::<types::RecordSession>(contract, text).map(drop),
        Contract::RecordEvent => parse::<types::RecordEvent>(contract, text).map(drop),
        Contract::DemoPlan => parse::<types::DemoPlan>(contract, text).map(drop),
        Contract::DaemonApi => parse::<types::DaemonApi>(contract, text).map(drop),
    }
}

fn check_contract_name(contract: Contract, document: &Value) -> Result<(), ContractError> {
    let found = document
        .get("contract")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if found == contract.name() {
        Ok(())
    } else {
        Err(ContractError::WrongContract {
            expected: contract,
            found,
        })
    }
}
