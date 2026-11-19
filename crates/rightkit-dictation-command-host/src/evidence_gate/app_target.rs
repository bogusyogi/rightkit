//! Foreground application identity and the host's classification of it (macOS).
//!
//! Nothing here names an application. The host supplies two lists of bundle identifiers and one
//! explicit choice, [`AppTargetPolicy::new`] validates them, and the two pure functions
//! [`classify_observation`] and [`recheck_observation`] turn an [`AppObservation`] (read from the
//! OS by the package, never from a window title) into "this application may be written into" or a
//! stable refusal tag.
//!
//! The classes ([`AppClass`]) are decided by the host's lists only:
//!
//! * `Terminal`: the bundle identifier is on the host's terminal list. Never written into
//!   automatically on macOS: no pane confirmation exists there (the Windows pane check is
//!   `wezterm_pane_now`, Windows only), so there is nothing that could make a terminal write
//!   confirmed.
//! * `StreamingAllowed`: the bundle identifier is on the host's streaming list, which is an
//!   attestation by the host that the application is not a terminal and hosts none. This is the
//!   only class streaming writes into.
//! * `Unclassified`: on neither list. Streaming refuses it, always. Paste follows
//!   [`UnclassifiedPaste`], which the host must choose and which has no default.
//!
//! A bundle identifier cannot tell a terminal emulator from an editor with an integrated terminal
//! (the identifier names the application, not the focused view), and the accessibility roles the
//! automation crate reads do not tell them apart either, so an editor with an integrated terminal
//! can be classified only as a whole. The host decides which way; an application the host does not
//! list is `Unclassified`.
//!
//! Observation is tri-state ([`AppObservation`]): a known identity, unknown (the OS named no
//! application, or the application has no bundle identifier) and error (the identity was
//! malformed, or the platform cannot observe). Unknown and error refuse. Nothing negates or
//! defaults them. No type here implements `Debug`, because an identity carries a bundle path.

/// The most identifiers either list of an [`AppTargetPolicy`] may hold.
pub const MAX_POLICY_IDENTIFIERS: usize = 64;
/// The longest bundle identifier, in bytes, accepted from a host list or from the OS.
pub const MAX_BUNDLE_IDENTIFIER_BYTES: usize = 128;
/// The longest bundle path, in bytes, accepted from the OS. Longer is an observation error.
pub const MAX_BUNDLE_PATH_BYTES: usize = 1_024;

/// Why [`AppTargetPolicy::new`] refused its inputs. A closed enum without payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppPolicyError {
    /// More than [`MAX_POLICY_IDENTIFIERS`] terminal identifiers.
    TooManyTerminals,
    /// More than [`MAX_POLICY_IDENTIFIERS`] streaming identifiers.
    TooManyStreamingApps,
    /// An identifier is empty, longer than [`MAX_BUNDLE_IDENTIFIER_BYTES`], or holds a character
    /// other than ASCII letters, digits, `.`, `-` and `_`.
    InvalidIdentifier,
    /// One identifier (compared case-insensitively) is on both lists. A terminal is never allowed
    /// to stream, so the host's lists contradict each other.
    ListedAsBoth,
}

/// What an automatic paste (and the Return after it) does with an application that is on neither
/// list. The host chooses; there is no default. Streaming never consults this: it refuses an
/// unclassified application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnclassifiedPaste {
    /// Paste into it, with every other step of the gate (secure read, control read, modifier
    /// sample, receipt) unchanged. Risk the host accepts: a terminal that is not on the terminal
    /// list receives the paste.
    Allow,
    /// Refuse it: the transcript is offered for manual copy instead.
    Refuse,
}

/// The class of an application under a policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppClass {
    Terminal,
    StreamingAllowed,
    Unclassified,
}

/// The host's classification of applications by bundle identifier. Fields are private: the only
/// way to build one is [`AppTargetPolicy::new`], which validates, so a policy in hand is valid.
#[derive(Clone, Copy, Debug)]
pub struct AppTargetPolicy {
    terminals: &'static [&'static str],
    streaming_apps: &'static [&'static str],
    unclassified_paste: UnclassifiedPaste,
}

