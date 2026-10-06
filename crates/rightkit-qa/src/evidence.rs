//! A receipt that names a file without hashing it is a claim, not evidence.
//! Every artifact is hashed when recorded, a check may only reference hashed
//! artifacts, and the bundle can be re-verified against the filesystem.
use crate::util::{err, now_iso, require_text, sha256_hex, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Running,
    Passed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub name: String,
    pub path: PathBuf,
    pub sha256: String,
    pub size_bytes: u64,
    pub recorded_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub details: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bundle {
    pub schema_version: u32,
    pub app: String,
    pub platform: String,
    pub run_id: String,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    pub status: RunStatus,
    /// What was tested: config, scenario, binary and source-revision digests.
    #[serde(default)]
    pub identity: Value,
    pub checks: Vec<Check>,
    pub artifacts: Vec<Artifact>,
}

impl Bundle {
    pub fn new(app: &str, platform: &str, run_id: &str) -> Result<Self> {
        Ok(Self {
            schema_version: 4,
            app: require_text(app, "app")?,
            platform: require_text(platform, "platform")?,
            run_id: require_text(run_id, "runId")?,
            started_at: now_iso(),
            finished_at: None,
            status: RunStatus::Running,
            identity: Value::Null,
            checks: vec![],
            artifacts: vec![],
        })
    }

    fn require_running(&self) -> Result<()> {
        if self.status != RunStatus::Running {
            return err("cannot modify a QA evidence bundle after the run has finished");
        }
        Ok(())
    }

    /// Hash a file into the bundle now, so a later replacement cannot masquerade.
    pub fn add_artifact(&mut self, name: &str, path: &Path) -> Result<Artifact> {
        self.require_running()?;
        let name = require_text(name, "artifact name")?;
        if self.artifacts.iter().any(|a| a.name == name) {
            return err(format!(
                "QA evidence already records an artifact named {name}"
            ));
        }
        let bytes = fs::read(path).map_err(|e| {
            crate::util::Error(format!(
                "cannot record QA evidence artifact {name}: {} is unreadable ({e})",
                path.display()
            ))
        })?;
        let rec = Artifact {
            name,
            path: fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
            sha256: sha256_hex(&bytes),
            size_bytes: bytes.len() as u64,
            recorded_at: now_iso(),
        };
        self.artifacts.push(rec.clone());
        Ok(rec)
    }

    pub fn add_check(
        &mut self,
        name: &str,
        status: CheckStatus,
        details: Value,
        artifacts: &[String],
    ) -> Result<()> {
        self.require_running()?;
        let name = require_text(name, "check name")?;
        let unknown: Vec<&String> = artifacts
            .iter()
            .filter(|r| !self.artifacts.iter().any(|a| &a.name == *r))
            .collect();
        if !unknown.is_empty() {
            return err(format!(
                "QA check {name} references artifacts that were never hashed: {}",
                unknown
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        self.checks.push(Check {
            name,
            status,
            details,
            artifacts: artifacts.to_vec(),
        });
        Ok(())
    }

    /// Bind this receipt to the exact inputs it exercised. A passing receipt without
    /// an identity is refused at write time.
    pub fn set_identity(&mut self, identity: Value) -> Result<()> {
        self.require_running()?;
        self.identity = identity;
        Ok(())
    }

    pub fn finish(&mut self, status: RunStatus) {
        self.status = status;
        self.finished_at = Some(now_iso());
    }

    /// Re-hash every artifact; empty means the receipt still describes the disk.
    pub fn verify(&self) -> Vec<(String, String)> {
        let mut drift = vec![];
        for a in &self.artifacts {
            match fs::read(&a.path) {
                Err(_) => drift.push((a.name.clone(), format!("missing at {}", a.path.display()))),
                Ok(b) => {
                    let h = sha256_hex(&b);
                    if h != a.sha256 {
                        drift.push((
                            a.name.clone(),
                            format!("sha256 {h} does not match recorded {}", a.sha256),
                        ));
                    }
                }
            }
        }
        drift
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        if self.status == RunStatus::Running || self.finished_at.is_none() {
            return err("refusing to write unfinished QA evidence");
        }
        if self.status == RunStatus::Passed {
            let drift = self.verify();
            if !drift.is_empty() {
                return err(format!(
                    "refusing to write a passing QA receipt whose evidence changed: {}",
                    drift
                        .iter()
                        .map(|(n, r)| format!("{n} ({r})"))
                        .collect::<Vec<_>>()
                        .join("; ")
                ));
            }
            let ok = self
                .identity
                .get("config")
                .and_then(|c| c.get("sha256"))
                .is_some()
                && self
                    .identity
                    .get("scenarios")
                    .and_then(Value::as_array)
                    .map(|a| !a.is_empty())
                    .unwrap_or(false);
            if !ok {
                return err("refusing to write a passing QA receipt that is not bound to config and scenario identity");
            }
        }
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        let r = fs::rename(&tmp, path);
        let _ = fs::remove_file(&tmp);
        Ok(r?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmpfile(body: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("rkqa-ev-{}", crate::util::new_id()));
        fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn check_must_reference_hashed_artifacts() {
        let mut b = Bundle::new("a", "darwin", "r").unwrap();
        let e = b
            .add_check("c", CheckStatus::Passed, json!(null), &["ghost".into()])
            .unwrap_err();
        assert!(e.0.contains("never hashed"));
        let f = tmpfile("x");
        b.add_artifact("shot", &f).unwrap();
        assert!(b.add_artifact("shot", &f).is_err());
        b.add_check("c", CheckStatus::Passed, json!({"k":1}), &["shot".into()])
            .unwrap();
    }

    #[test]
    fn passing_receipt_refused_when_bytes_drift() {
        let mut b = Bundle::new("a", "darwin", "r").unwrap();
        let f = tmpfile("one");
        b.add_artifact("shot", &f).unwrap();
        b.finish(RunStatus::Passed);
        fs::write(&f, "two").unwrap();
        let out = std::env::temp_dir().join(format!("rkqa-ev-out-{}.json", crate::util::new_id()));
        assert!(b.write(&out).unwrap_err().0.contains("evidence changed"));
        b.status = RunStatus::Failed;
        b.write(&out).unwrap();
        assert!(Bundle::new("a", "d", "r").unwrap().write(&out).is_err());
    }
}
