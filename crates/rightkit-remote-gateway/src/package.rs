//! Limits for Office Open XML packages (DOCX, XLSX, PPTX), which are zip files: the checks a
//! document must pass before any reader or writer opens it. Remote callers hand these files over
//! (`files.fetch`, `files.patch` and the planned `docs.write`), so the file is untrusted.
//! `docs.read` runs `check` on a package before it names the format `unsupported_format`, so the
//! limits are enforced today and every later reader or writer starts from them.
//!
//! The check reads only the zip's central directory (no byte of an entry is decompressed, so it
//! cannot be made to inflate anything) and refuses, with a stable code:
//!
//! | Limit | Value | Code |
//! | --- | --- | --- |
//! | Entries in the package | `MAX_ENTRIES` (2000) | `too_large` |
//! | One entry unpacked | `MAX_ENTRY_BYTES` (64 MiB) | `too_large` |
//! | All entries unpacked | `MAX_TOTAL_BYTES` (256 MiB) | `too_large` |
//! | Unpack ratio of an entry over `RATIO_FLOOR` | `MAX_RATIO` (100 to 1) | `too_large` |
//! | An encrypted entry (zip encryption) | none allowed | `encrypted` |
//! | A password-protected Office file (an OLE container holding `EncryptedPackage`) | none allowed | `encrypted` |
//! | Zip64, a damaged directory, a duplicate entry name, an unsafe name (`..`, a leading `/`, `\`, `:`) | none allowed | `unreadable` |
//! | Macros (a VBA project or macro sheet), external links, embedded objects or ActiveX | none allowed | `active_content` |
//!
//! Not checked here, because it needs the decompressed XML and the host app has no reader yet: XML
//! entity expansion and DOCTYPE declarations inside the parts, and formula content (a formula
//! that starts a program or reads another file). The future reader owes both: it must refuse a
//! DOCTYPE, and `docs.write` must treat a cell or text that starts with `=`, `+`, `-` or `@` as
//! text, never as a formula. The macro check goes by entry names, not by the content-types part
//! (which is compressed): a package that marks itself macro-enabled but carries no VBA entry is
//! not caught here.

use super::{Refusal, Result};
use std::collections::HashSet;

/// Most entries one package may hold. A real DOCX has dozens, a large XLSX a few hundred.
pub const MAX_ENTRIES: usize = 2000;
/// Most bytes one entry may unpack to.
pub const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;
/// Most bytes all entries together may unpack to.
pub const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
/// An entry that unpacks to no more than this is not ratio-checked (small parts compress well).
pub const RATIO_FLOOR: u64 = 1024 * 1024;
/// Most an entry over `RATIO_FLOOR` may grow when unpacked (unpacked size / packed size).
pub const MAX_RATIO: u64 = 100;
/// Longest entry name, in bytes.
const MAX_NAME: usize = 512;

const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const EOCD_LEN: usize = 22;
/// The end record is within its fixed part plus a comment of at most 65535 bytes.
const EOCD_SEARCH: usize = EOCD_LEN + 65_535;
const ENTRY_SIGNATURE: u32 = 0x0201_4b50;
const ENTRY_FIXED: usize = 46;
const LOCAL_HEADER: u64 = 30;

/// What the directory says about a package that passed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    pub entries: usize,
    /// The sum of the entries' unpacked sizes.
    pub unpacked: u64,
}

