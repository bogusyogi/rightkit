//! Scenario wrapper for plain Rust integration tests (`#[test] fn x() { h.scenario(...) }`).
//!
//! What the declarative TOML runner gives a scenario, for Rust tests:
//! * **tier gating**: `fast` always runs; `full`, `paid` (or any name) run when selected by a
//!   cargo feature ([`Harness::with_feature_tiers`]) or the settings key
//!   ([`Harness::with_tier_key`], default `RIGHTKIT_QA_TIER`, a comma list or `all`);
//! * **requires-skip**: `env:NAME` (answered by the [`Settings`] file, so it survives
//!   `rightkit cargo`), `file:path`, `cmd:program`, `platform:darwin|windows`; plus a runtime
//!   [`Scenario::skip`] for prerequisites only discoverable inside the test;
//! * **skip accounting**: libtest reports a skipped test as `ok`, so every outcome is printed
//!   (`[qa] PASS|SKIP|FAIL name`) and appended to `<evidence root>/summary.jsonl`;
//! * **per-scenario evidence**: `<evidence root>/<scenario-slug>/` holds the scenario's files and
//!   a hashed `evidence.json` [`Bundle`] (artifacts recorded with [`Scenario::keep`]);
//! * **orphan check**: every process started through [`Scenario::tracker`] must be gone when the
//!   scenario returns, or the scenario fails (survivors are killed first);
//! * optional **run lock** shared by every test binary ([`Harness::with_run_lock`]).
//!
//! ```no_run
//! use rightkit_qa::harness::Harness;
//! fn h() -> Harness {
//!     Harness::new("myapp", env!("CARGO_MANIFEST_DIR"))
//!         .unwrap()
//!         .with_feature_tiers(&[("full", cfg!(feature = "full")), ("paid", cfg!(feature = "paid"))])
//! }
//! #[test]
//! fn export_is_loud_enough() {
//!     h().scenario("export loudness", "full", &["cmd:ffmpeg"], |sc| {
//!         sc.note("measuring");
//!     });
//! }
//! ```
use crate::evidence::{Bundle, CheckStatus, RunStatus};
use crate::lock::{acquire, LockOptions};
use crate::process::Tracker;
use crate::scenario::{tier_selected, unmet_requirement_with};
use crate::settings::Settings;
use crate::util::{new_id, sha256_hex, Result};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Settings key read for the tier override unless [`Harness::with_tier_key`] changes it.
pub const DEFAULT_TIER_KEY: &str = "RIGHTKIT_QA_TIER";
/// Settings key read for the evidence root (relative values resolve against the app root).
pub const EVIDENCE_KEY: &str = "RIGHTKIT_QA_EVIDENCE";

/// How a scenario ended without failing (a failure panics, so libtest marks the test failed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Skipped(String),
}

impl Outcome {
    pub fn is_skipped(&self) -> bool {
        matches!(self, Outcome::Skipped(_))
    }
}

/// Shared configuration for every scenario of a test crate. Cheap to rebuild per test.
#[derive(Debug, Clone)]
pub struct Harness {
    app: String,
    root: PathBuf,
    settings: Settings,
    tier_key: String,
    feature_tiers: Vec<String>,
    explicit_tiers: Option<String>,
    evidence_root: PathBuf,
    lock: Option<(PathBuf, Duration)>,
    identity: Value,
    prefix: String,
    run: Arc<Mutex<Option<crate::workspace::QaWorkspace>>>,
    run_root: PathBuf,
    scratch_root: PathBuf,
    allow_external_evidence: bool,
}

impl Drop for Harness {
    fn drop(&mut self) {
        if Arc::strong_count(&self.run) != 1 {
            return;
        }
        if let Ok(mut run) = self.run.lock() {
            if let Some(ws) = run.take() {
                let _ = crate::workspace::dispose(&ws, true, 3, 10);
            }
        }
    }
}

impl Harness {
    /// `app` names the evidence; `root` (usually the repository root) resolves `file:`
    /// requirements and is where [`Settings::discover`] searches.
    pub fn new(app: &str, root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let settings = Settings::discover(&root)?;
        Ok(Self::with_parts(app, root, settings))
    }

