//! PK07 phase contract, not a database implementation or SQLCipher qualification. This opaque
//! participant uses real FileTransaction bytes/identities to prove coordinator authority ordering.
//! DB's own tests qualify RelocationSession/RelocationSettlement and encrypted-store behavior.
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rightkit_daemon::{
    AppRoot, CutoverContext, CutoverError, CutoverParticipant, CutoverPhase, FsMove,
    MigrateOptions, ParticipantError, RecoverOutcome, RetireMode, RootCutover, RootSelection,
    Settlement, Suite, CUTOVER_RECORD,
};
use rightkit_fs::{FileKey, FileTransaction, TransactionEntry, TransactionOptions};
use serde_json::{json, Value};

const DATA: &str = "db/store.bin";
const JOURNAL: &str = ".db-phase.json";
const CHILD_ROOT: &str = "RIGHTKIT_DAEMON_DB_PHASE_CHILD_ROOT";
const CHILD_STOP: &str = "RIGHTKIT_DAEMON_DB_PHASE_CHILD_STOP";

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    old: PathBuf,
    app: AppRoot,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let old = root.join("old");
        fs::create_dir_all(old.join("db")).unwrap();
        fs::write(
            old.join(DATA),
            b"opaque participant bytes, not a fake SQL database",
        )
        .unwrap();
        fs::write(old.join("settings.json"), b"{\"fixture\":true}").unwrap();
        let app = Suite::at(root.join("suite"))
            .unwrap()
            .app_root("DbPhases")
            .unwrap();
        Self {
            _dir: dir,
            root,
            old,
            app,
        }
    }

    fn participant(&self) -> PhasedStore {
        PhasedStore {
            app: self.app.clone(),
            events: self.root.join("events"),
            stop: None,
            fail_copy: false,
            fail_reopen: false,
            fail_cleanup: false,
            host_state: HostState::Stopped,
            reopen_attempted: false,
        }
    }

    fn cutover(&self, participant: PhasedStore) -> RootCutover {
        RootCutover::new(self.app.clone(), &self.old)
            .fs_move(FsMove::Tree(MigrateOptions::strict()))
            .participant(participant)
    }

    fn events(&self) -> Vec<String> {
        fs::read_to_string(self.root.join("events"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// Adapter model: prepare/quiesce/copy/reopen/settle mirror the settled DB phase bridge. Copy
/// carries bytes; daemon journal alone grants authority. No database crate dependency exists.
struct PhasedStore {
    app: AppRoot,
    events: PathBuf,
    stop: Option<String>,
    fail_copy: bool,
    fail_reopen: bool,
    fail_cleanup: bool,
    host_state: HostState,
    reopen_attempted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostState {
    Old,
    Stopped,
    New,
}

fn participant_error(error: impl std::fmt::Display) -> ParticipantError {
    ParticipantError::new(error.to_string())
}

fn identity(path: &Path) -> Value {
    let key = FileKey::of_path(path).unwrap();
    json!({"volume":key.volume,"index":key.index})
}

fn file_evidence(path: &Path) -> (FileKey, Vec<u8>) {
    (FileKey::of_path(path).unwrap(), fs::read(path).unwrap())
}

impl PhasedStore {
    fn event(&self, event: &str) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.events)
            .unwrap();
        writeln!(file, "{event}").unwrap();
        file.sync_all().unwrap();
    }

    fn stop_at(&self, boundary: &str) {
        if self.stop.as_deref() == Some(boundary) {
            let ready = self.events.with_file_name("ready");
            let mut file = fs::File::create(ready).unwrap();
            file.write_all(boundary.as_bytes()).unwrap();
            file.sync_all().unwrap();
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }

    fn verify_copy(&self, cx: &CutoverContext) -> Result<Value, ParticipantError> {
        let journal: Value = serde_json::from_slice(
            &fs::read(cx.new_root().join(JOURNAL)).map_err(participant_error)?,
        )
        .map_err(participant_error)?;
        if journal["outcome"] != "copy_committed"
            || journal["old"] != identity(&cx.old_root().join(DATA))
            || journal["new"] != identity(&cx.new_root().join(DATA))
            || fs::read(cx.old_root().join(DATA)).map_err(participant_error)?
                != fs::read(cx.new_root().join(DATA)).map_err(participant_error)?
        {
            return Err(ParticipantError::new(
                "copy outcome or identity remains unknown",
            ));
        }
        Ok(journal)
    }
}

impl CutoverParticipant for PhasedStore {
    fn name(&self) -> &str {
        "database-phases"
    }
    fn owned_paths(&self) -> Vec<PathBuf> {
        vec!["db".into(), JOURNAL.into()]
    }

    fn prepare(&mut self, _: &CutoverContext) -> Result<(), ParticipantError> {
        assert_eq!(
            RootCutover::status(&self.app).unwrap().unwrap().selected,
            RootSelection::Old
        );
        self.host_state = HostState::Old;
        self.event("prepare");
        Ok(())
    }

    fn quiesce(&mut self, _: &CutoverContext) -> Result<(), ParticipantError> {
        assert_eq!(self.host_state, HostState::Old);
        self.host_state = HostState::Stopped;
        self.event("quiesced");
        Ok(())
    }

    fn relocate(&mut self, cx: &CutoverContext) -> Result<(), ParticipantError> {
        assert_eq!(self.host_state, HostState::Stopped);
        assert_eq!(
            RootCutover::status(&self.app).unwrap().unwrap().selected,
            RootSelection::Old
        );
        fs::create_dir_all(cx.new_root().join("db")).map_err(participant_error)?;
        let journal_path = cx.new_root().join(JOURNAL);
        if journal_path.exists() {
            self.verify_copy(cx)?;
        } else {
            let report = FileTransaction::prepare(
                cx.new_root(),
                vec![TransactionEntry::create_new(DATA, cx.old_root().join(DATA))],
                TransactionOptions::default(),
            )
            .map_err(participant_error)?
            .commit()
            .map_err(participant_error)?;
            if !report.is_committed() {
                return Err(ParticipantError::new(
                    "copy transaction did not commit; recover it",
                ));
            }
            let journal = json!({"outcome":"copy_committed","transactionId":report.id(),
                "old":identity(&cx.old_root().join(DATA)),"new":identity(&cx.new_root().join(DATA))});
            let mut file = fs::File::create(&journal_path).map_err(participant_error)?;
            file.write_all(&serde_json::to_vec(&journal).unwrap())
                .map_err(participant_error)?;
            file.sync_all().map_err(participant_error)?;
            self.verify_copy(cx)?;
        }
        self.event("copy_committed_writers_stopped");
        assert_eq!(self.host_state, HostState::Stopped);
        // Copy never activates a store or writes the daemon's decision record.
        assert_eq!(
            RootCutover::status(&self.app).unwrap().unwrap().phase,
            CutoverPhase::Copying
        );
        self.stop_at("before_switched");
        if self.fail_copy {
            return Err(ParticipantError::new("fail after committed copy"));
        }
        Ok(())
    }

    fn reopen(&mut self, cx: &CutoverContext) -> Result<(), ParticipantError> {
        assert_eq!(
            RootCutover::status(&self.app).unwrap().unwrap().phase,
            CutoverPhase::Switched
        );
        assert_eq!(self.host_state, HostState::Stopped);
        // Fence old resumption before verification/open can fail, matching DB's phased session.
        self.reopen_attempted = true;
        self.event("reopen_attempt_writers_stopped");
        self.stop_at("after_switched");
        // A fresh process reconstructs these identities directly; it has no prior session.
        self.verify_copy(cx)?;
        if self.fail_reopen {
            return Err(ParticipantError::new("new store open refused"));
        }
        self.host_state = HostState::New;
        self.event("new_active");
        self.stop_at("after_reopen");
        Ok(())
    }

    fn settle(
        &mut self,
        cx: &CutoverContext,
        settlement: Settlement,
    ) -> Result<(), ParticipantError> {
        match settlement {
            Settlement::Committed => {
                assert!(self.reopen_attempted);
                assert_eq!(self.host_state, HostState::New);
                assert_eq!(
                    RootCutover::status(&self.app).unwrap().unwrap().phase,
                    CutoverPhase::Completed
                );
                self.stop_at("after_completed");
                self.verify_copy(cx)?;
                fs::remove_file(cx.new_root().join(JOURNAL)).map_err(participant_error)?;
                self.event("committed_cleanup");
                self.stop_at("after_cleanup");
                if self.fail_cleanup {
                    return Err(ParticipantError::new(
                        "cleanup interrupted after journal removal",
                    ));
                }
            }
            Settlement::Aborted => {
                assert!(
                    !self.reopen_attempted,
                    "an attempted new-root reopen forbids old resume"
                );
                assert_eq!(
                    RootCutover::status(&self.app).unwrap().unwrap().selected,
                    RootSelection::Old
                );
                self.host_state = HostState::Old;
                self.event("old_resumed_copy_retained");
            }
            _ => return Err(ParticipantError::new("unknown settlement")),
        }
        Ok(())
    }

    fn recover_completed(&mut self, cx: &CutoverContext) -> Result<(), ParticipantError> {
        assert!(cx.resumed());
        assert_ne!(self.host_state, HostState::Old);
        let status = RootCutover::status(&self.app).unwrap().unwrap();
        assert_eq!(status.phase, CutoverPhase::Completed);
        assert_eq!(status.selected, RootSelection::New);
        // Cleanup reconstruction uses durable evidence, never prepare/reopen or old resumption.
        let journal = cx.new_root().join(JOURNAL);
        match fs::symlink_metadata(&journal) {
            Ok(_) => {
                self.verify_copy(cx)?;
                if self.fail_cleanup {
                    return Err(ParticipantError::new("completed cleanup refused"));
                }
                fs::remove_file(journal).map_err(participant_error)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(participant_error(error)),
        }
        self.event("completed_recovery_cleanup");
        Ok(())
    }
}

#[test]
fn abort_resumes_old_but_retains_committed_copy_for_next_generation() {
    let f = Fixture::new();
    let original = file_evidence(&f.old.join(DATA));
    let mut participant = f.participant();
    participant.fail_copy = true;
    assert!(f.cutover(participant).run().is_err());
    let copy = file_evidence(&f.app.root().join(DATA));
    assert_eq!(original, file_evidence(&f.old.join(DATA)));
    assert_eq!(original.1, copy.1);
    assert!(f.app.root().join(JOURNAL).exists());
    assert_eq!(f.events().last().unwrap(), "old_resumed_copy_retained");
    assert_eq!(
        RootCutover::status(&f.app).unwrap().unwrap().phase,
        CutoverPhase::Aborted
    );
    assert_eq!(f.cutover(f.participant()).run().unwrap().generation, 2);
    assert_eq!(copy, file_evidence(&f.app.root().join(DATA)));
    assert_eq!(original, file_evidence(&f.old.join(DATA)));
    assert!(!f.app.root().join(JOURNAL).exists());
    assert_eq!(
        fs::read(f.app.root().join("settings.json")).unwrap(),
        fs::read(f.old.join("settings.json")).unwrap()
    );
}

#[test]
fn reopen_failure_keeps_new_authority_and_never_resumes_old() {
    let f = Fixture::new();
    let mut participant = f.participant();
    participant.fail_reopen = true;
    assert!(matches!(
        f.cutover(participant).run(),
        Err(CutoverError::ReopenPending { .. })
    ));
    assert_eq!(
        RootCutover::status(&f.app).unwrap().unwrap().selected,
        RootSelection::New
    );
    assert!(!f
        .events()
        .iter()
        .any(|e| e == "old_resumed_copy_retained" || e == "new_active"));
    let before = f.events().len();
    assert!(matches!(
        f.cutover(f.participant()).recover().unwrap(),
        RecoverOutcome::Resumed(_)
    ));
    assert_eq!(
        &f.events()[before..],
        [
            "reopen_attempt_writers_stopped",
            "new_active",
            "committed_cleanup"
        ]
    );
}

#[test]
fn unknown_copy_outcome_recovery_preserves_journals_and_both_file_identities() {
    let f = Fixture::new();
    let mut participant = f.participant();
    participant.fail_reopen = true;
    assert!(matches!(
        f.cutover(participant).run(),
        Err(CutoverError::ReopenPending { .. })
    ));
    let journal_path = f.app.root().join(JOURNAL);
    let mut journal: Value = serde_json::from_slice(&fs::read(&journal_path).unwrap()).unwrap();
    journal["outcome"] = json!("unknown");
    fs::write(&journal_path, serde_json::to_vec(&journal).unwrap()).unwrap();
    let participant_before = file_evidence(&journal_path);
    let daemon_before = file_evidence(&f.app.root().join(CUTOVER_RECORD));
    let old_before = file_evidence(&f.old.join(DATA));
    let new_before = file_evidence(&f.app.root().join(DATA));
    assert!(matches!(
        f.cutover(f.participant()).recover(),
        Err(CutoverError::ReopenPending { .. })
    ));
    assert_eq!(participant_before, file_evidence(&journal_path));
    assert_eq!(
        daemon_before,
        file_evidence(&f.app.root().join(CUTOVER_RECORD))
    );
    assert_eq!(old_before, file_evidence(&f.old.join(DATA)));
    assert_eq!(new_before, file_evidence(&f.app.root().join(DATA)));
    assert_eq!(
        RootCutover::status(&f.app).unwrap().unwrap().phase,
        CutoverPhase::Switched
    );
    assert!(!f
        .events()
        .iter()
        .any(|e| e == "old_resumed_copy_retained" || e == "new_active"));
}

#[test]
fn db_phase_kill_helper_child() {
    let Some(root) = std::env::var_os(CHILD_ROOT).map(PathBuf::from) else {
        return;
    };
    let app = Suite::at(root.join("suite"))
        .unwrap()
        .app_root("DbPhases")
        .unwrap();
    let participant = PhasedStore {
        app: app.clone(),
        events: root.join("events"),
        stop: Some(std::env::var(CHILD_STOP).unwrap()),
        fail_copy: false,
        fail_reopen: false,
        fail_cleanup: false,
        host_state: HostState::Stopped,
        reopen_attempted: false,
    };
    RootCutover::new(app, root.join("old"))
        .fs_move(FsMove::Tree(MigrateOptions::strict()))
        .participant(participant)
        .run()
        .unwrap();
    panic!("child never reached requested boundary");
}

fn kill_at(f: &Fixture, boundary: &str) {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "db_phase_kill_helper_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ROOT, &f.root)
        .env(CHILD_STOP, boundary)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !f.root.join("ready").exists() {
        if Instant::now() > deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("helper never reached {boundary}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn killed_before_switched_keeps_old_authority_and_verified_copy() {
    let f = Fixture::new();
    let old_before = file_evidence(&f.old.join(DATA));
    kill_at(&f, "before_switched");
    let copied = file_evidence(&f.app.root().join(DATA));
    assert_eq!(
        RootCutover::status(&f.app).unwrap().unwrap().phase,
        CutoverPhase::Copying
    );
    let before = f.events();
    assert_eq!(before.last().unwrap(), "copy_committed_writers_stopped");
    assert!(matches!(
        f.cutover(f.participant()).recover().unwrap(),
        RecoverOutcome::Nothing
    ));
    assert_eq!(f.events(), before);
    assert_eq!(f.cutover(f.participant()).run().unwrap().generation, 2);
    assert_eq!(old_before, file_evidence(&f.old.join(DATA)));
    assert_eq!(copied, file_evidence(&f.app.root().join(DATA)));
}

#[test]
fn killed_after_switched_fresh_recovery_reopens_without_copy_or_old_resume() {
    let f = Fixture::new();
    let old_before = file_evidence(&f.old.join(DATA));
    kill_at(&f, "after_switched");
    let copied = file_evidence(&f.app.root().join(DATA));
    let status = RootCutover::status(&f.app).unwrap().unwrap();
    assert_eq!(status.phase, CutoverPhase::Switched);
    assert_eq!(status.selected, RootSelection::New);
    assert_eq!(
        fs::read(f.app.root().join("settings.json")).unwrap(),
        fs::read(f.old.join("settings.json")).unwrap()
    );
    let before = f.events().len();
    let mut recovery = f.cutover(f.participant());
    assert!(matches!(
        recovery.recover().unwrap(),
        RecoverOutcome::Resumed(_)
    ));
    assert_eq!(
        &f.events()[before..],
        [
            "reopen_attempt_writers_stopped",
            "new_active",
            "committed_cleanup"
        ]
    );
    assert_eq!(old_before, file_evidence(&f.old.join(DATA)));
    assert_eq!(copied, file_evidence(&f.app.root().join(DATA)));
    assert!(!f.app.root().join(JOURNAL).exists());
    assert!(matches!(
        recovery.recover().unwrap(),
        RecoverOutcome::Completed
    ));
}

#[test]
fn killed_after_reopen_before_completed_retains_replay_journal() {
    let f = Fixture::new();
    let original = file_evidence(&f.old.join(DATA));
    kill_at(&f, "after_reopen");
    assert_eq!(f.events().last().unwrap(), "new_active");
    let status = RootCutover::status(&f.app).unwrap().unwrap();
    assert_eq!(status.phase, CutoverPhase::Switched);
    assert_eq!(status.selected, RootSelection::New);
    let journal = file_evidence(&f.app.root().join(JOURNAL));
    let copied = file_evidence(&f.app.root().join(DATA));
    let mut refused = f.participant();
    refused.fail_reopen = true;
    assert!(matches!(
        f.cutover(refused).recover(),
        Err(CutoverError::ReopenPending { .. })
    ));
    assert_eq!(journal, file_evidence(&f.app.root().join(JOURNAL)));
    assert_eq!(
        RootCutover::status(&f.app).unwrap().unwrap().phase,
        CutoverPhase::Switched
    );
    let before = f.events().len();
    assert!(matches!(
        f.cutover(f.participant()).recover().unwrap(),
        RecoverOutcome::Resumed(_)
    ));
    assert_eq!(
        &f.events()[before..],
        [
            "reopen_attempt_writers_stopped",
            "new_active",
            "committed_cleanup"
        ]
    );
    assert_eq!(
        RootCutover::status(&f.app).unwrap().unwrap().phase,
        CutoverPhase::Completed
    );
    assert_eq!(original, file_evidence(&f.old.join(DATA)));
    assert_eq!(copied, file_evidence(&f.app.root().join(DATA)));
    assert!(!f.app.root().join(JOURNAL).exists());
    assert!(!f
        .events()
        .iter()
        .any(|event| event == "old_resumed_copy_retained"));
}

fn settlement_complete(app: &AppRoot) -> bool {
    let record: Value =
        serde_json::from_slice(&fs::read(app.root().join(CUTOVER_RECORD)).unwrap()).unwrap();
    record["settlement_complete"].as_bool().unwrap_or(false)
}

#[test]
fn killed_after_completed_recovers_only_cleanup_before_retiring_old() {
    let f = Fixture::new();
    let original = file_evidence(&f.old.join(DATA));
    kill_at(&f, "after_completed");
    let copied = file_evidence(&f.app.root().join(DATA));
    let journal = file_evidence(&f.app.root().join(JOURNAL));
    let status = RootCutover::status(&f.app).unwrap().unwrap();
    assert_eq!(status.phase, CutoverPhase::Completed);
    assert_eq!(status.selected, RootSelection::New);
    assert!(status.needs_recovery());
    assert!(!settlement_complete(&f.app));
    assert!(matches!(
        RootCutover::retire_old_root(&f.app, RetireMode::RenameAside),
        Err(CutoverError::SettlementPending { .. })
    ));
    assert_eq!(journal, file_evidence(&f.app.root().join(JOURNAL)));
    // A fresh process can refuse cleanup without losing its durable authority or opening either DB.
    let mut refused = f.participant();
    refused.fail_cleanup = true;
    assert!(matches!(
        f.cutover(refused).recover(),
        Err(CutoverError::SettlementPending { .. })
    ));
    assert_eq!(journal, file_evidence(&f.app.root().join(JOURNAL)));
    assert!(!settlement_complete(&f.app));
    let before = f.events().len();
    let mut recovery = f.cutover(f.participant());
    assert!(matches!(
        recovery.recover().unwrap(),
        RecoverOutcome::Completed
    ));
    assert_eq!(&f.events()[before..], ["completed_recovery_cleanup"]);
    assert!(settlement_complete(&f.app));
    assert!(!RootCutover::status(&f.app)
        .unwrap()
        .unwrap()
        .needs_recovery());
    assert_eq!(original, file_evidence(&f.old.join(DATA)));
    assert_eq!(copied, file_evidence(&f.app.root().join(DATA)));
    assert!(!f.app.root().join(JOURNAL).exists());
    let settled_events = f.events();
    assert!(matches!(
        recovery.recover().unwrap(),
        RecoverOutcome::Completed
    ));
    assert_eq!(settled_events, f.events());
    let retired = RootCutover::retire_old_root(&f.app, RetireMode::RenameAside).unwrap();
    assert_eq!(original, file_evidence(&retired.path.join(DATA)));
}

#[test]
fn partially_finished_settlement_is_visible_and_retryable_without_reopen() {
    let f = Fixture::new();
    let original = file_evidence(&f.old.join(DATA));
    let mut participant = f.participant();
    participant.fail_cleanup = true;
    let error = f.cutover(participant).run().unwrap_err();
    match &error {
        CutoverError::SettlementPending {
            generation,
            failures,
        } => {
            assert_eq!(*generation, 1);
            assert_eq!(failures.len(), 1);
            assert_eq!(failures[0].0, "database-phases");
            assert!(failures[0].1.message().contains("journal removal"));
        }
        other => panic!("cleanup failure was hidden: {other}"),
    }
    assert!(error.to_string().contains("selected new root"));
    assert_eq!(
        RootCutover::selected_root(&f.app, &f.old).unwrap(),
        fs::canonicalize(f.app.root()).unwrap()
    );
    assert!(!settlement_complete(&f.app));
    assert!(!f.app.root().join(JOURNAL).exists());
    assert!(matches!(
        RootCutover::retire_old_root(&f.app, RetireMode::RenameAside),
        Err(CutoverError::SettlementPending { .. })
    ));
    let before = f.events().len();
    // A failed settlement may already have removed its journal; this clean state is idempotent.
    assert!(f.cutover(f.participant()).run().unwrap().already_completed);
    assert_eq!(&f.events()[before..], ["completed_recovery_cleanup"]);
    assert!(settlement_complete(&f.app));
    assert_eq!(original, file_evidence(&f.old.join(DATA)));
    assert!(!f
        .events()
        .iter()
        .any(|event| event == "old_resumed_copy_retained"));
}

#[test]
fn killed_after_cleanup_and_legacy_completed_record_replay_idempotently() {
    let f = Fixture::new();
    let original = file_evidence(&f.old.join(DATA));
    kill_at(&f, "after_cleanup");
    assert!(!f.app.root().join(JOURNAL).exists());
    assert!(!settlement_complete(&f.app));
    // An old record without the marker must run cleanup, never assume it happened.
    let path = f.app.root().join(CUTOVER_RECORD);
    let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record
        .as_object_mut()
        .unwrap()
        .remove("settlement_complete");
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    assert!(RootCutover::status(&f.app)
        .unwrap()
        .unwrap()
        .needs_recovery());
    assert!(matches!(
        RootCutover::retire_old_root(&f.app, RetireMode::RenameAside),
        Err(CutoverError::SettlementPending { .. })
    ));
    let before = f.events().len();
    assert!(matches!(
        f.cutover(f.participant()).recover().unwrap(),
        RecoverOutcome::Completed
    ));
    assert_eq!(&f.events()[before..], ["completed_recovery_cleanup"]);
    assert!(settlement_complete(&f.app));
    assert_eq!(original, file_evidence(&f.old.join(DATA)));
}
