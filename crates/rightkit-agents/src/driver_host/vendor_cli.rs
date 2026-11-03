//! Detection and capability projection for subscription vendor CLIs that run
//! turns outside the host app's own provider stack: Claude Code (`claude`) and
//! the Antigravity CLI (`agy`).
//!
//! the host app never borrows these vendors' OAuth tokens — it runs the vendor's
//! own installed CLI as the agent and the CLI authenticates itself. This
//! module is the shared read-only probe every presentation surface (`/model`,
//! `/providers`, turn dispatch) uses to answer "is the binary installed, where,
//! which version, and what models does it offer" without duplicating PATH or
//! platform-specific install-location knowledge.
//!
//! Every spawned probe is a bounded, non-interactive `--version`/`models`-style
//! call run through `crate::discovery::run_bounded` (short timeout, capped output,
//! whole-tree kill, no console window on Windows), so probing a missing or wedged CLI can
//! never hang a picker or flash a console on the operator's desktop.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

/// The two subscription vendor CLIs the host app can dispatch turns onto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VendorCli {
    ClaudeCode,
    Antigravity,
}

impl VendorCli {
    /// The provider id used on routes (`claude-code/<model>`,
    /// `antigravity/<model>`). This is deliberately *not* the existing driver
    /// kind (`claude`): the route namespace describes which CLI runs the turn,
    /// while the driver kind describes a daemon-side protocol binding.
    pub fn provider_id(self) -> &'static str {
        match self {
            VendorCli::ClaudeCode => "claude-code",
            VendorCli::Antigravity => "antigravity",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            VendorCli::ClaudeCode => "Claude Code",
            VendorCli::Antigravity => "Antigravity",
        }
    }

    /// The executable name searched on `PATH`.
    pub fn binary_name(self) -> &'static str {
        match self {
            VendorCli::ClaudeCode => "claude",
            VendorCli::Antigravity => "agy",
        }
    }

    /// One-line remediation shown when the binary is not installed.
    pub fn install_hint(self) -> &'static str {
        match self {
            VendorCli::ClaudeCode => "install with `npm install -g @anthropic-ai/claude-code`",
            VendorCli::Antigravity => {
                "install the Antigravity CLI (agy) from https://antigravity.google"
            }
        }
    }

    /// How the operator signs the vendor CLI in. the host app never drives the
    /// vendor's own login flow — it reports the exact command to run.
    pub fn login_hint(self) -> &'static str {
        match self {
            VendorCli::ClaudeCode => "run `claude` in your terminal, then `/login`",
            VendorCli::Antigravity => "run `agy` in your terminal and sign in",
        }
    }

    /// Map a route provider id back to its vendor CLI. Returns `None` for
    /// every non-vendor provider id so callers can branch on `Some(..)` alone.
    pub fn from_provider_id(id: &str) -> Option<Self> {
        match id.trim().to_ascii_lowercase().as_str() {
            "claude-code" => Some(VendorCli::ClaudeCode),
            "antigravity" => Some(VendorCli::Antigravity),
            _ => None,
        }
    }
}

/// What a detection probe learned about one vendor CLI.
///
/// `signed_in` is `None` when the vendor exposes no cheap non-interactive
/// status probe — absence of information is never reported as "signed out".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorCliStatus {
    pub installed: bool,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    /// `None` = unknown (no cheap non-interactive status command exists).
    pub signed_in: Option<bool>,
}

#[derive(Clone)]
struct VendorSnapshot {
    status: VendorCliStatus,
    models: Vec<(String, String)>,
}

fn snapshot(kind: VendorCli) -> &'static VendorSnapshot {
    static CLAUDE: OnceLock<VendorSnapshot> = OnceLock::new();
    static AGY: OnceLock<VendorSnapshot> = OnceLock::new();
    let cache = match kind {
        VendorCli::ClaudeCode => &CLAUDE,
        VendorCli::Antigravity => &AGY,
    };
    cache.get_or_init(|| probe(kind))
}

/// One official status/catalog snapshot per process, shared by every picker
/// and CLI projection. Restarting the host app observes vendor sign-in changes.
pub fn detect(kind: VendorCli) -> VendorCliStatus {
    snapshot(kind).status.clone()
}

/// Hosts that must never execute real vendor CLIs (isolated e2e harnesses)
/// set this; the vendor is then reported as not installed.
pub const DISABLE_VENDOR_CLIS_ENV: &str = "RIGHTKIT_DISABLE_VENDOR_CLIS";

/// Antigravity's own switch that stops `agy` forking its detached
/// `--bg-updater`, whose `agy --version` grandchild otherwise gets a fresh
/// visible console that `CREATE_NO_WINDOW` on our spawn cannot reach.
pub const AGY_DISABLE_AUTO_UPDATE_ENV: &str = "AGY_CLI_DISABLE_AUTO_UPDATE";

