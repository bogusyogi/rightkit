use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::EffectError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigestState {
    Unchecked,
    Sealed,
    Mismatch,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignatureState {
    Unknown,
    Valid,
    Invalid,
    NotApplicable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionProbeEvidence {
    pub args: Vec<String>,
    pub output: Option<String>,
    pub exit_code: Option<i32>,
    pub qualified: bool,
    /// True only when the caller opted out of the probe because the
    /// executable is pinned by digest. `qualified` stays false: no version
    /// was observed.
    pub skipped_by_policy: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutableIdentity {
    pub requested_path: String,
    pub canonical_path: Option<PathBuf>,
    pub digest: Option<String>,
    pub digest_state: DigestState,
    pub signature: SignatureState,
    pub version: VersionProbeEvidence,
}

impl ExecutableIdentity {
    pub fn resolve(path: &str, expected_digest: Option<&str>) -> Result<Self, EffectError> {
        if !Path::new(path).is_absolute() {
            return Err(EffectError::UnsealedExecutable(path.into()));
        }
        let canonical = fs::canonicalize(path)
            .map_err(|error| EffectError::MissingExecutable(error.to_string()))?;
        let bytes = fs::read(&canonical)
            .map_err(|error| EffectError::MissingExecutable(error.to_string()))?;
        let digest = format!("sha256:{}", hex_digest(&bytes));
        let state = match expected_digest {
            Some(expected) if expected != digest => DigestState::Mismatch,
            Some(_) => DigestState::Sealed,
            None => DigestState::Unchecked,
        };
        if state == DigestState::Mismatch {
            return Err(EffectError::UnsealedExecutable(
                "executable digest mismatch".into(),
            ));
        }
        Ok(Self {
            requested_path: path.into(),
            canonical_path: Some(canonical),
            digest: Some(digest),
            digest_state: state,
            signature: SignatureState::Unknown,
            version: VersionProbeEvidence {
                args: Vec::new(),
                output: None,
                exit_code: None,
                qualified: false,
                skipped_by_policy: false,
            },
        })
    }

    pub fn with_version_probe(mut self, probe: VersionProbeEvidence) -> Self {
        self.version = probe;
        self
    }
    pub fn sealed(&self) -> bool {
        self.digest.is_some() && matches!(self.digest_state, DigestState::Sealed)
    }

    pub fn version_qualified(&self, requirement: Option<&str>) -> bool {
        self.version.qualified
            && requirement.is_none_or(|required| {
                self.version.output.as_deref().is_some_and(|output| {
                    if let Some(exact) = required.strip_prefix("exact:") {
                        let versions = output
                            .split(|character: char| {
                                !(character.is_ascii_alphanumeric()
                                    || matches!(character, '.' | '-' | '+' | '_'))
                            })
                            .filter(|token| {
                                let version = token.strip_prefix('v').unwrap_or(token);
                                version.contains('.')
                                    && version.as_bytes().first().is_some_and(u8::is_ascii_digit)
                            })
                            .collect::<Vec<_>>();
                        !exact.is_empty()
                            && versions.len() == 1
                            && (versions[0] == exact
                                || versions[0].strip_prefix('v') == Some(exact))
                    } else if let Some(tool_version) = required.strip_prefix("exact-tool:") {
                        tool_version
                            .strip_prefix("osv-scanner:")
                            .is_some_and(|exact| osv_scanner_version_matches(output, exact))
                    } else {
                        output.contains(required)
                    }
                })
            })
    }
}

// OSV's primary version is distinct from its embedded SCALIBR version. Keep
// this opt-in grammar closed: every line must match the known ordered banner.
fn osv_scanner_version_matches(output: &str, exact: &str) -> bool {
    if !stable_version(exact) {
        return false;
    }
    let mut lines = output.lines();
    let primary = lines
        .next()
        .and_then(|line| line.strip_prefix("osv-scanner version: "));
    let scalibr = lines
        .next()
        .and_then(|line| line.strip_prefix("osv-scalibr version: "));
    let commit = lines.next().and_then(|line| line.strip_prefix("commit: "));
    let built = lines
        .next()
        .and_then(|line| line.strip_prefix("built at: "));
    primary == Some(exact)
        && scalibr.is_some_and(stable_version)
        && commit.is_some_and(|commit| {
            commit.len() == 40
                && commit
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        && built.is_some_and(utc_build_timestamp)
        && lines.next().is_none()
}

fn stable_version(version: &str) -> bool {
    let parts = version.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
}

fn utc_build_timestamp(timestamp: &str) -> bool {
    let bytes = timestamp.as_bytes();
    if bytes.len() != 20
        || bytes.iter().enumerate().any(|(index, byte)| match index {
            4 | 7 => *byte != b'-',
            10 => *byte != b'T',
            13 | 16 => *byte != b':',
            19 => *byte != b'Z',
            _ => !byte.is_ascii_digit(),
        })
    {
        return false;
    }
    // ASCII was checked before indexing, so every slice is a character boundary.
    let number = |start, end| timestamp[start..end].parse::<u32>().unwrap();
    let year = number(0, 4);
    let month = number(5, 7);
    let day = number(8, 10);
    let days = match month {
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return false,
    };
    year > 0
        && (1..=days).contains(&day)
        && number(11, 13) < 24
        && number(14, 16) < 60
        && number(17, 19) < 60
}

fn hex_digest(bytes: &[u8]) -> String {
    hex_encode(&Sha256::digest(bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0xf) as usize] as char);
    }
    output
}
