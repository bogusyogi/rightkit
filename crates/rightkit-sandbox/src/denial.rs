//! Sandbox denial-signature taxonomy  (ported from CodeRight `sandbox/denial_signature.rs`).
//!
//! Classifies a sandbox denial from its raw error output so escalation
//! decisions are
//! evidence-driven rather than a blanket retry-with-more-privilege. Every
//! sandbox backend (macOS Seatbelt, Linux Landlock/seccomp, Windows
//! AppContainer/Job) surfaces denials as OS-specific error text; this module
//! is the single place that turns that text into a stable, cross-platform
//! signature the policy crate can reason about without depending on any
//! particular sandbox implementation.
//!
//! Pure classification: no I/O, no process spawning. Callers pass in
//! whatever stderr/exit-status text their sandbox backend produced.

use std::fmt;

/// The category of resource a sandbox denial was about.
///
/// Ordered so that a caller who wants "most specific wins" can compare
/// signatures found in the same output and prefer earlier variants, though
/// in practice callers should classify one denial at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DenialCategory {
    /// Read or write to a filesystem path was blocked.
    Filesystem,
    /// A network connection/bind/resolve was blocked.
    Network,
    /// Spawning a child process (fork/exec) was blocked.
    ProcessSpawn,
    /// A denial was recognized but does not fit the categories above
    /// (e.g. IPC, device access, ioctl restrictions).
    Other,
    /// The output did not match any known sandbox-denial pattern at all —
    /// distinct from `Other`, which means "denied, but not one of our
    /// named categories." `Unrecognized` means "we are not confident this
    /// was a sandbox denial in the first place."
    Unrecognized,
}

impl fmt::Display for DenialCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            DenialCategory::Filesystem => "filesystem",
            DenialCategory::Network => "network",
            DenialCategory::ProcessSpawn => "process-spawn",
            DenialCategory::Other => "other",
            DenialCategory::Unrecognized => "unrecognized",
        };
        f.write_str(label)
    }
}

/// Which sandbox backend produced the denial text, when known.
///
/// Purely informational — classification does not branch on this, since
/// the whole point of the taxonomy is one shared vocabulary across
/// backends. Kept so the decision log can record provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenialSource {
    MacosSeatbelt,
    LinuxLandlockSeccomp,
    WindowsAppContainer,
    WindowsJob,
    Unknown,
}

/// A classified sandbox denial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenialSignature {
    pub category: DenialCategory,
    pub source: DenialSource,
    /// The specific substring/pattern that triggered classification,
    /// retained for audit/debugging — never the full raw output, which may
    /// contain absolute paths or other environment-specific detail beyond
    /// what a decision log should retain.
    pub matched_pattern: &'static str,
}

impl DenialSignature {
    /// Whether this signature represents a real, recognized denial (as
    /// opposed to `Unrecognized` output that merely failed for some other
    /// reason and must not be treated as sandbox evidence).
    pub fn is_recognized(&self) -> bool {
        self.category != DenialCategory::Unrecognized
    }

    /// Stable machine identifier for decision-log / escalation-ladder
    /// attribution, e.g. `"denial-signature:filesystem"`.
    pub fn rule_id(&self) -> String {
        format!("denial-signature:{}", self.category)
    }
}

/// Filesystem-denial substrings, checked case-insensitively. Covers macOS
/// Seatbelt (`sandbox-exec` denial messages), Linux Landlock/seccomp EPERM
/// text, and Windows AppContainer ACCESS_DENIED phrasing.
const FILESYSTEM_PATTERNS: &[&str] = &[
    "deny file-read",
    "deny file-write",
    "deny(1) file-read",
    "deny(1) file-write",
    "operation not permitted",
    "permission denied",
    "eacces",
    "eperm",
    "access is denied",
    "landlock",
];

/// Network-denial substrings.
const NETWORK_PATTERNS: &[&str] = &[
    "deny network",
    "deny(1) network",
    "network-outbound",
    "network-inbound",
    "econnrefused",
    "eafnosupport",
    "could not resolve host",
    "name or service not known",
    "winsock",
    "wsaeacces",
];

/// Process-spawn-denial substrings.
const PROCESS_SPAWN_PATTERNS: &[&str] = &[
    "deny process-fork",
    "deny process-exec",
    "deny(1) process-fork",
    "deny(1) process-exec",
    "fork failed",
    "exec format error",
    "createprocess failed",
    "seccomp",
];

/// Denial signatures that are recognized but do not fit a named category —
/// still real sandbox evidence, just not filesystem/network/process.
const OTHER_PATTERNS: &[&str] = &[
    "deny ipc",
    "deny(1) ipc",
    "deny mach-lookup",
    "deny(1) mach-lookup",
    "deny sysctl",
    "deny signal",
    "operation not supported by sandbox",
];

/// Classify a sandbox denial from raw error output (stderr, panic text, or
/// an OS error message), plus the backend that produced it when known.
///
/// Matching is substring-based and case-insensitive by design: sandbox
/// backends do not emit structured errors, and CodeRight has no control
/// over their exact wording across OS versions. This deliberately trades
/// some precision for robustness — a scan that misses a rare phrasing
/// variant fails safe into `Unrecognized`, which blocks escalation rather
/// than permitting an unearned one.
pub fn classify_denial(raw_output: &str, source: DenialSource) -> DenialSignature {
    let lower = raw_output.to_ascii_lowercase();

    if let Some(pattern) = first_match(&lower, FILESYSTEM_PATTERNS) {
        return DenialSignature {
            category: DenialCategory::Filesystem,
            source,
            matched_pattern: pattern,
        };
    }
    if let Some(pattern) = first_match(&lower, NETWORK_PATTERNS) {
        return DenialSignature {
            category: DenialCategory::Network,
            source,
            matched_pattern: pattern,
        };
    }
    if let Some(pattern) = first_match(&lower, PROCESS_SPAWN_PATTERNS) {
        return DenialSignature {
            category: DenialCategory::ProcessSpawn,
            source,
            matched_pattern: pattern,
        };
    }
    if let Some(pattern) = first_match(&lower, OTHER_PATTERNS) {
        return DenialSignature {
            category: DenialCategory::Other,
            source,
            matched_pattern: pattern,
        };
    }

    DenialSignature {
        category: DenialCategory::Unrecognized,
        source,
        matched_pattern: "",
    }
}

fn first_match(lower_haystack: &str, patterns: &[&'static str]) -> Option<&'static str> {
    patterns
        .iter()
        .find(|pattern| contains_token(lower_haystack, pattern))
        .copied()
}

/// Whether `needle` appears in `haystack` as a whole token.
///
/// A plain `contains` is wrong here because the short errno tokens are
/// substrings of one another *across categories*: `eacces` sits inside
/// Winsock's `wsaeacces`. Since the filesystem list is scanned before the
/// network list, `contains` lets the generic filesystem errno swallow the more
/// specific network one, and "socket error WSAEACCES" classifies as a
/// filesystem denial. Requiring a non-alphanumeric neighbour on each side
/// keeps every pattern matching only where it is actually written, which fixes
/// the collision without making category order load-bearing.
fn contains_token(haystack: &str, needle: &str) -> bool {
    let mut from = 0usize;
    while let Some(offset) = haystack[from..].find(needle) {
        let start = from + offset;
        let end = start + needle.len();
        let boundary_before = !haystack[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric());
        let boundary_after = !haystack[end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric());
        if boundary_before && boundary_after {
            return true;
        }
        from = end;
    }
    false
}
