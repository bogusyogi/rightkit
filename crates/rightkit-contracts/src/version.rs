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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_strict_triplets() {
        assert_eq!(Version::parse("1.2.3").unwrap(), Version::new(1, 2, 3));
        assert_eq!(Version::parse("0.0.0").unwrap(), Version::new(0, 0, 0));
    }

    #[test]
    fn rejects_malformed_versions() {
        for text in [
            "1.2",
            "1.2.3.4",
            "1..3",
            "v1.2.3",
            "1.02.3",
            "1.2.-3",
            "",
            "1.2.3-rc1",
            " 1.2.3",
        ] {
            assert!(
                matches!(
                    Version::parse(text),
                    Err(ContractError::InvalidVersion { .. })
                ),
                "accepted malformed version {text:?}"
            );
        }
    }

    #[test]
    fn same_major_older_or_equal_minor_is_accepted() {
        assert_eq!(
            negotiate(Contract::ContentBrief, "1.0.0").unwrap(),
            Version::new(1, 0, 0)
        );
        assert_eq!(
            negotiate(Contract::ContentBrief, "1.0.9").unwrap(),
            Version::new(1, 0, 9)
        );
    }

    #[test]
    fn incompatible_major_is_refused() {
        let error = negotiate(Contract::RecordEvent, "2.0.0").unwrap_err();
        assert!(matches!(error, ContractError::IncompatibleMajor { .. }));
        assert!(error.is_version_refusal());
        let error = negotiate(Contract::RecordEvent, "0.9.0").unwrap_err();
        assert!(matches!(error, ContractError::IncompatibleMajor { .. }));
    }

    #[test]
    fn newer_minor_is_refused() {
        let error = negotiate(Contract::DemoPlan, "1.1.0").unwrap_err();
        assert!(matches!(error, ContractError::NewerMinor { .. }));
    }
}
