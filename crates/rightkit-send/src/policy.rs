//! Limits and folder names the host may tune. `Policy::default()` is Pulse's
//! current behaviour, so a host that changes nothing keeps its existing rules.

use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Folder under the save folder for files from a sender that could not prove
    /// its key. Kept apart from the owner's own files.
    pub unverified_folder: String,
    /// Most files one request may name when the sender could not prove its key.
    pub unproven_max_files: usize,
    /// Most bytes one request may name when the sender could not prove its key.
    pub unproven_max_bytes: u64,
    /// Largest single file any request may name, proven or not.
    pub max_file_bytes: u64,
    /// Longest text message kept, in characters (not bytes). Longer text is cut.
    pub max_message_chars: usize,
    /// How long a request waits for the owner's answer before it is declined.
    pub answer_wait: Duration,
}

impl Default for Policy {
    fn default() -> Policy {
        Policy {
            unverified_folder: "Received (unverified)".to_string(),
            unproven_max_files: 200,
            unproven_max_bytes: 2 * 1024 * 1024 * 1024,
            max_file_bytes: 64 * 1024 * 1024 * 1024,
            max_message_chars: 64 * 1024,
            answer_wait: Duration::from_secs(60),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_pulse_current_values() {
        let policy = Policy::default();
        assert_eq!(policy.unverified_folder, "Received (unverified)");
        assert_eq!(policy.unproven_max_files, 200);
        assert_eq!(policy.unproven_max_bytes, 2 * 1024 * 1024 * 1024);
        assert_eq!(policy.max_file_bytes, 64 * 1024 * 1024 * 1024);
        assert_eq!(policy.max_message_chars, 64 * 1024);
        assert_eq!(policy.answer_wait, Duration::from_secs(60));
    }
}
