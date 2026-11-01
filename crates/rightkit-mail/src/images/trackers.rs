//! Tracker-host blocklist for a remote-image proxy. A curated list of known
//! email/marketing tracking-pixel hosts (see `data/email-trackers.txt`
//! for source + license) is bundled into the binary via `include_str!` and
//! parsed once into a suffix set. Matching is suffix-on-registrable-host, e.g.
//! `track.example.com` matches an `example.com` entry, but `notexample.com`
//! does not (word-boundary check via a `.` before the suffix) — never a
//! substring match against the raw URL.
use std::collections::HashSet;
use std::sync::OnceLock;

const RAW_LIST: &str = include_str!("../../data/email-trackers.txt");

fn blocklist() -> &'static HashSet<String> {
    static BLOCKLIST: OnceLock<HashSet<String>> = OnceLock::new();
    BLOCKLIST.get_or_init(|| {
        RAW_LIST
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_ascii_lowercase)
            .collect()
    })
}

/// Number of curated entries currently loaded (used by tests + diagnostics).
#[allow(dead_code)]
pub fn blocklist_len() -> usize {
    blocklist().len()
}

/// True if `host` is a known tracker host: either an exact match for a
/// blocklist entry, or a subdomain of one (`sub.entry.com` matches
/// `entry.com`). Case-insensitive. Never does substring/URL matching — the
/// caller must pass an already-parsed URL host, not the raw URL string.
pub fn is_tracker_host(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    let list = blocklist();
    if list.contains(&host) {
        return true;
    }
    list.iter().any(|entry| {
        host.len() > entry.len() && host.ends_with(entry.as_str()) && {
            // Boundary check: the char right before the matched suffix must be
            // a '.', so "notexample.com" never matches an "example.com" entry.
            host.as_bytes()[host.len() - entry.len() - 1] == b'.'
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocklist_parses_curated_asset_into_entries() {
        // Sanity: the bundled list isn't empty and isn't accidentally huge from
        // a header-parsing bug (comments/blank lines must be filtered out).
        let len = blocklist_len();
        assert!(len > 50, "expected a substantial curated list, got {len}");
        assert!(len < 1000, "unexpectedly large blocklist: {len}");
    }

    #[test]
    fn exact_known_tracker_host_is_blocked() {
        assert!(is_tracker_host("mailchimp.com"));
        assert!(is_tracker_host("sendgrid.net"));
        assert!(is_tracker_host("track.mixmax.com"));
    }

    #[test]
    fn subdomain_of_known_tracker_host_is_blocked() {
        assert!(is_tracker_host("open.track.mixmax.com"));
        assert!(is_tracker_host("a.b.mailchimp.com"));
    }

    #[test]
    fn unrelated_host_is_not_blocked() {
        assert!(!is_tracker_host("example.com"));
        assert!(!is_tracker_host("damneddesigns.com"));
        assert!(!is_tracker_host("gmail.com"));
    }

    #[test]
    fn lookalike_host_is_not_a_false_positive_suffix_match() {
        // "notmailchimp.com" ends with "mailchimp.com" as a raw string suffix
        // but is NOT a subdomain of it — the '.' boundary check must reject it.
        assert!(!is_tracker_host("notmailchimp.com"));
        assert!(!is_tracker_host("evilsendgrid.net"));
    }

    #[test]
    fn matching_is_case_insensitive_and_trims_trailing_dot() {
        assert!(is_tracker_host("MailChimp.COM"));
        assert!(is_tracker_host("mailchimp.com."));
    }

    #[test]
    fn empty_or_whitespace_host_is_never_blocked() {
        assert!(!is_tracker_host(""));
        assert!(!is_tracker_host("   "));
    }
}