fn bad(why: &str) -> Refusal {
    Refusal::new(
        "unreadable",
        format!("the document package is damaged or unsafe: {why}"),
    )
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Checks the zip central directory of `bytes` against every limit above. Reads no entry data.
pub fn check(bytes: &[u8]) -> Result<Summary> {
    let eocd = find_end_record(bytes).ok_or_else(|| bad("no zip directory"))?;
    let declared = usize::from(u16_at(bytes, eocd + 10));
    let dir_size = u64::from(u32_at(bytes, eocd + 12));
    let dir_offset = u64::from(u32_at(bytes, eocd + 16));
    // The zip64 markers: sizes this package cannot need under the host app's file cap.
    if declared == 0xFFFF || dir_size == 0xFFFF_FFFF || dir_offset == 0xFFFF_FFFF {
        return Err(bad("zip64 packages are not read"));
    }
    if declared > MAX_ENTRIES {
        return Err(Refusal::new(
            "too_large",
            format!("the package holds more than {MAX_ENTRIES} entries"),
        ));
    }
    let end = dir_offset
        .checked_add(dir_size)
        .filter(|end| *end <= eocd as u64)
        .ok_or_else(|| bad("the directory runs past the end of the file"))?;
    let mut at = usize::try_from(dir_offset).map_err(|_| bad("the directory is out of range"))?;
    let end = end as usize;

    let mut names = HashSet::new();
    let mut unpacked_total = 0u64;
    let mut seen = 0usize;
    while at < end {
        if at + ENTRY_FIXED > end || u32_at(bytes, at) != ENTRY_SIGNATURE {
            return Err(bad("a directory entry is cut short"));
        }
        seen += 1;
        if seen > declared {
            return Err(bad("more entries than the directory declares"));
        }
        let flags = u16_at(bytes, at + 8);
        let method = u16_at(bytes, at + 10);
        let packed = u64::from(u32_at(bytes, at + 20));
        let unpacked = u64::from(u32_at(bytes, at + 24));
        let name_len = usize::from(u16_at(bytes, at + 28));
        let extra_len = usize::from(u16_at(bytes, at + 30));
        let comment_len = usize::from(u16_at(bytes, at + 32));
        let local = u64::from(u32_at(bytes, at + 42));
        let name_end = at + ENTRY_FIXED + name_len;
        let next = name_end + extra_len + comment_len;
        if name_len == 0 || name_len > MAX_NAME || next > end {
            return Err(bad("an entry name or field is out of range"));
        }
        if flags & 1 != 0 {
            return Err(Refusal::new(
                "encrypted",
                "the document package is encrypted; the app does not open protected documents",
            ));
        }
        if packed == 0xFFFF_FFFF || unpacked == 0xFFFF_FFFF {
            return Err(bad("zip64 entries are not read"));
        }
        if local + LOCAL_HEADER > dir_offset {
            return Err(bad("an entry starts outside the file's data"));
        }
        let name = String::from_utf8_lossy(&bytes[at + ENTRY_FIXED..name_end]).to_ascii_lowercase();
        if !safe_name(&name) {
            return Err(bad("an entry name is unsafe"));
        }
        if !names.insert(name.clone()) {
            return Err(bad("two entries share one name"));
        }
        if let Some(kind) = active_content(&name) {
            return Err(Refusal::new(
                "active_content",
                format!("the document carries {kind}, which the app does not open or write"),
            ));
        }
        if unpacked > MAX_ENTRY_BYTES {
            return Err(Refusal::new(
                "too_large",
                format!("an entry unpacks to more than {MAX_ENTRY_BYTES} bytes"),
            ));
        }
        unpacked_total = unpacked_total.saturating_add(unpacked);
        if unpacked_total > MAX_TOTAL_BYTES {
            return Err(Refusal::new(
                "too_large",
                format!("the package unpacks to more than {MAX_TOTAL_BYTES} bytes"),
            ));
        }
        // A stored entry has equal sizes; any other method that packs to nothing is not real.
        if unpacked > 0 && packed == 0 {
            return Err(bad("an entry claims content packed into no bytes"));
        }
        if unpacked > RATIO_FLOOR && method != 0 && unpacked / packed.max(1) > MAX_RATIO {
            return Err(Refusal::new(
                "too_large",
                format!("an entry unpacks more than {MAX_RATIO} times its packed size"),
            ));
        }
        at = next;
    }
    if seen != declared {
        return Err(bad("fewer entries than the directory declares"));
    }
    Ok(Summary {
        entries: seen,
        unpacked: unpacked_total,
    })
}

/// Whether an OLE compound file (the container of old binary Office files and of password-protected
/// DOCX and XLSX) names an `EncryptedPackage` stream. Directory names are UTF-16.
pub fn ole_encrypted(bytes: &[u8]) -> bool {
    let needle: Vec<u8> = "EncryptedPackage"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    bytes.windows(needle.len()).any(|window| window == needle)
}

/// The offset of the zip end-of-central-directory record: the last one in the final
/// `EOCD_SEARCH` bytes.
fn find_end_record(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < EOCD_LEN {
        return None;
    }
    let last = bytes.len() - EOCD_LEN;
    let first = bytes.len().saturating_sub(EOCD_SEARCH);
    (first..=last)
        .rev()
        .find(|at| u32_at(bytes, *at) == EOCD_SIGNATURE)
}

/// A relative name inside the package: no `..`, no leading `/`, no `\`, no drive colon, no NUL.
fn safe_name(name: &str) -> bool {
    !name.starts_with('/')
        && !name.contains(['\\', ':', '\0'])
        && !name.split('/').any(|part| part == "..")
}

/// What an entry name says about active content, if it does (names are lower case).
fn active_content(name: &str) -> Option<&'static str> {
    if name.ends_with("vbaproject.bin")
        || name.ends_with("vbaprojectsignature.bin")
        || name.contains("/macrosheets/")
        || name.starts_with("xl/macrosheets/")
    {
        return Some("macros");
    }
    if name.starts_with("xl/externallinks/") || name.contains("/externallinks/") {
        return Some("external links");
    }
    if name.contains("/activex/") || name.starts_with("word/activex") {
        return Some("ActiveX controls");
    }
    if name.contains("/embeddings/") {
        return Some("embedded objects");
    }
    None
}
