//! Signing evidence of a gate-signed QA run (rightkit#1303, rightkit#1304).
//!
//! The `sign` suite step writes a receipt and exports its path as `RIGHTKIT_QA_SIGNING_EVIDENCE`
//! (and the signed main binary as `RIGHTKIT_QA_APP_BINARY`). The receipt is schema 1, written by
//! `packages/release/suite-signing.mjs` (the file near :211, the per-file records at :102 for mac
//! and :121 for Windows):
//!
//! `{ "schema": 1, "step": "..", "files": [{ "file", "signed", "sha256Before", "sha256After", .. }] }`
//!
//! `signed` is the absolute path of the signed copy and `sha256After` its lowercase hex SHA-256.
//! A QA run that launches a file must call [`Evidence::require`]: exactly one record must name the
//! signed copy, and the file actually launched must hash to that record's `sha256After`. Anything
//! else (no receipt, a malformed one, no record, several records, a mismatch) is an [`Error`].
//!
//! Only `schema`, `files`, and each record's `file`, `signed` and `sha256After` are required, and read
//! strictly; `sha256Before` is read when present (a malformed one is refused) and every other field (the platform fields) is ignored, so the
//! signer can add fields without breaking a consumer. An error names the receipt or file path and
//! quotes no other receipt content than a digest: a hash mismatch prints the receipt's `sha256After` beside the file's own digest.

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The environment variable the `sign` step sets to the receipt's path.
pub const EVIDENCE_ENV: &str = "RIGHTKIT_QA_SIGNING_EVIDENCE";

/// The environment variable the `sign` step sets to the signed main binary.
pub const APP_BINARY_ENV: &str = "RIGHTKIT_QA_APP_BINARY";

/// Why signing evidence was refused.
#[derive(Debug)]
pub enum Error {
    /// `RIGHTKIT_QA_SIGNING_EVIDENCE` is unset or empty.
    MissingEnv,
    /// The receipt file could not be read.
    Unreadable { path: PathBuf, source: io::Error },
    /// The receipt is not JSON.
    InvalidJson {
        receipt: Option<PathBuf>,
        detail: String,
    },
    /// `schema` is absent or is not the number 1.
    UnsupportedSchema { receipt: Option<PathBuf> },
    /// `files` is absent, not an array, or empty.
    EmptyFiles { receipt: Option<PathBuf> },
    /// `files[index]` has no string `field` (`signed` or `sha256After`).
    MissingField {
        receipt: Option<PathBuf>,
        index: usize,
        field: &'static str,
    },
    /// `files[index].sha256After` is not 64 lowercase hex digits.
    BadSha {
        receipt: Option<PathBuf>,
        index: usize,
    },
    /// No record names `signed`.
    NoRecord {
        receipt: Option<PathBuf>,
        signed: PathBuf,
    },
    /// More than one record names `signed`.
    SeveralRecords {
        receipt: Option<PathBuf>,
        signed: PathBuf,
        count: usize,
    },
    /// The launched file could not be hashed.
    Unhashable { path: PathBuf, source: io::Error },
    /// The launched file does not hash to the signed record.
    HashMismatch {
        actual: PathBuf,
        signed: PathBuf,
        actual_sha256: String,
        signed_sha256: String,
    },
}

fn receipt_name(receipt: &Option<PathBuf>) -> String {
    match receipt {
        Some(path) => format!("signing evidence {}", path.display()),
        None => "signing evidence".to_owned(),
    }
}