fn vendor_clis_disabled() -> bool {
    std::env::var_os(DISABLE_VENDOR_CLIS_ENV).is_some_and(|value| !value.is_empty() && value != "0")
}

fn probe(kind: VendorCli) -> VendorSnapshot {
    let located = if vendor_clis_disabled() {
        None
    } else {
        locate_binary(kind)
    };
    let Some(path) = located else {
        return VendorSnapshot {
            status: VendorCliStatus {
                installed: false,
                path: None,
                version: None,
                signed_in: None,
            },
            models: Vec::new(),
        };
    };
    let version = crate::discovery::run_bounded(&path, &["--version"], Duration::from_secs(5))
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout)
                    .ok()
                    .and_then(|text| text.lines().next().map(str::trim).map(str::to_string))
                    .filter(|line| !line.is_empty())
            } else {
                None
            }
        });
    let (signed_in, models) = match kind {
        VendorCli::ClaudeCode => (
            claude_signed_in(&path),
            vec![
                ("opus".to_string(), "Opus".to_string()),
                ("sonnet".to_string(), "Sonnet".to_string()),
                ("haiku".to_string(), "Haiku".to_string()),
            ],
        ),
        VendorCli::Antigravity => {
            match crate::discovery::run_bounded(&path, &["models"], Duration::from_secs(10)) {
                Some(output) => {
                    agy_model_status(output.status.success(), &output.stdout, &output.stderr)
                }
                None => (None, Vec::new()),
            }
        }
    };
    VendorSnapshot {
        status: VendorCliStatus {
            installed: true,
            path: Some(path),
            version,
            signed_in,
        },
        models,
    }
}

/// `(id, display)` model rows for the route picker's level 2.
///
/// Claude Code exposes no non-interactive model-list command (`claude --help`
/// has none), so its list is the CLI's own aliases — `opus`, `sonnet`,
/// `haiku` — which `--model` accepts directly. Antigravity prints a live
/// `<id>\t<display>` table via `agy models`. An uninstalled binary always
/// produces an empty list rather than a fabricated catalogue.
pub fn list_models(kind: VendorCli) -> Vec<(String, String)> {
    snapshot(kind).models.clone()
}

fn agy_model_status(
    success: bool,
    stdout: &[u8],
    stderr: &[u8],
) -> (Option<bool>, Vec<(String, String)>) {
    if success {
        return (
            Some(true),
            parse_agy_models(&String::from_utf8_lossy(stdout)),
        );
    }
    let auth_error = [stdout, stderr].iter().any(|bytes| {
        let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
        [
            "unauthenticated",
            "unauthorized",
            "not signed in",
            "not logged in",
            "authentication required",
            "authentication failed",
            "please log in",
            "please sign in",
            "login required",
            "sign-in required",
            "invalid_grant",
        ]
        .iter()
        .any(|needle| text.contains(needle))
    });
    (auth_error.then_some(false), Vec::new())
}

/// Parse `agy models` output: `<id>\t<display name>` rows, skipping the
/// `Fetching available models...` status line, blanks, and any header rows
/// that are not tab-separated.
pub fn parse_agy_models(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim_end();
            let (id, display) = line.split_once('\t')?;
            let id = id.trim();
            let display = display.trim();
            if id.is_empty() || display.is_empty() {
                return None;
            }
            Some((id.to_string(), display.to_string()))
        })
        .collect()
}

/// Locate the vendor binary: `PATH` first, then the vendor's documented
/// install location (Antigravity installs to `%LOCALAPPDATA%\agy\bin\agy.exe`
/// outside `PATH` on some setups).
fn locate_binary(kind: VendorCli) -> Option<PathBuf> {
    if let Some(path) = find_on_path(kind.binary_name()) {
        return Some(path);
    }
    match kind {
        VendorCli::Antigravity => std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|root| root.join("agy").join("bin").join("agy.exe"))
            .filter(|path| path.is_file()),
        VendorCli::ClaudeCode => None,
    }
}

/// First match for `name` on `PATH`, honouring the executable extensions the
/// platform actually launches (`PATHEXT` on Windows).
fn find_on_path(name: &str) -> Option<PathBuf> {
    crate::driver_host::binary_lookup::find_on_path(name)
}

/// `claude auth status` prints `{"loggedIn": bool, ...}` without prompting: the one cheap
/// sign-in probe a vendor CLI currently offers. The parse is RightKit's.
fn claude_signed_in(path: &std::path::Path) -> Option<bool> {
    let output = crate::discovery::run_bounded(path, &["auth", "status"], Duration::from_secs(10))?;
    crate::discovery::claude_auth_status(&output.stdout)
}
