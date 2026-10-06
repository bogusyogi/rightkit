use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub const SUITE_DIR_NAME: &str = "Orthic Labs";
/// The shared-data subdirectory under [`SUITE_DIR_NAME`]. HeardRight, ScrapeRight and
/// CutRight already root their shared model store and speech discovery here
/// (`.../Orthic Labs/shared`), so the one convention adopts it.
pub const SHARED_DIR_NAME: &str = "shared";
/// Overrides the platform default (tests, isolated dev profiles). Must be absolute.
/// `RIGHT_SUITE_DIR` and `ORTHIC_SHARED_DIR` are honoured as legacy aliases.
pub const ENV_SUITE_ROOT: &str = "RIGHTKIT_SUITE_ROOT";

#[derive(Debug)]
pub enum SuiteError {
    /// No absolute app-data root could be determined. Never resolved to the cwd.
    NoRoot(String),
    NotAbsolute(PathBuf),
    /// A directory that must be private to the current user is not.
    Insecure(String),
    Io(io::Error),
}

impl fmt::Display for SuiteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SuiteError::NoRoot(s) => write!(
                f,
                "no app-data root: {s} (refusing to fall back to the working directory)"
            ),
            SuiteError::NotAbsolute(p) => write!(f, "suite root must be absolute: {}", p.display()),
            SuiteError::Insecure(s) => write!(f, "insecure service directory: {s}"),
            SuiteError::Io(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for SuiteError {}
impl From<io::Error> for SuiteError {
    fn from(e: io::Error) -> Self {
        SuiteError::Io(e)
    }
}

struct TempGuard(PathBuf);
impl Drop for TempGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The per-user suite directory that holds `runtime/<service>/service.json`
/// and other shared state. Always absolute.
#[derive(Clone)]
pub struct Suite {
    root: PathBuf,
    _temp: Option<Arc<TempGuard>>,
}

impl fmt::Debug for Suite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Suite({})", self.root.display())
    }
}

fn absolute(p: PathBuf) -> Result<PathBuf, SuiteError> {
    if !p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(SuiteError::NotAbsolute(p));
    }
    Ok(p)
}

fn env_nonempty(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.trim().is_empty())
}

impl Suite {
    /// `RIGHTKIT_SUITE_ROOT` if set, else the platform per-user default
    /// (`~/Library/Application Support/Orthic Labs/shared`, `%LOCALAPPDATA%\Orthic Labs\shared`).
    /// Errors, never guesses, when the environment does not name an absolute root.
    pub fn resolve() -> Result<Suite, SuiteError> {
        // Legacy aliases used by existing apps' isolated test runs (HeardRight e2e_engine).
        for key in [ENV_SUITE_ROOT, "RIGHT_SUITE_DIR", "ORTHIC_SHARED_DIR"] {
            if let Some(v) = env_nonempty(key) {
                return Suite::at(v);
            }
        }
        let base = if cfg!(windows) {
            env_nonempty("LOCALAPPDATA")
                .map(PathBuf::from)
                .ok_or_else(|| SuiteError::NoRoot("LOCALAPPDATA is not set".into()))?
        } else if cfg!(target_os = "macos") {
            env_nonempty("HOME")
                .map(|h| PathBuf::from(h).join("Library").join("Application Support"))
                .ok_or_else(|| SuiteError::NoRoot("HOME is not set".into()))?
        } else {
            env_nonempty("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    env_nonempty("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
                })
                .ok_or_else(|| SuiteError::NoRoot("neither XDG_DATA_HOME nor HOME is set".into()))?
        };
        Suite::at(absolute(base)?.join(SUITE_DIR_NAME).join(SHARED_DIR_NAME))
    }

    /// An explicit absolute root (isolated dev profiles, tests).
    pub fn at(root: impl Into<PathBuf>) -> Result<Suite, SuiteError> {
        Ok(Suite {
            root: absolute(root.into())?,
            _temp: None,
        })
    }

    /// A fresh private directory under the OS temp dir, deleted when the last
    /// clone is dropped. The explicit alternative to a missing app-data root.
    pub fn per_run_temp(label: &str) -> Result<Suite, SuiteError> {
        static N: AtomicU64 = AtomicU64::new(0);
        let label: String = label
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .take(24)
            .collect();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = absolute(std::env::temp_dir())?.join(format!(
            "rk-{label}-{}-{}-{nanos:x}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        make_private_dir(&dir)?;
        Ok(Suite {
            root: dir.clone(),
            _temp: Some(Arc::new(TempGuard(dir))),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<suite>/runtime/<service>`.
    pub fn service_dir(&self, service: &str) -> PathBuf {
        self.root.join("runtime").join(service)
    }

    /// A private (owner-only) directory under `<suite>/runtime/<service>`, e.g. `shots`.
    pub fn private_service_subdir(&self, service: &str, name: &str) -> Result<PathBuf, SuiteError> {
        let d = self.service_dir(service).join(name);
        make_private_dir(&d)?;
        Ok(d)
    }

    /// `<suite>/<area>`: shared app data that must not be relative to the cwd
    /// (screenshots, evidence, models). Created private to the user.
    pub fn data_dir(&self, area: &str) -> Result<PathBuf, SuiteError> {
        let d = self.root.join(area);
        make_private_dir(&d)?;
        Ok(d)
    }
}

pub(crate) fn valid_service_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 48
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Create `dir` (and parents) and make the leaf owner-only (0700 on Unix;
/// Windows ACLs inherit from the per-user profile).
pub(crate) fn make_private_dir(dir: &Path) -> Result<(), SuiteError> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let md = std::fs::metadata(dir)?;
        if md.uid() != crate::transport::current_uid() {
            return Err(SuiteError::Insecure(format!(
                "{} is not owned by the current user",
                dir.display()
            )));
        }
        if md.permissions().mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}
