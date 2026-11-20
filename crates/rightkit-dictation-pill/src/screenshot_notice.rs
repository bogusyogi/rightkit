//! Whether a screenshot outcome detail names a saved file or a clipboard copy. Moved from
//! HeardRight `pill_overlays.rs` (`screenshot_notice_text`); the host words each outcome.

/// What a screenshot command did, read from its detail string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotOutcome {
    /// A file was written (the detail names a `.png` or a screenshots folder).
    Saved,
    /// The image went to the clipboard.
    Copied,
}

/// Classify `detail` (case-insensitive; either path separator).
pub fn classify(detail: &str) -> ScreenshotOutcome {
    let lower = detail.to_ascii_lowercase();
    if lower.contains(".png")
        || lower.contains("\\screenshots\\")
        || lower.contains("/screenshots/")
    {
        ScreenshotOutcome::Saved
    } else {
        ScreenshotOutcome::Copied
    }
}
