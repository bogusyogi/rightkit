//! C ABI coverage (`ffi` feature): drives the `extern "C"` entry points the way a Swift host
//! would, against the real scriptable `engine_fixture` child and a real `rightkit-service` host.
//! The ABI keeps process-global state (factory, registered snapshot, handle table), so every case
//! takes `SERIAL`.

use rightkit_jobs::ffi::*;
use rightkit_jobs::SupervisorConfig;
use rightkit_process::OwnedCommand;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

static SERIAL: Mutex<()> = Mutex::new(());

fn fixture() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let examples = exe.parent().unwrap().parent().unwrap().join("examples");
    let exact = if cfg!(windows) {
        "engine_fixture.exe"
    } else {
        "engine_fixture"
    };
    if examples.join(exact).is_file() {
        return examples.join(exact);
    }
    std::fs::read_dir(&examples)
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let binary =
                cfg!(windows) && name.ends_with(".exe") || !cfg!(windows) && !name.contains('.');
            name.starts_with("engine_fixture-") && binary && entry.path().is_file()
        })
        .map(|entry| entry.path())
        .unwrap_or_else(|| examples.join(exact))
}

struct Fx {
    dir: PathBuf,
    plan: String,
}

impl HostFactory for Fx {
    fn supervisor_config(&self) -> SupervisorConfig {
        let (dir, plan) = (self.dir.clone(), self.plan.clone());
        SupervisorConfig::new("ffi-fixture", move || {
            let mut c = OwnedCommand::new(fixture());
            c.command_mut().env("FX_DIR", &dir).env("FX_PLAN", &plan);
            Ok(c)
        })
        .with_hello_timeout(Duration::from_secs(10))
        .with_ready_wait(Duration::from_secs(3))
        .with_shutdown_grace(Duration::from_millis(200))
    }
    fn snapshot_method(&self) -> String {
        "set_init".into()
    }
    fn start_budget(&self) -> Duration {
        Duration::from_secs(20)
    }
    fn connect_service(
        &self,
        _timeout: Duration,
    ) -> Result<rightkit_service::Client, rightkit_service::ServiceError> {
        Err(rightkit_service::ServiceError::new(
            "service_unavailable",
            "no service in this fixture",
        ))
    }
}

/// Take the lock, install a factory for `plan`, and clear any registered snapshot.
fn rig(plan: &str) -> (MutexGuard<'static, ()>, tempfile::TempDir) {
    let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    install_factory(Arc::new(Fx {
        dir: dir.path().to_path_buf(),
        plan: plan.into(),
    }));
    reset_registration();
    (guard, dir)
}

fn empty_err() -> RkpError {
    RkpError {
        code: 0,
        message: RkpBuf {
            ptr: std::ptr::null_mut(),
            len: 0,
        },
    }
}

fn take(buf: &mut RkpBuf) -> Vec<u8> {
    let v = if buf.ptr.is_null() {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(buf.ptr, buf.len) }.to_vec()
    };
    unsafe { rkp_buf_free(buf) };
    v
}

fn register(json: &str, generation: u64) -> i32 {
    let mut err = empty_err();
    let code = unsafe { rkp_register_snapshot(json.as_ptr(), json.len(), generation, &mut err) };
    take(&mut err.message);
    code
}

fn start() -> Result<u64, i32> {
    let mut host = 0u64;
    let mut err = empty_err();
    let code = unsafe { rkp_host_start(&mut host, &mut err) };
    take(&mut err.message);
    if code == RKP_OK {
        Ok(host)
    } else {
        Err(code)
    }
}

struct Events {
    count: AtomicUsize,
    bytes: Mutex<Vec<Vec<u8>>>,
}

unsafe extern "C" fn on_event(ctx: *mut c_void, ptr: *const u8, len: usize) {
    let ev = &*(ctx as *const Events);
    ev.count.fetch_add(1, Ordering::SeqCst);
    ev.bytes
        .lock()
        .unwrap()
        .push(std::slice::from_raw_parts(ptr, len).to_vec());
}

