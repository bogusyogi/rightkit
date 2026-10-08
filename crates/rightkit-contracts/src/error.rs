use std::fmt;

use crate::{Contract, Version};

/// Why a document was refused. Variants are ordered by the stage that raises
/// them: name and version first, then schema, then semantics, then typing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    /// The document's `contract` field names a different contract.
    WrongContract { expected: Contract, found: String },
    /// The document has no `contractVersion` string.
    MissingVersion { contract: Contract },
    /// The document's major version differs from the supported major.
    IncompatibleMajor {
        contract: Contract,
        offered: Version,
        supported: Version,
    },
    /// The document's minor version is newer than this build supports.
    NewerMinor {
        contract: Contract,
        offered: Version,
        supported: Version,
    },
    /// `contractVersion` is not a `major.minor.patch` string.
    InvalidVersion { text: String },
    /// JSON Schema validation failed. Each entry is `path: message`.
    Schema {
        contract: Contract,
        errors: Vec<String>,
    },
    /// The schema passed but a cross-field rule failed.
    Semantic { contract: Contract, rule: String },
    /// The text is not JSON, or does not fit the typed struct.
    Parse { message: String },
}

impl ContractError {
    /// True for refusals that happen before any document content is read
    /// beyond its name and version.
    pub fn is_version_refusal(&self) -> bool {
        matches!(
            self,
            ContractError::MissingVersion { .. }
                | ContractError::IncompatibleMajor { .. }
                | ContractError::NewerMinor { .. }
                | ContractError::InvalidVersion { .. }
        )
    }
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContractError::WrongContract { expected, found } => {
                write!(f, "contract: expected `{}`, found `{}`", expected.name(), found)
            }
            ContractError::MissingVersion { contract } => {
                write!(f, "{}: missing contractVersion", contract.name())
            }
            ContractError::IncompatibleMajor { contract, offered, supported } => write!(
                f,
                "{}: incompatible major version {} (this build supports {}); refused before any work",
                contract.name(),
                offered,
                supported
            ),
            ContractError::NewerMinor { contract, offered, supported } => write!(
                f,
                "{}: version {} is newer than this build supports ({}); upgrade rightkit-contracts first",
                contract.name(),
                offered,
                supported
            ),
            ContractError::InvalidVersion { text } => write!(f, "invalid contractVersion `{text}`"),
            ContractError::Schema { contract, errors } => {
                write!(f, "{}: schema validation failed", contract.name())?;
                for error in errors {
                    write!(f, "\n  - {error}")?;
                }
                Ok(())
            }
            ContractError::Semantic { contract, rule } => {
                write!(f, "{}: semantic rule failed: {}", contract.name(), rule)
            }
            ContractError::Parse { message } => write!(f, "parse error: {message}"),
        }
    }
}

impl std::error::Error for ContractError {}
