//! `rightkit-engine-native`: `rightkit-job-queue`'s C ABI (`ffi` feature) as one static library
//! for a Swift host, plus one configuration function, `rke_install_engine`.
//!
//! The crate ships no header of its own: `Sources/CRightKitEngine/include/rightkit_engine.h`
//! is transcribed from `rightkit-job-queue` 0.3.1's `ffi.rs` (`ABI_VERSION` 1). The `rkp_*`
//! functions are the upstream ones, re-exported here so they are certain to be in the archive;
//! nothing wraps or alters them.
//!
//! The ABI leaves the engine command, the snapshot method and the start budget to the embedding
//! library (`HostFactory`, installed with `install_factory`). `rke_install_engine` is that
//! embedding: the app passes the executable path, its arguments, the extra environment variable
//! names to pass through, the snapshot method and the start budget. The crate itself knows
//! nothing about any particular engine.

use std::ffi::{c_char, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rightkit_job_queue::ffi as rkp;
use rightkit_job_queue::{OwnedCommand, SupervisorConfig};

pub use rkp::{
    rkp_buf_free, rkp_host_call, rkp_host_lifecycle, rkp_host_shutdown, rkp_host_snapshot,
    rkp_host_start, rkp_register_snapshot, rkp_release_connection, rkp_release_host,
    rkp_service_call, rkp_service_connect, rkp_versions, RkpBuf, RkpError, RkpEventFn, RkpVersions,
};

/// Most arguments or environment names `rke_install_engine` accepts.
const MAX_LIST: usize = 4096;
/// Start budget when the caller passes zero (the ABI's own default).
const DEFAULT_START_BUDGET: Duration = Duration::from_secs(60);
/// Grace for the engine's own `shutdown` request, and for reaping, before it is killed.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// Spawns exactly one executable. The environment is scrubbed: only the supervisor's default
/// allowlist (PATH, HOME, USER, LANG, TMPDIR, USERPROFILE, LOCALAPPDATA and the like) plus the
/// names the app listed reach the engine. The engine must advertise the current wire contract.
struct EngineFactory {
    path: PathBuf,
    args: Vec<String>,
    env_keys: Vec<String>,
    snapshot_method: String,
    start_budget: Duration,
}

impl rkp::HostFactory for EngineFactory {
    fn supervisor_config(&self) -> SupervisorConfig {
        let (path, args, env_keys) = (self.path.clone(), self.args.clone(), self.env_keys.clone());
        SupervisorConfig::new("rightkit-engine", move || {
            let mut command = OwnedCommand::new(&path);
            command.command_mut().args(&args);
            command.env_allowlist_with_defaults(env_keys.iter());
            command.windows_hide();
            Ok(command)
        })
        .require_scrubbed_env()
        .require_current_contract_version()
        .with_mandatory_contract_check()
        .with_shutdown_grace(SHUTDOWN_GRACE)
    }

    fn snapshot_method(&self) -> String {
        self.snapshot_method.clone()
    }

    fn start_budget(&self) -> Duration {
        self.start_budget
    }

    /// A supervised engine is a stdio child; it serves no `rightkit-daemon` endpoint.
    fn connect_service(
        &self,
        _timeout: Duration,
    ) -> Result<rightkit_daemon::Client, rightkit_daemon::ServiceError> {
        Err(rightkit_daemon::ServiceError::new(
            "service_unavailable",
            "a supervised engine serves no daemon endpoint",
        ))
    }
}

/// Same layout and allocator contract as the ABI's own buffers, so `rkp_buf_free` frees it.
fn make_buf(bytes: Vec<u8>) -> RkpBuf {
    if bytes.is_empty() {
        return RkpBuf {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
    }
    let boxed = bytes.into_boxed_slice();
    let len = boxed.len();
    RkpBuf {
        ptr: Box::into_raw(boxed) as *mut u8,
        len,
    }
}

fn write_err(err: *mut RkpError, code: i32, message: &str) -> i32 {
    if !err.is_null() {
        // SAFETY: the caller promises a null or valid `RkpError`; a prior value is overwritten.
        unsafe {
            std::ptr::write(
                err,
                RkpError {
                    code,
                    message: make_buf(message.as_bytes().to_vec()),
                },
            )
        };
    }
    code
}

unsafe fn c_string(p: *const c_char, what: &str) -> Result<String, String> {
    if p.is_null() {
        return Err(format!("{what} is null"));
    }
    CStr::from_ptr(p)
        .to_str()
        .map(str::to_owned)
        .map_err(|_| format!("{what} is not UTF-8"))
}

unsafe fn c_strings(
    p: *const *const c_char,
    count: usize,
    what: &str,
) -> Result<Vec<String>, String> {
    if count > MAX_LIST {
        return Err(format!("{what} has too many entries"));
    }
    if count == 0 {
        return Ok(Vec::new());
    }
    if p.is_null() {
        return Err(format!("{what} is null"));
    }
    (0..count)
        .map(|i| c_string(*p.add(i), what))
        .collect::<Result<Vec<_>, _>>()
}

/// Install the engine every later `rkp_host_start` spawns. Replaces an earlier installation
/// (hosts already running keep their own configuration); the install is process-wide.
///
/// * `engine_path`: the executable to spawn, UTF-8, not empty.
/// * `args`, `argc`: its arguments (`args` may be null when `argc` is zero).
/// * `env_keys`, `env_count`: environment variable names to pass through beyond the default
///   allowlist (null when the count is zero). The engine otherwise starts with a cleared
///   environment.
/// * `snapshot_method`: the engine method that receives the registered snapshot; null or empty
///   means `init`.
/// * `start_budget_ms`: how long `rkp_host_start` waits for the first `Ready`; zero means 60 s.
///
/// Returns `RKP_OK` (0), `RKP_INVALID_ARGUMENT` (1) or `RKP_PANIC` (8); on failure `err`, when
/// not null, carries the message (free it with `rkp_buf_free`).
///
/// # Safety
/// Every pointer is null where allowed above, or valid NUL-terminated C strings (and arrays of
/// them with the stated counts) for the duration of the call; `err` is null or valid.
#[no_mangle]
pub unsafe extern "C" fn rke_install_engine(
    engine_path: *const c_char,
    args: *const *const c_char,
    argc: usize,
    env_keys: *const *const c_char,
    env_count: usize,
    snapshot_method: *const c_char,
    start_budget_ms: u32,
    err: *mut RkpError,
) -> i32 {
    if !err.is_null() {
        std::ptr::write(
            err,
            RkpError {
                code: 0,
                message: make_buf(Vec::new()),
            },
        );
    }
    let built = catch_unwind(AssertUnwindSafe(|| {
        let path = c_string(engine_path, "engine_path")?;
        if path.is_empty() {
            return Err("engine_path is empty".to_string());
        }
        let args = c_strings(args, argc, "args")?;
        let env_keys = c_strings(env_keys, env_count, "env_keys")?;
        let method = if snapshot_method.is_null() {
            String::new()
        } else {
            c_string(snapshot_method, "snapshot_method")?
        };
        Ok(EngineFactory {
            path: PathBuf::from(path),
            args,
            env_keys,
            snapshot_method: if method.is_empty() {
                "init".to_string()
            } else {
                method
            },
            start_budget: if start_budget_ms == 0 {
                DEFAULT_START_BUDGET
            } else {
                Duration::from_millis(u64::from(start_budget_ms))
            },
        })
    }));
    match built {
        Ok(Ok(factory)) => {
            rkp::install_factory(Arc::new(factory));
            keep_abi_linked();
            rkp::RKP_OK
        }
        Ok(Err(message)) => write_err(err, rkp::RKP_INVALID_ARGUMENT, &message),
        Err(_) => write_err(err, rkp::RKP_PANIC, "panic in rke_install_engine"),
    }
}

/// Reference every ABI entry point from an exported function, so none can be dropped from the
/// archive however the final link prunes. Costs twelve pointer copies per install.
#[inline(never)]
fn keep_abi_linked() {
    let entries: [*const (); 12] = [
        rkp_versions as *const (),
        rkp_register_snapshot as *const (),
        rkp_host_start as *const (),
        rkp_host_snapshot as *const (),
        rkp_host_call as *const (),
        rkp_host_lifecycle as *const (),
        rkp_host_shutdown as *const (),
        rkp_service_connect as *const (),
        rkp_service_call as *const (),
        rkp_release_host as *const (),
        rkp_release_connection as *const (),
        rkp_buf_free as *const (),
    ];
    std::hint::black_box(entries);
}