impl Error {
    /// A stable name for the refusal kind, also the prefix of the message.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::MissingEnv => "missing-env",
            Error::Unreadable { .. } => "unreadable",
            Error::InvalidJson { .. } => "invalid-json",
            Error::UnsupportedSchema { .. } => "unsupported-schema",
            Error::EmptyFiles { .. } => "empty-files",
            Error::MissingField { .. } => "missing-field",
            Error::BadSha { .. } => "bad-sha",
            Error::NoRecord { .. } => "no-record",
            Error::SeveralRecords { .. } => "several-records",
            Error::Unhashable { .. } => "unhashable",
            Error::HashMismatch { .. } => "hash-mismatch",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "signing refused ({}): ", self.kind())?;
        match self {
            Error::MissingEnv => write!(f, "{EVIDENCE_ENV} is not set in a signed QA run"),
            Error::Unreadable { path, source } => {
                write!(
                    f,
                    "cannot read signing evidence {}: {source}",
                    path.display()
                )
            }
            Error::InvalidJson { receipt, detail } => {
                write!(f, "{} is not JSON: {detail}", receipt_name(receipt))
            }
            Error::UnsupportedSchema { receipt } => {
                write!(f, "{} schema is not 1", receipt_name(receipt))
            }
            Error::EmptyFiles { receipt } => {
                write!(f, "{} has no files", receipt_name(receipt))
            }
            Error::MissingField {
                receipt,
                index,
                field,
            } => write!(
                f,
                "{} files[{index}] has no string `{field}`",
                receipt_name(receipt)
            ),
            Error::BadSha { receipt, index } => write!(
                f,
                "{} files[{index}].sha256After is not a lowercase SHA-256",
                receipt_name(receipt)
            ),
            Error::NoRecord { receipt, signed } => write!(
                f,
                "{} has no record for {}",
                receipt_name(receipt),
                signed.display()
            ),
            Error::SeveralRecords {
                receipt,
                signed,
                count,
            } => write!(
                f,
                "{} has more than one record ({count}) for {}",
                receipt_name(receipt),
                signed.display()
            ),
            Error::Unhashable { path, source } => {
                write!(f, "cannot hash {}: {source}", path.display())
            }
            Error::HashMismatch {
                actual,
                signed,
                actual_sha256,
                signed_sha256,
            } => write!(
                f,
                "{} hashes to {actual_sha256}, not the signed {} ({signed_sha256})",
                actual.display(),
                signed.display()
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Unreadable { source, .. } | Error::Unhashable { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// One signed file of the receipt: the fields a consumer relies on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// The file's receipt-relative path (`/` separators), e.g. `aarch64-apple-darwin/debug/heardright-engine`.
    pub file: String,
    /// The signed copy's path, as the receipt wrote it.
    pub signed: PathBuf,
    /// The signed copy's lowercase hex SHA-256.
    pub sha256_after: String,
    /// The unsigned build output's lowercase hex SHA-256, when the receipt carries it (the `sign` step always does): what ties a
    /// signed copy to the attested build output it was made from (GB-79).
    pub sha256_before: Option<String>,
}

/// A parsed schema 1 signing receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    source: Option<PathBuf>,
    records: Vec<Record>,
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

impl Evidence {
    /// Read and parse the receipt named by `RIGHTKIT_QA_SIGNING_EVIDENCE`.
    pub fn from_env() -> Result<Evidence, Error> {
        Self::from_env_value(std::env::var_os(EVIDENCE_ENV))
    }

    fn from_env_value(value: Option<OsString>) -> Result<Evidence, Error> {
        match value {
            Some(path) if !path.is_empty() => Self::load(Path::new(&path)),
            _ => Err(Error::MissingEnv),
        }
    }

    /// Read and parse the receipt at `path`.
    pub fn load(path: &Path) -> Result<Evidence, Error> {
        let bytes = std::fs::read(path).map_err(|source| Error::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse_from(&bytes, Some(path))
    }

    /// Parse a receipt's bytes. Errors do not name a path.
    pub fn parse(bytes: &[u8]) -> Result<Evidence, Error> {
        Self::parse_from(bytes, None)
    }

    fn parse_from(bytes: &[u8], source: Option<&Path>) -> Result<Evidence, Error> {
        let receipt = || source.map(Path::to_path_buf);
        let root: Value = serde_json::from_slice(bytes).map_err(|e| Error::InvalidJson {
            receipt: receipt(),
            detail: e.to_string(),
        })?;
        if root.get("schema").and_then(Value::as_u64) != Some(1) {
            return Err(Error::UnsupportedSchema { receipt: receipt() });
        }
        let files = root
            .get("files")
            .and_then(Value::as_array)
            .filter(|files| !files.is_empty())
            .ok_or_else(|| Error::EmptyFiles { receipt: receipt() })?;
        let mut records = Vec::with_capacity(files.len());
        for (index, entry) in files.iter().enumerate() {
            let field = |name: &'static str| {
                entry
                    .get(name)
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::MissingField {
                        receipt: receipt(),
                        index,
                        field: name,
                    })
            };
            let sha256_after = field("sha256After")?;
            if !is_sha256_hex(sha256_after) {
                return Err(Error::BadSha {
                    receipt: receipt(),
                    index,
                });
            }
            let signed = field("signed")?;
            let file = field("file")?;
            let sha256_before = match entry.get("sha256Before") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) if is_sha256_hex(s) => Some(s.clone()),
                Some(_) => {
                    return Err(Error::BadSha {
                        receipt: receipt(),
                        index,
                    })
                }
            };
            records.push(Record {
                file: file.to_owned(),
                signed: PathBuf::from(signed),
                sha256_after: sha256_after.to_owned(),
                sha256_before,
            });
        }
        Ok(Evidence {
            source: source.map(Path::to_path_buf),
            records,
        })
    }

    /// Every record of the receipt.
    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// The signed copy of the file the build left at receipt-relative `file` (`/` separators, the
    /// `files[].file` the `sign` step wrote): exactly one record must name it, and the signed copy
    /// must still hash to that record's `sha256After`. A test that launches a product binary (a
    /// sidecar, `CARGO_BIN_EXE_<bin>`) inside a signed suite resolves it here and launches the
    /// returned path, never the unsigned build output.
    pub fn signed_for(&self, file: &str) -> Result<PathBuf, Error> {
        let mut matches = self.records.iter().filter(|r| r.file == file);
        let record = match (matches.next(), matches.next()) {
            (Some(record), None) => record,
            (None, _) => {
                return Err(Error::NoRecord {
                    receipt: self.source.clone(),
                    signed: PathBuf::from(file),
                })
            }
            (Some(_), Some(_)) => {
                return Err(Error::SeveralRecords {
                    receipt: self.source.clone(),
                    signed: PathBuf::from(file),
                    count: self.records.iter().filter(|r| r.file == file).count(),
                })
            }
        };
        self.require(&record.signed, &record.signed)?;
        Ok(record.signed.clone())
    }

    /// Require exactly one record whose `signed` is `signed` (compared canonicalised), and require
    /// `actual` (the signed copy itself, or a copy of it the run launches) to hash to that
    /// record's `sha256After`.
    pub fn require(&self, signed: &Path, actual: &Path) -> Result<(), Error> {
        let mut matches = self.records.iter().filter(|r| same_path(&r.signed, signed));
        let record = match (matches.next(), matches.next()) {
            (Some(record), None) => record,
            (None, _) => {
                return Err(Error::NoRecord {
                    receipt: self.source.clone(),
                    signed: signed.to_path_buf(),
                })
            }
            (Some(_), Some(_)) => {
                let count = self
                    .records
                    .iter()
                    .filter(|r| same_path(&r.signed, signed))
                    .count();
                return Err(Error::SeveralRecords {
                    receipt: self.source.clone(),
                    signed: signed.to_path_buf(),
                    count,
                });
            }
        };
        let digest =
            rightkit_fs::digest::sha256_file(actual).map_err(|source| Error::Unhashable {
                path: actual.to_path_buf(),
                source,
            })?;
        if digest == record.sha256_after {
            Ok(())
        } else {
            Err(Error::HashMismatch {
                actual: actual.to_path_buf(),
                signed: signed.to_path_buf(),
                actual_sha256: digest,
                signed_sha256: record.sha256_after.clone(),
            })
        }
    }
}

