//! Hostile-input caps, enforced inside the crate before any parser sees a byte.

use serde::{Deserialize, Serialize};

/// Caps on one input file. The defaults suit documents a person or an agent opens; raise them
/// per call, never process-wide.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Limits {
    /// Compressed (on-disk) size of the input.
    pub max_input_bytes: u64,
    /// Total decompressed bytes of every part of a package.
    pub max_total_bytes: u64,
    /// Decompressed bytes of any one part.
    pub max_part_bytes: u64,
    /// Entries in a package.
    pub max_entries: usize,
    /// Decompressed to compressed ratio, applied to any part over 1 MiB and to the package as a
    /// whole once it is over 4 MiB. Honest spreadsheets with repeated rows reach 50:1.
    pub max_ratio: u64,
    /// Pages a PDF may have.
    pub max_pdf_pages: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 64 << 20,
            max_total_bytes: 256 << 20,
            max_part_bytes: 64 << 20,
            max_entries: 10_000,
            max_ratio: 200,
            max_pdf_pages: 5_000,
        }
    }
}
