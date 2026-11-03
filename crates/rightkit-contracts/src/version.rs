use std::fmt;

use crate::{Contract, ContractError};

/// A `major.minor.patch` contract version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Version {
            major,
            minor,
            patch,
        }
    }

    /// Parses strict `major.minor.patch` with decimal numeric parts.
    pub fn parse(text: &str) -> Result<Version, ContractError> {
        let invalid = || ContractError::InvalidVersion {
            text: text.to_string(),
        };
        let mut parts = text.split('.');
        let mut next = || -> Result<u64, ContractError> {
            let part = parts.next().ok_or_else(invalid)?;
            if part.is_empty()
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
            {
                return Err(invalid());
            }
            part.parse::<u64>().map_err(|_| invalid())
        };
        let version = Version::new(next()?, next()?, next()?);
        if parts.next().is_some() {
            return Err(invalid());
        }
        Ok(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Decides whether a document offering `offered` can be read by this build.
///
/// Accepts when the majors match and the offered minor is not newer than the
/// supported minor. Refuses a different major, and refuses a newer minor
/// because fields this build does not know could be silently dropped.
/// Returns the offered version on success.
pub fn negotiate(contract: Contract, offered: &str) -> Result<Version, ContractError> {
    let offered = Version::parse(offered)?;
    let supported = contract.supported_version();
    if offered.major != supported.major {
        return Err(ContractError::IncompatibleMajor {
            contract,
            offered,
            supported,
        });
    }
    if offered.minor > supported.minor {
        return Err(ContractError::NewerMinor {
            contract,
            offered,
            supported,
        });
    }
    Ok(offered)
}