fn call(
    host: u64,
    method: &str,
    params: &str,
    resume: u32,
    timeout_ms: u64,
    events: Option<&Events>,
) -> Result<Vec<u8>, i32> {
    let mut out = RkpBuf {
        ptr: std::ptr::null_mut(),
        len: 0,
    };
    let mut err = empty_err();
    let (cb, ctx) = match events {
        Some(e) => (
            Some(on_event as RkpEventFn),
            e as *const Events as *mut c_void,
        ),
        None => (None, std::ptr::null_mut()),
    };
    let code = unsafe {
        rkp_host_call(
            host,
            method.as_ptr(),
            method.len(),
            params.as_ptr(),
            params.len(),
            resume,
            timeout_ms,
            cb,
            ctx,
            &mut out,
            &mut err,
        )
    };
    take(&mut err.message);
    let body = take(&mut out);
    if code == RKP_OK {
        Ok(body)
    } else {
        Err(code)
    }
}

fn snapshot(host: u64) -> Result<(u64, u32, serde_json::Value), i32> {
    let (mut generation, mut lifecycle) = (0u64, 0u32);
    let mut payload = RkpBuf {
        ptr: std::ptr::null_mut(),
        len: 0,
    };
    let mut err = empty_err();
    let code = unsafe {
        rkp_host_snapshot(
            host,
            &mut generation,
            &mut lifecycle,
            &mut payload,
            &mut err,
        )
    };
    take(&mut err.message);
    let body = take(&mut payload);
    if code == RKP_OK {
        Ok((
            generation,
            lifecycle,
            serde_json::from_slice(&body).unwrap(),
        ))
    } else {
        Err(code)
    }
}

fn shutdown(host: u64, budget_ms: u64) -> (u32, u64) {
    let (mut outcome, mut unreaped) = (99u32, 99u64);
    let mut err = empty_err();
    let code = unsafe { rkp_host_shutdown(host, budget_ms, &mut outcome, &mut unreaped, &mut err) };
    take(&mut err.message);
    assert_eq!(code, RKP_OK);
    (outcome, unreaped)
}

#[test]
fn versions_match_the_contract_and_schema() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut v = RkpVersions {
        abi: 0,
        contract: 0,
        schema: 0,
    };
    assert_eq!(unsafe { rkp_versions(&mut v) }, RKP_OK);
    assert_eq!(
        v,
        RkpVersions {
            abi: ABI_VERSION,
            contract: rightkit_jobs::CONTRACT_VERSION,
            schema: SCHEMA_VERSION,
        }
    );
    assert_eq!(
        unsafe { rkp_versions(std::ptr::null_mut()) },
        RKP_INVALID_ARGUMENT
    );
}