    /// Test and embedding constructor with settings discovery and an explicit managed root.
    pub fn new_in_managed_root(
        app: &str,
        root: impl Into<PathBuf>,
        managed_root: impl Into<PathBuf>,
    ) -> Result<Self> {
        let root = root.into();
        let settings = Settings::discover(&root)?;
        Ok(Self::with_parts_in_managed_root(
            app,
            root,
            settings,
            managed_root,
        ))
    }

    /// Like [`Harness::new`] with settings already loaded.
    pub fn with_parts(app: &str, root: PathBuf, settings: Settings) -> Self {
        Self::with_parts_at(app, root, settings, None)
    }

    /// Test and embedding constructor with an explicit managed root. This keeps
    /// fixture runs off machine storage while preserving production defaults in
    /// [`Harness::with_parts`].
    pub fn with_parts_in_managed_root(
        app: &str,
        root: PathBuf,
        settings: Settings,
        managed_root: impl Into<PathBuf>,
    ) -> Self {
        let managed_root = managed_root.into();
        Self::with_parts_at(app, root, settings, Some(managed_root.as_path()))
    }

    fn with_parts_at(
        app: &str,
        root: PathBuf,
        settings: Settings,
        managed_root: Option<&Path>,
    ) -> Self {
        if let Some(p) = locate_ffprobe(app, &settings) {
            crate::media::set_ffprobe_bin(p);
        }
        let run = Some(
            crate::workspace::create_impl_for_harness(managed_root, app)
                .unwrap_or_else(|e| panic!("cannot create managed QA run: {e}")),
        );
        let evidence_root = run.as_ref().expect("managed run").evidence_dir.clone();
        let run_root = run.as_ref().expect("managed run").root.clone();
        Self {
            app: app.to_string(),
            root,
            settings,
            tier_key: DEFAULT_TIER_KEY.into(),
            feature_tiers: vec![],
            explicit_tiers: None,
            evidence_root,
            lock: None,
            identity: Value::Null,
            prefix: "[qa]".into(),
            scratch_root: run.as_ref().expect("managed run").home.join("scratch"),
            run: Arc::new(Mutex::new(run)),
            run_root,
            allow_external_evidence: managed_root.is_some(),
        }
    }

    /// Tiers enabled at compile time: `&[("full", cfg!(feature = "full")), ...]`.
    pub fn with_feature_tiers(mut self, tiers: &[(&str, bool)]) -> Self {
        self.feature_tiers = tiers
            .iter()
            .filter(|(_, on)| *on)
            .map(|(t, _)| t.to_string())
            .collect();
        self
    }

    /// Settings key whose value (comma list or `all`) replaces the computed tier selection.
    pub fn with_tier_key(mut self, key: &str) -> Self {
        self.tier_key = key.into();
        self
    }

    /// Fixed selection, ignoring features and settings.
    pub fn with_tiers(mut self, list: &str) -> Self {
        self.explicit_tiers = Some(list.into());
        self
    }

    pub fn with_evidence_root(mut self, dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        if !self.allow_external_evidence && !dir.starts_with(&self.run_root) {
            panic!(
                "QA evidence root must be inside managed run root: {}",
                self.run_root.display()
            );
        }
        self.evidence_root = dir;
        self
    }

    /// Serialize scenarios across threads and test binaries through a [`crate::lock`] directory.
    pub fn with_run_lock(mut self, lock_dir: impl Into<PathBuf>, timeout: Duration) -> Self {
        self.lock = Some((lock_dir.into(), timeout));
        self
    }

    /// Extra identity fields bound into every evidence bundle (binary digests, revisions).
    /// `config.sha256` defaults to the settings file digest.
    pub fn with_identity(mut self, identity: Value) -> Self {
        self.identity = identity;
        self
    }

    /// Hash a file (the binary under test) into the identity as `binaries.<label>`.
    pub fn with_identity_file(mut self, label: &str, path: &Path) -> Result<Self> {
        let digest = sha256_hex(&fs::read(path)?);
        if !self.identity.is_object() {
            self.identity = json!({});
        }
        self.identity["binaries"][label] = json!({"path": path, "sha256": digest});
        Ok(self)
    }