const fn is_valid_identifier(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_BUNDLE_IDENTIFIER_BYTES {
        return false;
    }
    let mut index = 0;
    while index < bytes.len() {
        if !matches!(bytes[index], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'-' | b'_') {
            return false;
        }
        index += 1;
    }
    true
}

const fn ascii_lower(byte: u8) -> u8 {
    if byte >= b'A' && byte <= b'Z' {
        byte + 32
    } else {
        byte
    }
}

const fn same_identifier(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut index = 0;
    while index < a.len() {
        if ascii_lower(a[index]) != ascii_lower(b[index]) {
            return false;
        }
        index += 1;
    }
    true
}

impl AppTargetPolicy {
    /// Validate and build a policy. A `const fn`, so a host that builds it in a `static` is
    /// validated at compile time (an invalid list is a build error there). Ranges: at most
    /// [`MAX_POLICY_IDENTIFIERS`] identifiers per list, each 1 to [`MAX_BUNDLE_IDENTIFIER_BYTES`]
    /// bytes of ASCII letters, digits, `.`, `-` and `_`; no identifier on both lists. Either list
    /// may be empty (an empty streaming list means streaming never writes on macOS; an empty
    /// terminal list means no terminal is identified, which is the host's explicit choice).
    pub const fn new(
        terminal_bundle_identifiers: &'static [&'static str],
        streaming_bundle_identifiers: &'static [&'static str],
        unclassified_paste: UnclassifiedPaste,
    ) -> Result<Self, AppPolicyError> {
        if terminal_bundle_identifiers.len() > MAX_POLICY_IDENTIFIERS {
            return Err(AppPolicyError::TooManyTerminals);
        }
        if streaming_bundle_identifiers.len() > MAX_POLICY_IDENTIFIERS {
            return Err(AppPolicyError::TooManyStreamingApps);
        }
        let mut terminal = 0;
        while terminal < terminal_bundle_identifiers.len() {
            if !is_valid_identifier(terminal_bundle_identifiers[terminal]) {
                return Err(AppPolicyError::InvalidIdentifier);
            }
            terminal += 1;
        }
        let mut streaming = 0;
        while streaming < streaming_bundle_identifiers.len() {
            if !is_valid_identifier(streaming_bundle_identifiers[streaming]) {
                return Err(AppPolicyError::InvalidIdentifier);
            }
            streaming += 1;
        }
        let mut outer = 0;
        while outer < terminal_bundle_identifiers.len() {
            let mut inner = 0;
            while inner < streaming_bundle_identifiers.len() {
                if same_identifier(
                    terminal_bundle_identifiers[outer],
                    streaming_bundle_identifiers[inner],
                ) {
                    return Err(AppPolicyError::ListedAsBoth);
                }
                inner += 1;
            }
            outer += 1;
        }
        Ok(Self {
            terminals: terminal_bundle_identifiers,
            streaming_apps: streaming_bundle_identifiers,
            unclassified_paste,
        })
    }

    /// The class of `bundle_identifier` (ASCII case-insensitive). The terminal list wins.
    pub fn classify(&self, bundle_identifier: &str) -> AppClass {
        if self
            .terminals
            .iter()
            .any(|listed| listed.eq_ignore_ascii_case(bundle_identifier))
        {
            return AppClass::Terminal;
        }
        if self
            .streaming_apps
            .iter()
            .any(|listed| listed.eq_ignore_ascii_case(bundle_identifier))
        {
            return AppClass::StreamingAllowed;
        }
        AppClass::Unclassified
    }

    pub fn unclassified_paste(&self) -> UnclassifiedPaste {
        self.unclassified_paste
    }
}

/// The foreground application as the OS reports it: the process id, the bundle identifier and the
/// bundle path. Not `Debug` (the path is a user path). Two identities are the same application
/// only when all three are equal.
#[derive(Clone, PartialEq, Eq)]
pub struct AppIdentity {
    pid: u32,
    bundle_identifier: String,
    bundle_path: Option<String>,
}

impl AppIdentity {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn bundle_identifier(&self) -> &str {
        &self.bundle_identifier
    }
}