#[test]
fn snapshot_is_registered_before_start_and_replayed_into_the_engine() {
    let (_g, _d) = rig("ok");
    assert_eq!(
        start().unwrap_err(),
        RKP_NOT_READY,
        "no snapshot registered yet"
    );
    assert_eq!(register("{not json", 1), RKP_INVALID_ARGUMENT);
    assert_eq!(register(r#"{"theme":"dark"}"#, 7), RKP_OK);
    let host = start().unwrap();
    let (generation, lifecycle, payload) = snapshot(host).unwrap();
    assert_eq!((generation, lifecycle), (7, RKP_READY));
    assert_eq!(payload["fenced"], false);
    // The fixture remembers `set_*` calls: the registered snapshot reached the engine.
    let state: serde_json::Value =
        serde_json::from_slice(&call(host, "get_state", "{}", RKP_RESUME, 5000, None).unwrap())
            .unwrap();
    assert_eq!(state["set_init"], serde_json::json!({"theme":"dark"}));
    assert_eq!(shutdown(host, 5000).0, RKP_SHUTDOWN_EXITED);
    rkp_release_host(host);
}

#[test]
fn events_are_delivered_as_borrowed_bytes_and_stop_at_return() {
    let (_g, _d) = rig("ok");
    register("{}", 1);
    let host = start().unwrap();
    let ev = Events {
        count: AtomicUsize::new(0),
        bytes: Mutex::new(Vec::new()),
    };
    let out = call(host, "progress", r#"{"n":3}"#, RKP_RESUME, 5000, Some(&ev)).unwrap();
    assert!(!out.is_empty());
    assert_eq!(ev.count.load(Ordering::SeqCst), 3);
    let first: serde_json::Value = serde_json::from_slice(&ev.bytes.lock().unwrap()[0]).unwrap();
    assert_eq!(first["i"], 0);
    // Nothing arrives after the call returned.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(ev.count.load(Ordering::SeqCst), 3);
    shutdown(host, 5000);
    rkp_release_host(host);
}

#[test]
fn a_deadline_returns_one_terminal_and_resume_keeps_the_engine() {
    let (_g, _d) = rig("ok");
    register("{}", 1);
    let host = start().unwrap();
    let t = Instant::now();
    assert_eq!(
        call(host, "hang", "{}", RKP_RESUME, 300, None).unwrap_err(),
        RKP_DEADLINE
    );
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    // Resume: only the wait was abandoned; the same engine still answers.
    let (_, lifecycle, _) = snapshot(host).unwrap();
    assert_eq!(lifecycle, RKP_READY);
    assert!(call(host, "ping", "{}", RKP_RESUME, 5000, None).is_ok());
    shutdown(host, 5000);
    rkp_release_host(host);
}

#[test]
fn never_policy_treats_a_timeout_as_a_wedge_and_restarts() {
    let (_g, _d) = rig("ok");
    register("{}", 1);
    let host = start().unwrap();
    assert_eq!(
        call(host, "hang", "{}", RKP_NEVER, 300, None).unwrap_err(),
        RKP_DEADLINE
    );
    // The wedged engine was killed; the supervisor boots a replacement and replays the snapshot.
    let until = Instant::now() + Duration::from_secs(10);
    let mut generation = 0u64;
    while Instant::now() < until {
        if let Ok((_, RKP_READY, p)) = snapshot(host) {
            generation = p["engine_generation"].as_u64().unwrap();
            if generation >= 2 {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(generation >= 2, "engine was not replaced: {generation}");
    shutdown(host, 5000);
    rkp_release_host(host);
}

#[test]
fn generation_fencing_refuses_stale_registrations_hosts_and_handles() {
    let (_g, _d) = rig("ok");
    assert_eq!(register("{}", 5), RKP_OK);
    assert_eq!(register("{}", 4), RKP_STALE_GENERATION, "older generation");
    assert_eq!(register("{}", 5), RKP_OK, "same generation, same bytes");
    assert_eq!(
        register(r#"{"x":1}"#, 5),
        RKP_STALE_GENERATION,
        "same generation, new bytes"
    );
    let host = start().unwrap();
    assert_eq!(snapshot(host).unwrap().1, RKP_READY);
    // A newer snapshot is registered: this host's engine is out of date until replaced.
    assert_eq!(register(r#"{"x":2}"#, 6), RKP_OK);
    let (generation, lifecycle, payload) = snapshot(host).unwrap();
    assert_eq!((generation, lifecycle), (5, RKP_RECOVERING));
    assert_eq!(payload["fenced"], true);
    assert_eq!(
        call(host, "ping", "{}", RKP_RESUME, 2000, None).unwrap_err(),
        RKP_STALE_GENERATION
    );
    // A replacement host starts at the new generation.
    let next = start().unwrap();
    assert_ne!(next, host, "handle ids are never reused");
    assert_eq!(snapshot(next).unwrap().0, 6);
    for h in [host, next] {
        shutdown(h, 5000);
        rkp_release_host(h);
        rkp_release_host(h); // idempotent
        assert_eq!(snapshot(h).unwrap_err(), RKP_STALE_HANDLE);
    }
}

#[test]
fn shutdown_is_single_refuses_new_work_and_reports_exited() {
    let (_g, _d) = rig("ok");
    register("{}", 1);
    let host = start().unwrap();
    let (outcome, unreaped) = shutdown(host, 5000);
    assert_eq!((outcome, unreaped), (RKP_SHUTDOWN_EXITED, 0));
    assert_eq!(shutdown(host, 5000).0, RKP_SHUTDOWN_ALREADY);
    assert_eq!(
        call(host, "ping", "{}", RKP_RESUME, 1000, None).unwrap_err(),
        RKP_SHUTTING_DOWN
    );
    rkp_release_host(host);
    assert_eq!(retained_count(), 0);
}

#[test]
fn a_shutdown_past_its_budget_is_unconfirmed_and_reaped_in_the_background() {
    let (_g, _d) = rig("wedge:1500");
    register("{}", 1);
    let host = start().unwrap();
    // After 1.5 s the engine stops reading; the graceful request then times out and the tree is
    // killed, which takes longer than this budget.
    std::thread::sleep(Duration::from_millis(1700));
    let (outcome, unreaped) = shutdown(host, 20);
    assert_eq!(outcome, RKP_SHUTDOWN_UNCONFIRMED);
    assert!(unreaped >= 1, "unconfirmed carries the unreaped count");
    // Releasing keeps the host counted until the reaper confirms the process gone, then it drops.
    rkp_release_host(host);
    let until = Instant::now() + Duration::from_secs(15);
    while retained_count() > 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(retained_count(), 0, "reaper confirmed the child gone");
}

#[test]
fn unconfirmed_hosts_cannot_accumulate_past_the_cap() {
    let (_g, _d) = rig("ok");
    register("{}", 1);
    let mut hosts = Vec::new();
    for _ in 0..MAX_HOSTS {
        hosts.push(start().unwrap());
    }
    assert_eq!(start().unwrap_err(), RKP_TOO_MANY_HOSTS);
    for h in hosts {
        shutdown(h, 5000);
        rkp_release_host(h);
    }
    let again = start().unwrap();
    shutdown(again, 5000);
    rkp_release_host(again);
}

#[test]
fn bad_arguments_are_refused_not_dereferenced() {
    let (_g, _d) = rig("ok");
    register("{}", 1);
    let host = start().unwrap();
    assert_eq!(
        call(host, "ping", "{not json", RKP_RESUME, 1000, None).unwrap_err(),
        RKP_INVALID_ARGUMENT
    );
    assert_eq!(
        call(host, "ping", "{}", 9, 1000, None).unwrap_err(),
        RKP_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe {
            rkp_host_call(
                host,
                std::ptr::null(),
                4,
                std::ptr::null(),
                0,
                RKP_RESUME,
                1000,
                None,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        RKP_INVALID_ARGUMENT
    );
    // An unknown service handle and an unconfigured connect are typed refusals.
    let mut out = RkpBuf {
        ptr: std::ptr::null_mut(),
        len: 0,
    };
    let code = unsafe {
        rkp_service_call(
            424242,
            b"x".as_ptr(),
            1,
            std::ptr::null(),
            0,
            1000,
            &mut out,
            std::ptr::null_mut(),
        )
    };
    assert_eq!(code, RKP_STALE_HANDLE);
    let mut conn = 0u64;
    let mut err = empty_err();
    let code = unsafe { rkp_service_connect(host, 1000, &mut conn, &mut err) };
    assert_eq!(code, RKP_NATIVE, "factory refused the connect");
    take(&mut err.message);
    shutdown(host, 5000);
    rkp_release_host(host);
}
