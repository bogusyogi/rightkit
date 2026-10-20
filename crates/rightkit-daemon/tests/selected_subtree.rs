//! Bounded installation-root companion migration; synthetic files only.
use std::fs;
use std::path::PathBuf;

use rightkit_daemon::{
    AppRoot, CutoverContext, CutoverParticipant, FsMove, MigrateOptions, MigrationScope,
    MigrationState, ParticipantError, RootCutover, Settlement, Suite, CUTOVER_RECORD,
};

const DIRECTORIES: [&str; 4] = ["webview2", "imgcache", "attachcache", "agent-exports"];
const LOGS: [&str; 6] = [
    "startup.log",
    "startup.log.1",
    "diagnostics.log",
    "diagnostics.log.1",
    "telemetry.log",
    "telemetry.log.1",
];

struct Fixture {
    _temporary: tempfile::TempDir,
    old: PathBuf,
    app: AppRoot,
}

fn roots(old: &std::path::Path) -> Vec<PathBuf> {
    let mut selected: Vec<PathBuf> = DIRECTORIES
        .into_iter()
        .chain(LOGS)
        .map(PathBuf::from)
        .collect();
    for entry in fs::read_dir(old).unwrap() {
        let name = entry.unwrap().file_name();
        let text = name.to_str().unwrap();
        if text.starts_with("mailright.sqlite.pre-v") && text.ends_with(".bak") {
            selected.push(PathBuf::from(name));
        }
    }
    selected
}

#[test]
fn selected_installation_companions_preserve_empty_directories_and_skip_payload() {
    let fixture = Fixture::new();
    RootCutover::new(fixture.app.clone(), &fixture.old)
        .fs_move(FsMove::Tree(MigrateOptions::strict()))
        .tree_roots(|cx| Ok(roots(cx.old_root())))
        .run()
        .unwrap();
    for directory in DIRECTORIES {
        assert!(fixture.app.root().join(directory).join("empty").is_dir());
        assert_eq!(
            fs::read(fixture.app.root().join(directory).join("data.bin")).unwrap(),
            directory.as_bytes()
        );
    }
    for file in LOGS.into_iter().chain([
        "mailright.sqlite.pre-v7.bak",
        "mailright.sqlite.pre-v20.bak",
    ]) {
        assert_eq!(
            fs::read(fixture.app.root().join(file)).unwrap(),
            file.as_bytes()
        );
    }
    for file in [
        "MailRight.exe",
        "uninstall.exe",
        "mailright.sqlite",
        "resources",
        "startup.log.2",
        "mailright.sqlite.pre-v20.bak.tmp",
        "unrelated.pre-v20.bak",
    ] {
        assert!(!fixture.app.root().join(file).exists(), "{file}");
        assert!(fixture.old.join(file).exists(), "old {file}");
    }
}

#[test]
fn empty_selection_does_not_satisfy_later_whole_tree_migration() {
    let fixture = Fixture::new();
    fixture
        .app
        .migrate_from_with(
            &fixture.old,
            &MigrateOptions::strict().include_roots(Vec::new()).unwrap(),
        )
        .unwrap();
    assert!(!fixture.app.root().join("MailRight.exe").exists());
    assert!(!fixture
        .app
        .root()
        .join(rightkit_daemon::MIGRATION_MARKER)
        .exists());
    let state: serde_json::Value = serde_json::from_slice(
        &fs::read(fixture.app.root().join(rightkit_daemon::MIGRATION_STATE)).unwrap(),
    )
    .unwrap();
    assert_eq!(state["version"], 3);
    assert_eq!(state["selected_roots"], serde_json::json!([]));
    let status = fixture.app.migration_status(&fixture.old).unwrap().unwrap();
    assert_eq!(status.state(), MigrationState::Ready);
    assert_eq!(status.scope(), &MigrationScope::Roots(Vec::new()));
    fixture
        .app
        .migrate_from_with(&fixture.old, &MigrateOptions::strict())
        .unwrap();
    assert!(fixture.app.root().join("MailRight.exe").is_file());
    assert_eq!(
        fixture
            .app
            .migration_status(&fixture.old)
            .unwrap()
            .unwrap()
            .scope(),
        &MigrationScope::All
    );
}

