// rightkit-daemon 0.4.0 registry consumer, outside the workspace, pinning `rightkit-daemon =0.4.0`
// exactly with default features only (the `local` marker). It avoids `remote`, `remote-loopback`,
// `link`, `relay` and `test-mode`.
//
// Proves the PK07-3 root-cutover surface an app uses to move its data root: `Suite::per_run_temp`
// and `Suite::app_root`, `RootCutover::{new, fs_move, participant, run, status, selected_root,
// recover, retire_old_root}`, `FsMove::Tree` with `MigrateOptions::strict`, a no-op
// `CutoverParticipant` that keeps the default hooks, a refused relative old root (`InvalidRoot`),
// the completed rerun, the completed recovery, the retirement of the old root, `valid_app_name`,
// and the record file name.
// Also proves frozen PK07 R2 example at 0.4.0: `Service::builder().methods(..).handler(..)
// .local(..).start()` + `Client::connect_expecting` (host = this process), echo round-trip,
// executable-mismatch refusal before any request & unlisted-app refusal.
// Does not prove: `FsMove::Companions`, participant failure, abort or panic paths, path
// substitution, the db relocation participant, approvals (PK07-4), separate-process host,
// takeover across processes, or any non-default feature.
// State: per-run private temp suites, removed on drop. No network/TCP, no real user data.
use rightkit_daemon::contract::{MethodPolicy, MethodTable};
use rightkit_daemon::{
    valid_app_name, Allowlist, CallContext, Client, ClientConfig, ConnectError, CutoverError,
    CutoverParticipant, CutoverPhase, Emit, FsMove, Handler, HostExpectation,
    HostIdentityReason, HostOptions, MigrateOptions, RecoverOutcome, RetireMode, RootCutover,
    RootSelection, Service, ServiceConfig, ServiceError, Suite, CUTOVER_RECORD,
    ERROR_NOT_ALLOWED, ERROR_RATE_LIMITED,
};
use serde_json::{json, Value};
use std::error::Error;
use std::fs;
use std::sync::Arc;

struct Echo;

impl Handler for Echo {
    fn call(
        &self,
        cx: &CallContext,
        method: &str,
        params: &Value,
        _emit: &Emit,
    ) -> Result<Value, ServiceError> {
        match method {
            "echo" => Ok(json!({ "echo": params, "app": cx.app() })),
            _ => Err(ServiceError::new("unknown_method", method)),
        }
    }
}

fn service_r2() -> Result<(), Box<dyn Error>> {
    let suite = Suite::per_run_temp("consumer-svc")?;
    let table = MethodTable::new().method("echo", MethodPolicy::local(["alpha"]));
    let service = Service::builder()
        .methods(table)
        .handler(Arc::new(Echo))
        .local(
            &suite,
            ServiceConfig::new("rkconsumer", "test", 5, "0.0.0", Allowlist::new()),
            HostOptions::new(),
        )
        .start()?;
    assert!(service.local().is_some());
    assert!(service.methods().get("echo").is_some());

    let config = ClientConfig::new("rkconsumer", "alpha").suite(suite.clone());
    let me = std::env::current_exe()?;
    let client = Client::connect_expecting(&config, &HostExpectation::executable(&me))
        .unwrap_or_else(|e| panic!("expected host must be accepted: {e}"));
    let verified = client.verified_host().expect("verified host recorded");
    assert_eq!(verified.pid, std::process::id());
    assert_eq!(
        client.call("echo", json!({ "n": 1 }))?,
        json!({ "echo": { "n": 1 }, "app": "alpha" })
    );
    assert_eq!(
        client.call("not_in_table", json!({})).expect_err("method outside table").code,
        ERROR_NOT_ALLOWED
    );
    let other = suite.root().join("some-other-host-binary");
    match Client::connect_expecting(&config, &HostExpectation::executable(&other)) {
        Err(ConnectError::HostIdentity(e)) => {
            assert_eq!(e.reason, HostIdentityReason::ExecutableMismatch);
            assert_eq!(e.found_pid, Some(std::process::id()));
        }
        other => panic!("different executable must be refused, got {:?}", other.map(|_| ())),
    }
    let ghost = Client::connect(&suite, "rkconsumer", "ghost").err().expect("unlisted app");
    assert_eq!(ghost.code, ERROR_NOT_ALLOWED);
    assert_eq!(ERROR_RATE_LIMITED, "rate_limited");
    drop(client);
    service.shutdown();
    Ok(())
}