/// One tri-state observation of the foreground application. Not `Debug`.
pub enum AppObservation {
    /// The OS named the application, and the identity passed validation.
    Known(AppIdentity),
    /// The OS named no frontmost application, or the application has no bundle identifier.
    /// Refuses.
    Unknown,
    /// The identity was malformed (a non-positive process id, an identifier outside the accepted
    /// charset or length, a bundle path over [`MAX_BUNDLE_PATH_BYTES`]) or the platform cannot
    /// observe. Refuses.
    Error,
}

impl AppObservation {
    /// Build an observation from what the OS gave: `pid`, the bundle identifier and the bundle
    /// path of the frontmost application. Pure.
    pub fn from_parts(
        pid: i32,
        bundle_identifier: Option<&str>,
        bundle_path: Option<&str>,
    ) -> Self {
        let Ok(pid) = u32::try_from(pid) else {
            return Self::Error;
        };
        if pid == 0 {
            return Self::Error;
        }
        let Some(bundle_identifier) = bundle_identifier.filter(|id| !id.is_empty()) else {
            return Self::Unknown;
        };
        if !is_valid_identifier(bundle_identifier) {
            return Self::Error;
        }
        if bundle_path.is_some_and(|path| path.len() > MAX_BUNDLE_PATH_BYTES) {
            return Self::Error;
        }
        Self::Known(AppIdentity {
            pid,
            bundle_identifier: bundle_identifier.to_string(),
            bundle_path: bundle_path.map(str::to_string),
        })
    }
}

/// What the application is being observed for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppUse {
    /// Streaming insertion: only a `StreamingAllowed` application passes.
    Streaming,
    /// An automatic paste or Return: a terminal refuses; `Unclassified` follows
    /// [`UnclassifiedPaste`].
    Paste,
}

/// Why an application observation refused. A closed enum without payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppRefusal {
    /// The application is on the host's terminal list.
    Terminal,
    /// The application is on neither list and the use does not permit that.
    Unclassified,
    /// The OS named no application, or it has no bundle identifier.
    IdentityUnknown,
    /// The identity was malformed or the platform cannot observe.
    IdentityError,
    /// The application observed now is not the one bound at capture.
    IdentityChanged,
}

impl AppRefusal {
    /// A stable, content-free tag.
    pub fn code(self) -> &'static str {
        match self {
            AppRefusal::Terminal => "app_terminal",
            AppRefusal::Unclassified => "app_unclassified",
            AppRefusal::IdentityUnknown => "app_identity_unknown",
            AppRefusal::IdentityError => "app_identity_error",
            AppRefusal::IdentityChanged => "app_identity_changed",
        }
    }
}

/// Capture: classify an observation for `usage` and return the identity to bind, or the refusal.
/// A terminal, an unclassified application (where the use does not permit one), an unknown
/// observation and an error observation all refuse.
pub fn classify_observation(
    policy: &AppTargetPolicy,
    observation: AppObservation,
    usage: AppUse,
) -> Result<AppIdentity, AppRefusal> {
    let identity = match observation {
        AppObservation::Known(identity) => identity,
        AppObservation::Unknown => return Err(AppRefusal::IdentityUnknown),
        AppObservation::Error => return Err(AppRefusal::IdentityError),
    };
    match policy.classify(identity.bundle_identifier()) {
        AppClass::Terminal => Err(AppRefusal::Terminal),
        AppClass::StreamingAllowed => Ok(identity),
        AppClass::Unclassified => match usage {
            AppUse::Streaming => Err(AppRefusal::Unclassified),
            AppUse::Paste => match policy.unclassified_paste() {
                UnclassifiedPaste::Allow => Ok(identity),
                UnclassifiedPaste::Refuse => Err(AppRefusal::Unclassified),
            },
        },
    }
}

/// Re-observation at an authorization: the fresh observation must pass the same classification
/// and be the identity bound at capture. A switch to a terminal reports `Terminal`, a switch to
/// another permitted application reports `IdentityChanged`.
pub fn recheck_observation(
    policy: &AppTargetPolicy,
    bound: &AppIdentity,
    observation: AppObservation,
    usage: AppUse,
) -> Result<(), AppRefusal> {
    let fresh = classify_observation(policy, observation, usage)?;
    if &fresh == bound {
        Ok(())
    } else {
        Err(AppRefusal::IdentityChanged)
    }
}