    /// Log line prefix (default `[qa]`; GenRight greps `[e2e]`).
    pub fn with_log_prefix(mut self, prefix: &str) -> Self {
        self.prefix = prefix.into();
        self
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn evidence_root(&self) -> &Path {
        &self.evidence_root
    }

    /// The active tier list: explicit, else the settings key, else `fast` plus feature tiers.
    pub fn selected_tiers(&self) -> String {
        if let Some(t) = &self.explicit_tiers {
            return t.clone();
        }
        if let Some(t) = self.settings.get(&self.tier_key) {
            return t;
        }
        let mut t = vec!["fast".to_string()];
        t.extend(self.feature_tiers.iter().cloned());
        t.join(",")
    }

    /// First unmet requirement, with `env:` answered by the settings (environment first).
    pub fn unmet(&self, requires: &[&str]) -> Option<String> {
        let reqs: Vec<String> = requires.iter().map(|s| s.to_string()).collect();
        unmet_requirement_with(&reqs, &self.root, &|k| self.settings.get(k))
    }

    /// Run one scenario. Returns [`Outcome::Passed`] or [`Outcome::Skipped`]; a failing body,
    /// a leaked process, or a lock timeout panics after its evidence is written.
    pub fn scenario(
        &self,
        name: &str,
        tier: &str,
        requires: &[&str],
        f: impl FnOnce(&Scenario),
    ) -> Outcome {
        let started = Instant::now();
        let dir = self.evidence_root.join(slug(name));
        let selected = self.selected_tiers();
        let gate = if !tier_selected(tier, &selected) {
            Some(format!("tier {tier} not selected (selected: {selected})"))
        } else {
            self.unmet(requires)
        };
        if let Some(reason) = gate {
            self.finish(
                name,
                tier,
                &dir,
                CheckStatus::Skipped,
                json!({"reason": reason}),
                &[],
                started,
            );
            return Outcome::Skipped(reason);
        }
        if let Err(e) = fs::create_dir_all(&dir) {
            panic!(
                "{} cannot create evidence dir {}: {e}",
                self.prefix,
                dir.display()
            );
        }
        let mut lock = match &self.lock {
            Some((lock_dir, timeout)) => {
                if let Some(p) = lock_dir.parent() {
                    let _ = fs::create_dir_all(p);
                }
                match acquire(
                    lock_dir,
                    &format!("{}:{name}", self.app),
                    &LockOptions {
                        timeout: *timeout,
                        ..Default::default()
                    },
                ) {
                    Ok(l) => Some(l),
                    Err(e) => {
                        let msg = format!("run lock: {e}");
                        self.finish(
                            name,
                            tier,
                            &dir,
                            CheckStatus::Failed,
                            json!({"error": msg}),
                            &[],
                            started,
                        );
                        panic!("{msg}");
                    }
                }
            }
            None => None,
        };
        let sc = Scenario {
            name: name.to_string(),
            tier: tier.to_string(),
            dir: dir.clone(),
            tracker: Tracker::new(),
            settings: self.settings.clone(),
            artifacts: Mutex::new(vec![]),
            notes: Mutex::new(vec![]),
            skipped: Mutex::new(None),
            scratches: Mutex::new(vec![]),
            scratch_root: self.scratch_root.clone(),
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&sc)));
        let orphans = match &result {
            Ok(()) => sc.tracker.assert_no_orphans(),
            Err(_) => {
                sc.tracker.kill_all();
                Ok(())
            }
        };
        if let Some(l) = lock.as_mut() {
            l.release();
        }
        let arts = sc.artifacts.lock().unwrap().clone();
        let notes = sc.notes.lock().unwrap().clone();
        let skipped = sc.skipped.lock().unwrap().clone();
        let scratches = sc.scratches.lock().unwrap().clone();
        // Scratch dirs are removed once the scenario passes or skips (kept on failure for
        // debugging, when they hold a kept artifact, or with the `keep_scratch` setting): they
        // live in the OS temp dir, and model-sized fixtures leaked hundreds of GB otherwise.
        let clean_scratch = || {
            if self
                .settings
                .get_bool("keep_scratch")
                .ok()
                .flatten()
                .unwrap_or(false)
            {
                return;
            }
            for d in &scratches {
                if arts.iter().any(|(_, p)| p.starts_with(d)) {
                    continue;
                }
                let _ = fs::remove_dir_all(d);
            }
        };
        match (result, orphans) {
            (Ok(()), Ok(())) => match skipped {
                Some(reason) => {
                    self.finish(
                        name,
                        tier,
                        &dir,
                        CheckStatus::Skipped,
                        json!({"reason": reason, "notes": notes}),
                        &arts,
                        started,
                    );
                    clean_scratch();
                    Outcome::Skipped(reason)
                }
                None => {
                    self.finish(
                        name,
                        tier,
                        &dir,
                        CheckStatus::Passed,
                        json!({"notes": notes}),
                        &arts,
                        started,
                    );
                    clean_scratch();
                    Outcome::Passed
                }
            },
            (Ok(()), Err(e)) => {
                self.finish(
                    name,
                    tier,
                    &dir,
                    CheckStatus::Failed,
                    json!({"error": e.0, "notes": notes}),
                    &arts,
                    started,
                );
                panic!("{e}");
            }
            (Err(p), _) => {
                let msg = p
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "panic".into());
                self.finish(
                    name,
                    tier,
                    &dir,
                    CheckStatus::Failed,
                    json!({"error": msg, "notes": notes}),
                    &arts,
                    started,
                );
                std::panic::resume_unwind(p);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn finish(
        &self,
        name: &str,
        tier: &str,
        dir: &Path,
        status: CheckStatus,
        mut details: Value,
        arts: &[(String, PathBuf)],
        started: Instant,
    ) {
        let ms = started.elapsed().as_millis() as u64;
        let (word, reason) = match status {
            CheckStatus::Passed => ("PASS", None),
            CheckStatus::Skipped => ("SKIP", details["reason"].as_str().map(String::from)),
            CheckStatus::Failed => ("FAIL", details["error"].as_str().map(String::from)),
        };
        match &reason {
            Some(r) => println!("{} {word} {name}: {r}", self.prefix),
            None => println!("{} {word} {name} ({ms} ms)", self.prefix),
        }
        details["ms"] = json!(ms);
        details["tier"] = json!(tier);
        let evidence = dir.join("evidence.json");
        let written = self.write_bundle(name, status, details, arts, &evidence);
        if let Err(e) = &written {
            println!("{} evidence for {name} not written: {e}", self.prefix);
        }
        let line = json!({
            "name": name, "tier": tier, "status": status, "reason": reason, "ms": ms,
            "evidence": written.ok().map(|_| evidence),
        });
        let _ = fs::create_dir_all(&self.evidence_root);
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.evidence_root.join("summary.jsonl"))
        {
            let _ = f.write_all(format!("{line}\n").as_bytes());
        }
    }