struct NoOp;

impl CutoverParticipant for NoOp {
    fn name(&self) -> &str {
        "noop"
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    assert!(valid_app_name("CutoverApp"));
    assert!(!valid_app_name("../escape"));
    assert_eq!(CUTOVER_RECORD, ".rightkit-root-cutover.json");

    let suite = Suite::per_run_temp("consumer")?;
    let app = suite.app_root("CutoverApp")?;
    let old = suite.root().join("old");
    fs::create_dir_all(old.join("sub"))?;
    fs::write(old.join("a.txt"), b"alpha")?;
    fs::write(old.join("sub").join("b.txt"), b"beta")?;
    assert!(RootCutover::status(&app)?.is_none(), "no record before the first cutover");

    // A relative old root is refused before anything is copied or switched.
    let mut refused = RootCutover::new(app.clone(), "relative-old");
    assert!(
        matches!(refused.run(), Err(CutoverError::InvalidRoot { .. })),
        "a relative old root must be refused with InvalidRoot"
    );

    let mut cutover = RootCutover::new(app.clone(), old.clone())
        .fs_move(FsMove::Tree(MigrateOptions::strict()))
        .participant(NoOp);
    let report = cutover.run()?;
    assert_eq!(report.selected, RootSelection::New);
    assert_eq!(report.generation, 1);
    assert!(!report.resumed, "the first run is not a resume");
    assert!(!report.already_completed, "the first run is not a rerun");
    let migration = report.migration.as_ref().expect("a tree move reports its migration");
    assert!(migration.complete(), "the tree migration completed");
    assert_eq!(fs::read(app.root().join("a.txt"))?, b"alpha");
    assert_eq!(fs::read(app.root().join("sub").join("b.txt"))?, b"beta");
    assert_eq!(fs::read(old.join("a.txt"))?, b"alpha", "the old root stays complete");

    let status = RootCutover::status(&app)?.expect("a record exists after the cutover");
    assert_eq!(status.phase, CutoverPhase::Completed);
    assert_eq!(status.generation, 1);
    assert_eq!(status.selected, RootSelection::New);
    assert!(status.settlement_complete, "the no-op participant settled");
    assert_eq!(status.participants, vec!["noop".to_string()]);
    assert_eq!(RootCutover::selected_root(&app, &old)?, status.new_root);

    // A completed cutover reruns as a no-op, and recovery of a completed cutover is a no-op too.
    let again = cutover.run()?;
    assert!(again.already_completed, "a completed cutover reruns as a no-op");
    let mut recovering = RootCutover::new(app.clone(), old.clone()).participant(NoOp);
    assert!(
        matches!(recovering.recover()?, RecoverOutcome::Completed),
        "recovery of a completed cutover reports Completed"
    );

    // Retiring the old root renames it beside the original; a second call reports it as done.
    let retired = RootCutover::retire_old_root(&app, RetireMode::RenameAside)?;
    assert!(!retired.already, "the first retirement is not a repeat");
    assert!(!old.exists(), "the old root was renamed aside");
    assert_eq!(fs::read(retired.path.join("a.txt"))?, b"alpha");
    let repeat = RootCutover::retire_old_root(&app, RetireMode::RenameAside)?;
    assert!(repeat.already, "a second retirement reports the retired root");

    service_r2()?;

    println!("OK rightkit-daemon 0.4.0 (registry consumer, PK07-3 root cutover + PK07 R2 service)");
    Ok(())
}
