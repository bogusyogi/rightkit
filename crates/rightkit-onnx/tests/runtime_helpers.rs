use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::Mutex;

use rightkit_onnx::{
    configure_runtime, configure_runtime_with_options, configured_runtime,
    developer_override_candidate, runtime_filename, CandidateSource, RuntimeBindingOptions,
    RuntimeBindingStatus, RuntimeCandidate, RuntimeError,
};

// All env-mutating tests in this binary share this lock. RAII restores values
// before unlocking, including during assertion unwinding. No ORT is loaded.
static ENV_LOCK: Mutex<()> = Mutex::new(());
const OVERRIDE: &str = "RIGHTKIT_ORT_HELPER_TEST_OVERRIDE";
const ORT_PATH: &str = "ORT_DYLIB_PATH";

struct EnvRestore {
    name: &'static str,
    previous: Option<OsString>,
}

impl EnvRestore {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            previous: std::env::var_os(name),
        }
    }

    fn set(&self, value: impl AsRef<OsStr>) {
        std::env::set_var(self.name, value);
    }

    fn remove(&self) {
        std::env::remove_var(self.name);
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => self.set(value),
            None => self.remove(),
        }
    }
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("rightkit-ort-helpers-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        Self(root.canonicalize().unwrap())
    }

    fn candidate(&self, directory: &str) -> RuntimeCandidate {
        RuntimeCandidate::new(
            self.0.join(directory).join(runtime_filename()),
            CandidateSource::Bundled,
        )
    }

    fn file(&self, directory: &str) -> RuntimeCandidate {
        let candidate = self.candidate(directory);
        std::fs::create_dir_all(candidate.path.parent().unwrap()).unwrap();
        std::fs::write(&candidate.path, b"runtime-fixture").unwrap();
        candidate
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(debug_assertions)]
#[test]
fn developer_override_uses_only_nonempty_named_variable_as_explicit_candidate() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let env = EnvRestore::new(OVERRIDE);
    env.remove();
    assert_eq!(developer_override_candidate(OVERRIDE), None);
    env.set("");
    assert_eq!(developer_override_candidate(OVERRIDE), None);
    for value in ["relative developer runtime", " "] {
        env.set(value);
        assert_eq!(
            developer_override_candidate(OVERRIDE),
            Some(RuntimeCandidate::new(value, CandidateSource::Explicit))
        );
        assert_eq!(std::env::var_os(OVERRIDE), Some(OsString::from(value)));
    }
}

#[cfg(not(debug_assertions))]
#[test]
fn developer_override_is_ignored_in_release() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let env = EnvRestore::new(OVERRIDE);
    for value in ["", "shell-exported-dev-runtime"] {
        env.set(value);
        assert_eq!(developer_override_candidate(OVERRIDE), None);
        assert_eq!(std::env::var_os(OVERRIDE), Some(OsString::from(value)));
    }
    env.remove();
    assert_eq!(developer_override_candidate(OVERRIDE), None);
}

#[cfg(all(unix, debug_assertions))]
#[test]
fn developer_override_preserves_non_unicode_paths() {
    use std::os::unix::ffi::OsStringExt;

    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let env = EnvRestore::new(OVERRIDE);
    let value = OsString::from_vec(b"developer-\xff-runtime".to_vec());
    env.set(&value);
    assert_eq!(
        developer_override_candidate(OVERRIDE),
        Some(RuntimeCandidate::new(
            PathBuf::from(value),
            CandidateSource::Explicit
        ))
    );
}

// One test owns process binding state in this integration-test binary. Other
// tests only read the caller-named override; none invokes runtime binding.
#[test]
fn configured_runtime_tracks_successful_bindings_and_preserves_failed_binding_state() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let env = EnvRestore::new(ORT_PATH);
    env.remove();
    assert_eq!(configured_runtime(), None);
    env.set("");
    assert_eq!(configured_runtime(), None);
    assert_eq!(std::env::var_os(ORT_PATH), Some(OsString::new()));

    // Fallback reports even an unvalidated path without touching env or ORT.
    env.set("inherited-runtime");
    assert_eq!(
        configured_runtime(),
        Some(PathBuf::from("inherited-runtime"))
    );
    assert_eq!(
        std::env::var_os(ORT_PATH),
        Some(OsString::from("inherited-runtime"))
    );
    assert!(matches!(
        configure_runtime([]),
        Err(RuntimeError::NoCandidates)
    ));
    assert_eq!(
        configured_runtime(),
        Some(PathBuf::from("inherited-runtime"))
    );

    let fixture = Fixture::new();
    let first = fixture.file("first");
    let second = fixture.file("second");
    let first_path = first.path.canonicalize().unwrap();
    let second_path = second.path.canonicalize().unwrap();
    // Resolution alone must not claim a process binding.
    rightkit_onnx::resolve_runtime([first.clone()]).unwrap();
    assert_eq!(
        configured_runtime(),
        Some(PathBuf::from("inherited-runtime"))
    );

    // An already matching inherited path is recorded without rewriting it.
    env.set(&first.path);
    let inherited = std::env::var_os(ORT_PATH);
    configure_runtime([first]).unwrap();
    assert_eq!(configured_runtime(), Some(first_path.clone()));
    assert_eq!(std::env::var_os(ORT_PATH), inherited);
    assert!(matches!(
        configure_runtime([second.clone()]),
        Err(RuntimeError::AlreadyConfigured { .. })
    ));
    assert_eq!(configured_runtime(), Some(first_path.clone()));

    env.set(&second.path);
    assert_eq!(configured_runtime(), Some(first_path.clone()));
    assert_eq!(
        std::env::var_os(ORT_PATH),
        Some(second.path.clone().into_os_string())
    );
    env.remove();
    assert_eq!(configured_runtime(), Some(first_path));
    configure_runtime([second]).unwrap();
    assert_eq!(configured_runtime(), Some(second_path.clone()));
    assert_eq!(
        std::env::var_os(ORT_PATH),
        Some(second_path.into_os_string())
    );

    let missing = fixture.candidate("missing");
    env.set("invalid-inherited-runtime");
    let binding = configure_runtime_with_options(
        [missing.clone()],
        &RuntimeBindingOptions {
            force_replace: true,
            allow_missing: true,
        },
    )
    .unwrap();
    assert_eq!(binding.status, RuntimeBindingStatus::Missing);
    assert_eq!(configured_runtime(), Some(missing.path.clone()));
    env.remove();
    assert_eq!(configured_runtime(), Some(missing.path.clone()));
    assert!(matches!(
        configure_runtime([fixture.candidate("still-missing")]),
        Err(RuntimeError::NotFound { .. })
    ));
    assert_eq!(configured_runtime(), Some(missing.path));
    assert_eq!(std::env::var_os(ORT_PATH), None);
}
