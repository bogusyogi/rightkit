//! Parser for the `ebur128` filter's end-of-run summary on FFmpeg's stderr.
//!
//! Shipped binaries may depend on this crate (rightkit-qa is a dev tool and is not one). The
//! sample peak and the true peak are kept apart: each `Peak:` value is attributed by the
//! `Sample peak:` or `True peak:` header above it.

/// The integrated loudness, loudness range and the two peaks, in the filter's own units.
/// A field is `None` when its value is absent or not finite (`-inf`, `nan`, gated values).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Ebur128Summary {
    /// `I:` under `Integrated loudness:`, in LUFS.
    pub integrated_lufs: Option<f64>,
    /// `LRA:` under `Loudness range:`, in LU.
    pub loudness_range_lu: Option<f64>,
    /// `Peak:` under `Sample peak:`, in dBFS.
    pub sample_peak_dbfs: Option<f64>,
    /// `Peak:` under `True peak:`, in dBTP.
    pub true_peak_dbtp: Option<f64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    None,
    Sample,
    True,
}

/// Parse the summary the `ebur128` filter prints when the run ends. Returns `None` only when
/// there is no `Summary:` line. Only the text after the last `Summary:` is read, so log lines
/// from other filters that ran in the same process are ignored.
pub fn parse_ebur128_summary(stderr: &str) -> Option<Ebur128Summary> {
    const MARKER: &str = "Summary:";
    let start = stderr.rfind(MARKER)? + MARKER.len();
    let mut summary = Ebur128Summary::default();
    let mut section = Section::None;
    for raw in stderr[start..].lines() {
        let line = raw.trim();
        match line {
            "Integrated loudness:" | "Loudness range:" => section = Section::None,
            "Sample peak:" => section = Section::Sample,
            "True peak:" => section = Section::True,
            _ => {
                if let Some(rest) = line.strip_prefix("I:") {
                    summary.integrated_lufs = first_finite(rest);
                } else if let Some(rest) = line.strip_prefix("LRA:") {
                    summary.loudness_range_lu = first_finite(rest);
                } else if let Some(rest) = line.strip_prefix("Peak:") {
                    match section {
                        Section::Sample => summary.sample_peak_dbfs = first_finite(rest),
                        Section::True => summary.true_peak_dbtp = first_finite(rest),
                        Section::None => {}
                    }
                }
            }
        }
    }
    Some(summary)
}

/// The first whitespace token of `rest` as a finite number, or `None` for `-inf`, `nan` or a
/// token that is not a number.
fn first_finite(rest: &str) -> Option<f64> {
    rest.split_whitespace()
        .next()
        .and_then(|token| token.parse::<f64>().ok())
        .filter(|value| value.is_finite())
}