    fn write_bundle(
        &self,
        name: &str,
        status: CheckStatus,
        details: Value,
        arts: &[(String, PathBuf)],
        out: &Path,
    ) -> Result<()> {
        let platform = crate::util::platform().unwrap_or(std::env::consts::OS);
        let mut b = Bundle::new(
            &self.app,
            platform,
            &format!("{}-{}", std::process::id(), new_id()),
        )?;
        let mut names = vec![];
        let mut problems = vec![];
        for (n, p) in arts {
            match b.add_artifact(n, p) {
                Ok(_) => names.push(n.clone()),
                Err(e) => problems.push(e.0),
            }
        }
        let mut identity = if self.identity.is_object() {
            self.identity.clone()
        } else {
            json!({})
        };
        if identity.get("config").is_none() {
            let digest = self
                .settings
                .source()
                .and_then(|p| fs::read(p).ok())
                .map(|b| sha256_hex(&b))
                .unwrap_or_else(|| sha256_hex(b""));
            identity["config"] = json!({"sha256": digest, "settings": self.settings.source()});
        }
        identity["scenarios"] = json!([name]);
        identity["tiers"] = json!(self.selected_tiers());
        b.set_identity(identity)?;
        let mut details = details;
        if !problems.is_empty() {
            details["artifact_errors"] = json!(problems);
        }
        // An artifact that could not be hashed downgrades a pass: the receipt would be a claim.
        let status = if status == CheckStatus::Passed && !problems.is_empty() {
            CheckStatus::Failed
        } else {
            status
        };
        b.add_check(name, status, details, &names)?;
        b.finish(if status == CheckStatus::Failed {
            RunStatus::Failed
        } else {
            RunStatus::Passed
        });
        b.write(out)
    }
}

/// The handle a scenario body receives.
pub struct Scenario {
    name: String,
    tier: String,
    dir: PathBuf,
    tracker: Tracker,
    settings: Settings,
    artifacts: Mutex<Vec<(String, PathBuf)>>,
    notes: Mutex<Vec<String>>,
    skipped: Mutex<Option<String>>,
    scratches: Mutex<Vec<PathBuf>>,
    scratch_root: PathBuf,
}