#[test]
fn migration_status_preserves_scope_and_refuses_ambiguous_metadata() {
    let fixture = Fixture::new();
    let source = fs::canonicalize(&fixture.old).unwrap();
    let state_path = fixture.app.root().join(rightkit_daemon::MIGRATION_STATE);
    let marker_path = fixture.app.root().join(rightkit_daemon::MIGRATION_MARKER);
    assert!(fixture
        .app
        .migration_status(&fixture.old)
        .unwrap()
        .is_none());
    for version in [1, 2] {
        fs::write(
            &state_path,
            serde_json::to_vec(
                &serde_json::json!({"version":version,"source":source,"state":"ready"}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            fixture
                .app
                .migration_status(&fixture.old)
                .unwrap()
                .unwrap()
                .scope(),
            &MigrationScope::All
        );
    }
    fs::write(&state_path, serde_json::to_vec(&serde_json::json!({"version":3,"source":source,"state":"incomplete","selected_roots":["startup.log","webview2"]})).unwrap()).unwrap();
    let selected = fixture.app.migration_status(&fixture.old).unwrap().unwrap();
    assert_eq!(selected.state(), MigrationState::Incomplete);
    assert_eq!(
        selected.scope(),
        &MigrationScope::Roots(vec![
            PathBuf::from("startup.log"),
            PathBuf::from("webview2")
        ])
    );
    assert_eq!(
        fixture.app.migration_state(&fixture.old).unwrap(),
        Some(MigrationState::Incomplete)
    );
    fs::write(
        &marker_path,
        serde_json::to_vec(&serde_json::json!({"source":source,"complete":false})).unwrap(),
    )
    .unwrap();
    for invalid in [
        serde_json::json!({"version":3,"source":source,"state":"ready"}),
        serde_json::json!({"version":3,"source":source,"state":"ready","selected_roots":["webview2","startup.log"]}),
        serde_json::json!({"version":3,"source":source,"state":"ready","selected_roots":["../escape"]}),
        serde_json::json!({"version":4,"source":source,"state":"ready"}),
        serde_json::json!({"version":2,"source":source,"state":"unknown"}),
    ] {
        fs::write(&state_path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(fixture.app.migration_status(&fixture.old).is_err());
        assert!(fixture.app.migration_state(&fixture.old).is_err());
    }
    fs::write(&state_path, b"malformed").unwrap();
    assert!(fixture.app.migration_status(&fixture.old).is_err());
    fs::write(
        &state_path,
        serde_json::to_vec(
            &serde_json::json!({"version":2,"source":source.join("other"),"state":"ready"}),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(fixture
        .app
        .migration_status(&fixture.old)
        .unwrap()
        .is_none());
    fs::remove_file(&state_path).unwrap();
    let historical = fixture.app.migration_status(&fixture.old).unwrap().unwrap();
    assert_eq!(historical.state(), MigrationState::ReadyWithOmissions);
    assert_eq!(historical.scope(), &MigrationScope::All);
    assert!(fixture
        .app
        .migration_status(&fixture.old.join("absent"))
        .unwrap()
        .is_none());
}

#[test]
fn invalid_scope_and_case_aliases_refuse_before_copy() {
    for path in [
        "",
        "../escape",
        "/absolute",
        "stream:payload",
        "folder/./file",
        rightkit_daemon::MIGRATION_STATE,
        ".writer.lock/child",
    ] {
        assert!(
            MigrateOptions::strict()
                .include_roots(vec![PathBuf::from(path)])
                .is_err(),
            "{path}"
        );
    }
    assert!(MigrateOptions::strict()
        .include_roots(vec![
            PathBuf::from("webview2/a"),
            PathBuf::from("WebView2/b")
        ])
        .is_err());
    let fixture = Fixture::new();
    let selected = MigrateOptions::strict()
        .include_roots(vec![PathBuf::from("WebView2")])
        .unwrap();
    assert!(fixture
        .app
        .migrate_from_with(&fixture.old, &selected)
        .is_err());
    assert!(!fixture.app.root().join("webview2").exists());
}

#[test]
fn reserved_and_participant_owned_roots_refuse_selected_copy() {
    struct Owner;
    impl CutoverParticipant for Owner {
        fn name(&self) -> &str {
            "opaque-db"
        }
        fn owned_paths(&self) -> Vec<PathBuf> {
            vec![PathBuf::from("mailright.sqlite")]
        }
    }
    for root in [
        CUTOVER_RECORD,
        ".rktxn",
        ".rightkit-cutover.lock",
        "mailright.sqlite",
        "mailright.sqlite/child",
    ] {
        let fixture = Fixture::new();
        let result = RootCutover::new(fixture.app.clone(), &fixture.old)
            .fs_move(FsMove::Tree(MigrateOptions::strict()))
            .participant(Owner)
            .tree_roots(move |_| Ok(vec![PathBuf::from(root)]))
            .run();
        assert!(result.is_err(), "{root}");
        assert!(!fixture.app.root().join("mailright.sqlite").exists());
        assert_eq!(
            RootCutover::selected_root(&fixture.app, &fixture.old).unwrap(),
            fixture.old
        );

        // Static descriptors must refuse before persisting a forbidden scope. A corrected
        // attempt must not union the rejected scope back into its next generation.
        let fixture = Fixture::new();
        let options = MigrateOptions::strict()
            .include_roots(vec![PathBuf::from(root)])
            .unwrap();
        let result = RootCutover::new(fixture.app.clone(), &fixture.old)
            .fs_move(FsMove::Tree(options))
            .participant(Owner)
            .run();
        assert!(result.is_err(), "static {root}");
        assert!(
            !fixture.app.root().join(CUTOVER_RECORD).exists(),
            "static {root}"
        );
        assert_eq!(
            RootCutover::selected_root(&fixture.app, &fixture.old).unwrap(),
            fixture.old
        );
        let mut corrected = RootCutover::new(fixture.app.clone(), &fixture.old)
            .fs_move(FsMove::Tree(
                MigrateOptions::strict()
                    .include_roots(vec![PathBuf::from("startup.log")])
                    .unwrap(),
            ))
            .participant(Owner);
        corrected.recover().unwrap();
        corrected.run().unwrap();
        assert!(fixture.app.root().join("startup.log").is_file());
    }
}

#[test]
fn failed_resolver_does_not_write_abort_into_substituted_destination() {
    let fixture = Fixture::new();
    let new = fixture.app.root().to_path_buf();
    let replacement = new.clone();
    let result = RootCutover::new(fixture.app.clone(), &fixture.old)
        .fs_move(FsMove::Tree(MigrateOptions::strict()))
        .tree_roots(move |_| {
            fs::rename(&replacement, replacement.with_extension("saved")).unwrap();
            fs::create_dir(&replacement).unwrap();
            fs::write(replacement.join("sentinel"), b"replacement").unwrap();
            Err(ParticipantError::new("resolver failed after replacement"))
        })
        .run();
    assert!(result.is_err());
    assert!(!new.join(CUTOVER_RECORD).exists());
    assert_eq!(fs::read(new.join("sentinel")).unwrap(), b"replacement");
    assert!(new.with_extension("saved").join(CUTOVER_RECORD).exists());
}

#[test]
fn switched_recovery_uses_frozen_selection_without_resolving_again() {
    let fixture = Fixture::new();
    RootCutover::new(fixture.app.clone(), &fixture.old)
        .fs_move(FsMove::Tree(MigrateOptions::strict()))
        .tree_roots(|_| Ok(vec![PathBuf::from("startup.log")]))
        .run()
        .unwrap();
    let path = fixture.app.root().join(CUTOVER_RECORD);
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    // Model a durable switch before completion; copied bytes & frozen descriptor already exist.
    record["phase"] = serde_json::json!("switched");
    record["settlement_complete"] = serde_json::json!(false);
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    RootCutover::new(fixture.app.clone(), &fixture.old)
        .fs_move(FsMove::Tree(MigrateOptions::strict()))
        .tree_roots(|_| panic!("switched recovery must never enumerate companions"))
        .recover()
        .unwrap();
    assert!(fixture.app.root().join("startup.log").is_file());
    assert!(!fixture.app.root().join("MailRight.exe").exists());
}

struct Interrupted {
    old: PathBuf,
    new: PathBuf,
    substitute: bool,
}
impl CutoverParticipant for Interrupted {
    fn name(&self) -> &str {
        "interrupt"
    }
    fn relocate(&mut self, _: &CutoverContext) -> Result<(), ParticipantError> {
        if self.substitute {
            fs::rename(&self.old, self.old.with_extension("saved")).unwrap();
            fs::create_dir(&self.old).unwrap();
        } else {
            // Same bytes model a copy published before an interrupted attempt.
            fs::copy(
                self.old.join("mailright.sqlite.pre-v7.bak"),
                self.new.join("mailright.sqlite.pre-v7.bak"),
            )
            .unwrap();
            fs::remove_file(self.old.join("mailright.sqlite.pre-v7.bak")).unwrap();
        }
        Err(ParticipantError::new("stop before shared copy"))
    }
    fn settle(&mut self, _: &CutoverContext, _: Settlement) -> Result<(), ParticipantError> {
        Ok(())
    }
}

#[test]
fn retry_union_retains_disappeared_backup_and_refuses_destination_orphan() {
    let fixture = Fixture::new();
    let participant = Interrupted {
        old: fixture.old.clone(),
        new: fixture.app.root().to_path_buf(),
        substitute: false,
    };
    let mut first = RootCutover::new(fixture.app.clone(), &fixture.old)
        .fs_move(FsMove::Tree(MigrateOptions::strict()))
        .tree_roots(|cx| Ok(roots(cx.old_root())))
        .participant(participant);
    assert!(first.run().is_err());
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.app.root().join(CUTOVER_RECORD)).unwrap())
            .unwrap();
    assert!(record["tree_selection"]["roots"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "mailright.sqlite.pre-v7.bak"));
    // Fresh participant has same identity but no side effects; dropped source name cannot shrink scope.
    struct Quiet;
    impl CutoverParticipant for Quiet {
        fn name(&self) -> &str {
            "interrupt"
        }
    }
    let result = RootCutover::new(fixture.app.clone(), &fixture.old)
        .fs_move(FsMove::Tree(MigrateOptions::strict()))
        .tree_roots(|cx| Ok(roots(cx.old_root())))
        .participant(Quiet)
        .run();
    assert!(matches!(
        result,
        Err(rightkit_daemon::CutoverError::Migrate(
            rightkit_daemon::AppRootError::Collision(_)
        ))
    ));
    assert!(fixture
        .app
        .root()
        .join("mailright.sqlite.pre-v7.bak")
        .exists());
}

#[test]
fn resolver_path_substitution_aborts_before_relocate_or_copy() {
    let fixture = Fixture::new();
    let old = fixture.old.clone();
    let result = RootCutover::new(fixture.app.clone(), &fixture.old)
        .fs_move(FsMove::Tree(MigrateOptions::strict()))
        .tree_roots(move |_| {
            fs::rename(&old, old.with_extension("saved")).unwrap();
            fs::create_dir(&old).unwrap();
            Ok(vec![PathBuf::from("startup.log")])
        })
        .run();
    assert!(matches!(
        result,
        Err(rightkit_daemon::CutoverError::PathSubstituted(_))
    ));
    assert!(!fixture.app.root().join("startup.log").exists());
}

#[test]
fn differing_selected_file_preserves_destination_and_never_switches() {
    let fixture = Fixture::new();
    fs::write(fixture.app.root().join("startup.log"), b"contender").unwrap();
    let result = RootCutover::new(fixture.app.clone(), &fixture.old)
        .fs_move(FsMove::Tree(MigrateOptions::strict()))
        .tree_roots(|_| Ok(vec![PathBuf::from("startup.log")]))
        .run();
    assert!(matches!(
        result,
        Err(rightkit_daemon::CutoverError::Migrate(
            rightkit_daemon::AppRootError::Collision(_)
        ))
    ));
    assert_eq!(
        fs::read(fixture.app.root().join("startup.log")).unwrap(),
        b"contender"
    );
    assert_eq!(
        RootCutover::selected_root(&fixture.app, &fixture.old).unwrap(),
        fixture.old
    );
}

#[test]
fn selection_is_durable_before_relocate_and_resolver_failure_aborts_prepared() {
    use std::sync::{Arc, Mutex};
    struct Probe {
        log: Arc<Mutex<Vec<&'static str>>>,
    }
    impl CutoverParticipant for Probe {
        fn name(&self) -> &str {
            "probe"
        }
        fn prepare(&mut self, _: &CutoverContext) -> Result<(), ParticipantError> {
            self.log.lock().unwrap().push("prepare");
            Ok(())
        }
        fn quiesce(&mut self, _: &CutoverContext) -> Result<(), ParticipantError> {
            self.log.lock().unwrap().push("quiesce");
            Ok(())
        }
        fn relocate(&mut self, cx: &CutoverContext) -> Result<(), ParticipantError> {
            let record: serde_json::Value =
                serde_json::from_slice(&fs::read(cx.new_root().join(CUTOVER_RECORD)).unwrap())
                    .unwrap();
            assert_eq!(
                record["tree_selection"]["roots"],
                serde_json::json!(["startup.log"])
            );
            self.log.lock().unwrap().push("relocate");
            Ok(())
        }
        fn settle(
            &mut self,
            _: &CutoverContext,
            settlement: Settlement,
        ) -> Result<(), ParticipantError> {
            self.log
                .lock()
                .unwrap()
                .push(if matches!(settlement, Settlement::Aborted) {
                    "abort"
                } else {
                    "commit"
                });
            Ok(())
        }
    }
    let fixture = Fixture::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let callback_log = log.clone();
    RootCutover::new(fixture.app.clone(), &fixture.old)
        .fs_move(FsMove::Tree(MigrateOptions::strict()))
        .participant(Probe { log: log.clone() })
        .tree_roots(move |_| {
            callback_log.lock().unwrap().push("resolve");
            Ok(vec![PathBuf::from("startup.log")])
        })
        .run()
        .unwrap();
    assert_eq!(
        *log.lock().unwrap(),
        ["prepare", "quiesce", "resolve", "relocate", "commit"]
    );
    let other = Fixture::new();
    log.lock().unwrap().clear();
    let result = RootCutover::new(other.app.clone(), &other.old)
        .fs_move(FsMove::Tree(MigrateOptions::strict()))
        .participant(Probe { log: log.clone() })
        .tree_roots(|_| Err(ParticipantError::new("inventory unavailable")))
        .run();
    assert!(result.is_err());
    assert_eq!(*log.lock().unwrap(), ["prepare", "quiesce", "abort"]);
}

#[cfg(unix)]
#[test]
fn selected_link_refused_while_unlisted_link_is_not_traversed() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    symlink("missing", fixture.old.join("unlisted-link")).unwrap();
    let selected = MigrateOptions::strict()
        .include_roots(vec![PathBuf::from("webview2")])
        .unwrap();
    fixture
        .app
        .migrate_from_with(&fixture.old, &selected)
        .unwrap();
    symlink("../../MailRight.exe", fixture.old.join("webview2/link")).unwrap();
    assert!(fixture
        .app
        .migrate_from_with(&fixture.old, &selected)
        .is_err());
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let old = temporary.path().join("installation");
        fs::create_dir_all(&old).unwrap();
        for directory in DIRECTORIES {
            fs::create_dir_all(old.join(directory).join("empty")).unwrap();
            fs::write(old.join(directory).join("data.bin"), directory.as_bytes()).unwrap();
        }
        for log in LOGS {
            fs::write(old.join(log), log.as_bytes()).unwrap();
        }
        for file in [
            "mailright.sqlite.pre-v7.bak",
            "mailright.sqlite.pre-v20.bak",
        ] {
            fs::write(old.join(file), file.as_bytes()).unwrap();
        }
        // These siblings represent installation payload & names outside app's exact allowlist.
        for file in [
            "MailRight.exe",
            "uninstall.exe",
            "mailright.sqlite",
            "startup.log.2",
            "mailright.sqlite.pre-v20.bak.tmp",
            "unrelated.pre-v20.bak",
        ] {
            fs::write(old.join(file), b"must remain only at old root").unwrap();
        }
        fs::create_dir_all(old.join("resources").join("empty")).unwrap();
        fs::write(
            old.join("resources").join("payload.bin"),
            b"installation payload",
        )
        .unwrap();
        let app = Suite::at(temporary.path().join("suite"))
            .unwrap()
            .app_root("SelectedCompanions")
            .unwrap();
        Self {
            _temporary: temporary,
            old,
            app,
        }
    }
}