/// The signed main binary the `sign` step exported as `RIGHTKIT_QA_APP_BINARY`, if any.
pub fn app_binary() -> Option<PathBuf> {
    app_binary_from(std::env::var_os(APP_BINARY_ENV))
}

fn app_binary_from(value: Option<OsString>) -> Option<PathBuf> {
    value.filter(|v| !v.is_empty()).map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // SHA-256 of the three bytes "abc" (FIPS 180-2, B.1).
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn temp(name: &str, bytes: &[u8]) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rightkit-qa-signing-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn receipt(records: &[(&Path, &str)]) -> String {
        let files: Vec<Value> = records
            .iter()
            .map(|(signed, sha)| {
                serde_json::json!({
                    "platform": "mac", "file": "debug/x", "signed": signed,
                    "sha256Before": sha, "sha256After": sha, "extra": { "any": 1 }
                })
            })
            .collect();
        serde_json::json!({ "schema": 1, "step": "qa-sign", "files": files }).to_string()
    }

    fn sha_of(bytes: &[u8]) -> String {
        rightkit_fs::digest::sha256_file(&temp("hashme", bytes)).unwrap()
    }

    fn parse(text: &str) -> Result<Evidence, Error> {
        Evidence::parse(text.as_bytes())
    }

    #[test]
    fn a_copy_with_the_signed_hash_passes() {
        let signed = temp("shell", b"abc");
        let launched = temp("shell-stash", b"abc");
        let evidence = parse(&receipt(&[(&signed, ABC)])).unwrap();
        assert_eq!(evidence.records().len(), 1);
        evidence.require(&signed, &launched).unwrap();
        evidence.require(&signed, &signed).unwrap();
    }

    #[test]
    fn a_non_canonical_signed_path_still_matches() {
        let signed = temp("dotted", b"abc");
        let dir = signed.parent().unwrap();
        let dotted = dir.join(".").join("dotted");
        let evidence = parse(&receipt(&[(&dotted, ABC)])).unwrap();
        evidence.require(&signed, &signed).unwrap();
    }

    #[test]
    fn a_load_from_a_file_passes() {
        let signed = temp("loaded", b"abc");
        let file = temp("evidence.json", receipt(&[(&signed, ABC)]).as_bytes());
        Evidence::load(&file)
            .unwrap()
            .require(&signed, &signed)
            .unwrap();
    }

    #[test]
    fn a_copy_with_other_bytes_is_refused() {
        let signed = temp("engine", b"abc");
        let stale = temp("engine-stale", b"abd");
        let evidence = parse(&receipt(&[(&signed, ABC)])).unwrap();
        let error = evidence.require(&signed, &stale).unwrap_err();
        assert!(matches!(error, Error::HashMismatch { .. }), "{error}");
        let text = error.to_string();
        assert!(text.contains("not the signed"));
        assert!(text.contains("engine-stale"));
        assert!(text.contains("hash-mismatch"));
        assert!(text.contains(ABC));
        assert!(text.contains(&sha_of(b"abd")));
        assert_eq!(error.kind(), "hash-mismatch");
    }

    #[test]
    fn no_record_is_refused() {
        let signed = temp("only", b"abc");
        let other = temp("other", b"abc");
        let evidence = parse(&receipt(&[(&other, ABC)])).unwrap();
        let error = evidence.require(&signed, &signed).unwrap_err();
        assert!(matches!(error, Error::NoRecord { .. }), "{error}");
        assert!(error.to_string().contains("no record"));
    }

    #[test]
    fn several_records_are_refused() {
        let signed = temp("twice", b"abc");
        let evidence = parse(&receipt(&[(&signed, ABC), (&signed, ABC)])).unwrap();
        let error = evidence.require(&signed, &signed).unwrap_err();
        assert!(
            matches!(error, Error::SeveralRecords { count: 2, .. }),
            "{error}"
        );
        assert!(error.to_string().contains("more than one"));
    }

    #[test]
    fn an_unhashable_launched_file_is_refused() {
        let signed = temp("present", b"abc");
        let gone = signed.with_file_name("missing-launched");
        let evidence = parse(&receipt(&[(&signed, ABC)])).unwrap();
        let error = evidence.require(&signed, &gone).unwrap_err();
        assert!(matches!(error, Error::Unhashable { .. }), "{error}");
        assert!(error.to_string().contains("missing-launched"));
    }

    #[test]
    fn malformed_receipts_are_refused() {
        assert!(matches!(
            parse("not json").unwrap_err(),
            Error::InvalidJson { .. }
        ));
        assert!(matches!(
            parse(r#"{"schema":2,"files":[]}"#).unwrap_err(),
            Error::UnsupportedSchema { .. }
        ));
        assert!(matches!(
            parse(r#"{"files":[{"signed":"/x","sha256After":"a"}]}"#).unwrap_err(),
            Error::UnsupportedSchema { .. }
        ));
        assert!(matches!(
            parse(r#"{"schema":"1","files":[]}"#).unwrap_err(),
            Error::UnsupportedSchema { .. }
        ));
        assert!(matches!(
            parse(r#"{"schema":1,"files":[]}"#).unwrap_err(),
            Error::EmptyFiles { .. }
        ));
        assert!(matches!(
            parse(r#"{"schema":1}"#).unwrap_err(),
            Error::EmptyFiles { .. }
        ));
        let no_sha = parse(r#"{"schema":1,"files":[{"signed":"/x"}]}"#).unwrap_err();
        assert!(
            matches!(
                no_sha,
                Error::MissingField {
                    field: "sha256After",
                    index: 0,
                    ..
                }
            ),
            "{no_sha}"
        );
        let sha = format!(r#"{{"schema":1,"files":[{{"sha256After":"{ABC}"}}]}}"#);
        assert!(matches!(
            parse(&sha).unwrap_err(),
            Error::MissingField {
                field: "signed",
                ..
            }
        ));
        let upper = ABC.to_uppercase();
        for bad in [upper.as_str(), "abc", "", &ABC[..63]] {
            let text =
                format!(r#"{{"schema":1,"files":[{{"signed":"/x","sha256After":"{bad}"}}]}}"#);
            let error = parse(&text).unwrap_err();
            assert!(matches!(error, Error::BadSha { index: 0, .. }), "{error}");
            assert!(error.to_string().contains("lowercase"));
        }
        let non_hex = "g".repeat(64);
        let text =
            format!(r#"{{"schema":1,"files":[{{"signed":"/x","sha256After":"{non_hex}"}}]}}"#);
        assert!(matches!(parse(&text).unwrap_err(), Error::BadSha { .. }));
    }

    #[test]
    fn a_bad_second_record_is_refused_with_its_index() {
        let text = format!(
            r#"{{"schema":1,"files":[{{"file":"a","signed":"/a","sha256After":"{ABC}"}},{{"file":"b","signed":"/b","sha256After":"zz"}}]}}"#
        );
        assert!(matches!(
            parse(&text).unwrap_err(),
            Error::BadSha { index: 1, .. }
        ));
    }

    #[test]
    fn signed_for_resolves_a_sidecar_by_its_receipt_relative_file() {
        let engine = temp("heardright-engine", b"signed engine");
        let main = temp("app", b"signed app");
        let text = serde_json::json!({ "schema": 1, "files": [
            { "file": "aarch64-apple-darwin/debug/app", "signed": main, "sha256After": sha_of(b"signed app") },
            { "file": "aarch64-apple-darwin/debug/heardright-engine", "signed": engine, "sha256After": sha_of(b"signed engine") },
        ]})
        .to_string();
        let evidence = parse(&text).unwrap();
        assert_eq!(
            evidence
                .signed_for("aarch64-apple-darwin/debug/heardright-engine")
                .unwrap(),
            engine
        );
        assert!(matches!(
            evidence
                .signed_for("aarch64-apple-darwin/debug/other")
                .unwrap_err(),
            Error::NoRecord { .. }
        ));
        // A signed copy replaced after signing is refused, never returned.
        std::fs::write(&engine, b"stale engine").unwrap();
        assert!(matches!(
            evidence
                .signed_for("aarch64-apple-darwin/debug/heardright-engine")
                .unwrap_err(),
            Error::HashMismatch { .. }
        ));
    }

    #[test]
    fn signed_for_refuses_two_records_for_one_file() {
        let a = temp("a", b"x");
        let text = serde_json::json!({ "schema": 1, "files": [
            { "file": "debug/e", "signed": a, "sha256After": sha_of(b"x") },
            { "file": "debug/e", "signed": a, "sha256After": sha_of(b"x") },
        ]})
        .to_string();
        assert!(matches!(
            parse(&text).unwrap().signed_for("debug/e").unwrap_err(),
            Error::SeveralRecords { count: 2, .. }
        ));
    }

    #[test]
    fn a_missing_receipt_is_refused_and_named() {
        let gone = std::env::temp_dir().join("rightkit-qa-signing-does-not-exist.json");
        let error = Evidence::load(&gone).unwrap_err();
        assert!(matches!(error, Error::Unreadable { .. }), "{error}");
        assert!(error
            .to_string()
            .contains("rightkit-qa-signing-does-not-exist.json"));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn a_loaded_receipt_names_its_path_and_not_its_contents() {
        let secret = "SECRET-CONTENT-MARKER";
        let file = temp(
            "bad.json",
            format!("{{\"schema\":1,\"files\":[],\"x\":\"{secret}\"").as_bytes(),
        );
        let error = Evidence::load(&file).unwrap_err();
        assert!(matches!(error, Error::InvalidJson { .. }), "{error}");
        let text = error.to_string();
        assert!(text.contains("bad.json"));
        assert!(!text.contains(secret));
        let empty = temp(
            "empty.json",
            format!("{{\"schema\":1,\"files\":[],\"x\":\"{secret}\"}}").as_bytes(),
        );
        let error = Evidence::load(&empty).unwrap_err();
        assert!(error.to_string().contains("empty.json"));
        assert!(!error.to_string().contains(secret));
    }

    #[test]
    fn an_unset_or_empty_variable_is_refused() {
        assert!(matches!(
            Evidence::from_env_value(None).unwrap_err(),
            Error::MissingEnv
        ));
        assert!(matches!(
            Evidence::from_env_value(Some(OsString::new())).unwrap_err(),
            Error::MissingEnv
        ));
        assert!(Evidence::from_env_value(None)
            .unwrap_err()
            .to_string()
            .contains(EVIDENCE_ENV));
    }

    #[test]
    fn a_set_variable_loads_the_receipt() {
        let signed = temp("envsigned", b"abc");
        let file = temp("env-evidence.json", receipt(&[(&signed, ABC)]).as_bytes());
        let evidence = Evidence::from_env_value(Some(file.into_os_string())).unwrap();
        evidence.require(&signed, &signed).unwrap();
    }

    #[test]
    fn the_app_binary_is_the_variable_when_set_and_not_empty() {
        assert_eq!(app_binary_from(None), None);
        assert_eq!(app_binary_from(Some(OsString::new())), None);
        assert_eq!(
            app_binary_from(Some(OsString::from("/x/app"))),
            Some(PathBuf::from("/x/app"))
        );
    }
}