impl Scenario {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn tier(&self) -> &str {
        &self.tier
    }
    /// This scenario's evidence directory (exists; files written here sit beside `evidence.json`).
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    /// Register every process the scenario starts here (`run_owned`, engine targets, sidecars,
    /// or `tracker().register(pid, label)` for anything spawned by hand).
    pub fn tracker(&self) -> &Tracker {
        &self.tracker
    }
    pub fn settings(&self) -> &Settings {
        &self.settings
    }
    /// Mark the scenario skipped from inside (a prerequisite only discoverable at run time);
    /// return from the body afterwards.
    pub fn skip(&self, reason: &str) {
        *self.skipped.lock().unwrap() = Some(reason.to_string());
    }
    /// Record a produced file; it is hashed into the evidence bundle when the scenario ends.
    pub fn keep(&self, name: &str, path: &Path) {
        self.artifacts
            .lock()
            .unwrap()
            .push((name.to_string(), path.to_path_buf()));
    }
    pub fn note(&self, msg: impl Into<String>) {
        self.notes.lock().unwrap().push(msg.into());
    }
    /// A fresh scratch directory inside this run's managed root. It is
    /// removed when the scenario passes or skips; kept on failure for debugging.
    pub fn scratch(&self, label: &str) -> PathBuf {
        let d = self
            .scratch_root
            .join(format!("rkqa-{}-{}", slug(label), &new_id()[..8]));
        let _ = fs::create_dir_all(&d);
        let d = d.canonicalize().unwrap_or(d);
        self.scratches.lock().unwrap().push(d.clone());
        d
    }
}

// Resolve explicit Settings pins before PATH discovery. Generic key wins, followed
// by this app's key and GenRight's historical key for existing settings files.
fn locate_ffprobe(app: &str, settings: &Settings) -> Option<PathBuf> {
    let app_key: String = app
        .to_uppercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let app_key = format!("{app_key}_FFPROBE");
    let key = ["RIGHTKIT_QA_FFPROBE", app_key.as_str(), "GENRIGHT_FFPROBE"]
        .into_iter()
        .find(|key| settings.get(key).is_some())
        .unwrap_or("RIGHTKIT_QA_FFPROBE");
    crate::media::locate_tool("ffprobe", settings, key)
}

/// Filesystem-safe scenario directory name (bounded length, hash suffix keeps it unique).
pub fn slug(name: &str) -> String {
    let base: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let short: String = base.chars().take(60).collect();
    format!("{short}-{}", &sha256_hex(name.as_bytes())[..8])
}

/// Read back `summary.jsonl` (one JSON object per finished scenario).
pub fn read_summary(evidence_root: &Path) -> Result<Vec<Value>> {
    let text = fs::read_to_string(evidence_root.join("summary.jsonl"))?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}

#[cfg(test)]
mod tool_settings_tests {
    use super::*;

    #[test]
    fn ffprobe_settings_accept_app_and_legacy_keys_before_path_discovery() {
        let root = std::env::temp_dir().join(format!("rkqa-ffprobe-{}", new_id()));
        fs::create_dir_all(&root).unwrap();
        let generic = root.join("generic");
        let app = root.join("app");
        let legacy = root.join("legacy");
        for p in [&generic, &app, &legacy] {
            fs::write(p, "fixture tool").unwrap();
        }
        let settings_file = root.join("settings.env");
        fs::write(
            &settings_file,
            format!("GENRIGHT_FFPROBE={}\n", legacy.display()),
        )
        .unwrap();
        let mut settings = Settings::load(&settings_file)
            .unwrap()
            .with_env_override(false);
        assert_eq!(locate_ffprobe("genright", &settings), Some(legacy.clone()));
        assert_eq!(locate_ffprobe("scraperight", &settings), Some(legacy));
        settings.set("SCRAPE_RIGHT_FFPROBE", app.to_string_lossy());
        assert_eq!(locate_ffprobe("scrape-right", &settings), Some(app));
        settings.set("RIGHTKIT_QA_FFPROBE", generic.to_string_lossy());
        assert_eq!(locate_ffprobe("scrape-right", &settings), Some(generic));
        settings.set(
            "RIGHTKIT_QA_FFPROBE",
            root.join("missing").to_string_lossy(),
        );
        assert_eq!(locate_ffprobe("scrape-right", &settings), None);
        fs::remove_dir_all(root).unwrap();
    }
}
