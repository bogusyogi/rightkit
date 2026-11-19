//! Limits and pure validators shared by every platform: byte caps, dimension and PNG header
//! checks, enumeration budgets, logo-path screening and rejection reason codes.
//!
//! Everything here is pure (no registry, no COM, no AppKit), so it is unit-testable on any host.
//! A rejection is always a clean "no icon" for the caller; it is reported through the host's
//! event sink as `icon_rejected` with a detail of `<place>:<reason code>` and never carries file
//! contents or paths.
#![allow(dead_code)]

use std::io::Read;
use std::time::{Duration, Instant};

pub(crate) const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Event sink type of `IconConfig::event_sink`.
pub(crate) type Sink = fn(&str, &str);

// ---- byte caps (one per kind of file) ---------------------------------------------------------

/// `AppxManifest.xml`. Real manifests are 2-60 KB; 512 KiB leaves headroom for very large
/// capability lists and still bounds memory and parse time.
pub(crate) const MAX_MANIFEST_BYTES: u64 = 512 * 1024;
/// A packaged-app logo PNG. Real logos at the sizes this crate picks are under 100 KB.
pub(crate) const MAX_LOGO_BYTES: u64 = 2 * 1024 * 1024;
/// A PNG in the persistent cache. Larger than anything this crate writes: a stored-deflate PNG
/// of `MAX_PIXELS` is about 1 MiB.
pub(crate) const MAX_CACHE_PNG_BYTES: u64 = 2 * 1024 * 1024;
/// A registry string value (a path is at most 32,767 UTF-16 units; 64 KiB is that plus slack).
pub(crate) const MAX_REG_VALUE_BYTES: u32 = 64 * 1024;
/// A Start-menu `.lnk` file handed to the shell link loader.
pub(crate) const MAX_LNK_BYTES: u64 = 1024 * 1024;
/// A host-supplied name or path (process name, executable path) and a registry- or host-supplied
/// path string. 32,767 UTF-16 units is the Windows path limit; this is that in UTF-8 bytes with
/// slack. Longer is "no icon" before it reaches a cache key, a hash or a native call.
pub(crate) const MAX_INPUT_STR_BYTES: usize = 64 * 1024;
/// A regular file whose icon is asked of the shell (Windows executable) or of AppKit (macOS path
/// that is a file, not a bundle directory). The shell maps only the resource section, so this is
/// far above any real executable (Electron apps are a few hundred MiB); larger is "no icon".
pub(crate) const MAX_ICON_SOURCE_BYTES: u64 = 1024 * 1024 * 1024;
/// macOS: a plain file path handed to `NSWorkspace iconForFile` can be an image file whose
/// thumbnail AppKit would decode, so a file (not a bundle directory) is accepted only up to this.
pub(crate) const MAX_MAC_SOURCE_FILE_BYTES: u64 = 64 * 1024 * 1024;

// ---- dimensions ------------------------------------------------------------------------------

/// `IconConfig::max_edge` is clamped to this.
pub(crate) const HARD_MAX_EDGE: u32 = 1024;
/// Largest pixel count (width * height) ever decoded or accepted.
pub(crate) const MAX_PIXELS: u64 = 512 * 512;
/// Chunks accepted in one PNG.
pub(crate) const MAX_PNG_CHUNKS: usize = 1024;

// ---- manifest parse bounds ---------------------------------------------------------------------

pub(crate) const MAX_MANIFEST_APPS: usize = 64;
pub(crate) const MAX_LOGO_PATH_BYTES: usize = 260;
pub(crate) const MAX_LOGO_COMPONENTS: usize = 8;
/// Directory entries examined in one logo directory.
pub(crate) const MAX_LOGO_DIR_ENTRIES: u32 = 1024;
/// Accepted range of a `targetsize-N` qualifier and of a computed size.
pub(crate) const MAX_QUALIFIER_SIZE: u32 = 4096;
/// Accepted range of a `scale-N` qualifier (percent).
pub(crate) const MAX_QUALIFIER_SCALE: u32 = 1000;

// ---- enumeration budgets -----------------------------------------------------------------------

/// Entries, depth and wall time one scan may spend.
pub(crate) struct ScanLimits {
    pub entries: u32,
    pub depth: u8,
    pub time: Duration,
}

/// Start-menu shortcut walk (directory entries of every kind count).
pub(crate) const START_MENU: ScanLimits = ScanLimits {
    entries: 20_000,
    depth: 8,
    time: Duration::from_secs(5),
};
/// Packaged-app search: registry keys, directory entries and manifest probes share it.
pub(crate) const PACKAGES: ScanLimits = ScanLimits {
    entries: 16_384,
    depth: 1,
    time: Duration::from_secs(4),
};
/// Manifest probe for one executable under `\WindowsApps\`.
pub(crate) const LOGO_PROBE: ScanLimits = ScanLimits {
    entries: 2048,
    depth: 1,
    time: Duration::from_secs(2),
};
/// Running-process lookup.
pub(crate) const PROCESSES: ScanLimits = ScanLimits {
    entries: 4096,
    depth: 1,
    time: Duration::from_secs(2),
};
/// Shortcuts loaded through COM in one Start-menu walk.
pub(crate) const MAX_SHORTCUTS: usize = 4000;
/// Running applications examined on macOS.
pub(crate) const MAX_RUNNING_APPS: usize = 2048;
/// Image representations examined on macOS.
pub(crate) const MAX_REPS: usize = 64;
/// Side of the square bitmap macOS rendering may allocate at most: `512 * 512 == MAX_PIXELS`.
pub(crate) const MAX_RENDER_SIDE: u32 = 512;

// ---- rejection reasons ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reject {
    Oversize,
    Unreadable,
    NotText,
    BadPath,
    LinkOrEscape,
    NotFile,
    PngSignature,
    PngStructure,
    PngCrc,
    /// IDAT stream is not valid zlib/deflate, fails its checksum, is truncated or has trailing bytes.
    PngInflate,
    /// Decoded length is not exactly what the header implies, or the header implies more than the cap.
    PngLength,
    Dimensions,
    Pixels,
    Arithmetic,
    BudgetEntries,
    BudgetTime,
}

impl Reject {
    /// Stable reason code for the event sink.
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Oversize => "oversize",
            Self::Unreadable => "unreadable",
            Self::NotText => "not_utf8",
            Self::BadPath => "bad_path",
            Self::LinkOrEscape => "link_or_escape",
            Self::NotFile => "not_file",
            Self::PngSignature => "png_signature",
            Self::PngStructure => "png_structure",
            Self::PngCrc => "png_crc",
            Self::PngInflate => "png_inflate",
            Self::PngLength => "png_length",
            Self::Dimensions => "dimensions",
            Self::Pixels => "pixels",
            Self::Arithmetic => "arithmetic",
            Self::BudgetEntries => "budget_entries",
            Self::BudgetTime => "budget_time",
        }
    }

    /// Map a `rightkit_fs::contain` error. A missing file is `Unreadable` (common, not reported).
    pub(crate) fn from_contain(error: &rightkit_fs::ContainError) -> Self {
        use rightkit_fs::ContainError as E;
        match error {
            E::InvalidRelative | E::NotAbsolute => Self::BadPath,
            E::Escapes | E::Symlink | E::HardLinked => Self::LinkOrEscape,
            E::NotAFile => Self::NotFile,
            _ => Self::Unreadable,
        }
    }
}

/// Event-sink detail: `<place>:<reason code>`. Never contains a path or file contents.
pub(crate) fn detail(place: &str, reject: Reject) -> String {
    format!("{place}:{}", reject.code())
}

/// Report a rejection through the sink. A plain "cannot be read" (usually "not there") is not
/// an event.
pub(crate) fn report(sink: Sink, place: &str, reject: Reject) {
    if reject != Reject::Unreadable {
        sink("icon_rejected", &detail(place, reject));
    }
}

// ---- budget --------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    Entries,
    Time,
}

impl Stop {
    pub(crate) fn reject(self) -> Reject {
        match self {
            Self::Entries => Reject::BudgetEntries,
            Self::Time => Reject::BudgetTime,
        }
    }
}

/// Accounting for one bounded scan: an entry allowance, a maximum depth and a deadline. The
/// clock is passed in so the accounting is testable.
#[derive(Debug)]
pub(crate) struct Budget {
    entries_left: u32,
    spent: u32,
    max_depth: u8,
    deadline: Instant,
}

impl Budget {
    pub(crate) fn new(limits: &ScanLimits, now: Instant) -> Self {
        Self {
            entries_left: limits.entries,
            spent: 0,
            max_depth: limits.depth,
            // An unrepresentable deadline means "already out of time", never "unbounded".
            deadline: now.checked_add(limits.time).unwrap_or(now),
        }
    }

    pub(crate) fn start(limits: &ScanLimits) -> Self {
        Self::new(limits, Instant::now())
    }

    /// Spend one entry. Time is checked first and does not consume an entry.
    pub(crate) fn charge(&mut self, now: Instant) -> Result<(), Stop> {
        if now >= self.deadline {
            return Err(Stop::Time);
        }
        if self.entries_left == 0 {
            return Err(Stop::Entries);
        }
        self.entries_left -= 1;
        self.spent += 1;
        Ok(())
    }

    pub(crate) fn exhausted(&self, now: Instant) -> bool {
        now >= self.deadline || self.entries_left == 0
    }

    /// Directories deeper than the limit are skipped (roots are depth 0).
    pub(crate) fn depth_allowed(&self, depth: u8) -> bool {
        depth <= self.max_depth
    }

    pub(crate) fn spent(&self) -> u32 {
        self.spent
    }
}

// ---- bounded reads ---------------------------------------------------------------------------------

/// Read at most `cap` bytes. A source with more than `cap` bytes is `Oversize` (never a prefix).
/// Callers that have a file length check it first; this also bounds a source that grows or lies.
pub(crate) fn read_capped<R: Read>(reader: R, cap: u64) -> Result<Vec<u8>, Reject> {
    let mut buf = Vec::new();
    reader
        .take(cap.saturating_add(1))
        .read_to_end(&mut buf)
        .map_err(|_| Reject::Unreadable)?;
    if buf.len() as u64 > cap {
        return Err(Reject::Oversize);
    }
    Ok(buf)
}

// ---- link policy -------------------------------------------------------------------------------------

/// Open options for every file read out of a package or the cache: read-only, no link or reparse
/// point followed at open time (that is `open_confined` itself), and a file with more than one hard
/// link refused (`ContainError::HardLinked`, checked on the opened handle). The package root is not
/// trusted: it comes from the per-user registry or from a host-supplied path, both writable by the
/// user, and nothing in this crate can establish that a package file is immutable, so there is no
/// "trusted package root" exemption. A genuinely hard-linked package file is "no icon".
pub(crate) fn confined_read_options() -> rightkit_fs::contain::ConfinedOptions {
    rightkit_fs::contain::ConfinedOptions::read_only().require_single_link(true)
}

/// Options for a Start-menu `.lnk`: no link followed, hard links allowed. The Start-menu folders are
/// user-writable, so an attacker could place any shortcut there directly; a hard link adds nothing,
/// and the read is byte-capped and parsed from memory.
pub(crate) fn shortcut_read_options() -> rightkit_fs::contain::ConfinedOptions {
    rightkit_fs::contain::ConfinedOptions::read_only()
}

/// A path or name handed in by the host (or read from the registry) is usable only up to
/// `MAX_INPUT_STR_BYTES` and without a NUL.
pub(crate) fn input_str_ok(text: &str) -> bool {
    text.len() <= MAX_INPUT_STR_BYTES && !text.contains('\0')
}

/// A path whose icon may be asked of the shell or AppKit: it must exist, be a directory (a macOS
/// bundle; a Windows folder icon) or a regular file of at most `file_cap` bytes. Anything else
/// (device, FIFO) is refused. Reparse points and links are followed by the metadata call, as the
/// shell would follow them; the pixel output is bounded separately by `check_dimensions`.
pub(crate) fn source_acceptable(path: &std::path::Path, file_cap: u64) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => true,
        Ok(meta) => meta.is_file() && meta.len() <= file_cap,
        Err(_) => false,
    }
}

/// Edge of the square bitmap macOS may be asked to render for `max_edge`: never above `max_edge`,
/// and its square never above `MAX_PIXELS`.
pub(crate) fn render_side(max_edge: u32) -> u32 {
    max_edge.clamp(1, HARD_MAX_EDGE).min(MAX_RENDER_SIDE)
}

/// Open `path` through `roots` (no link followed at open time, nothing outside the root) and read
/// it with a byte cap.
pub(crate) fn read_confined(
    roots: &rightkit_fs::Roots,
    path: &std::path::Path,
    options: &rightkit_fs::contain::ConfinedOptions,
    cap: u64,
) -> Result<Vec<u8>, Reject> {
    let file = roots
        .open_confined(path, options)
        .map_err(|e| Reject::from_contain(&e))?;
    let len = file.metadata().map_err(|_| Reject::Unreadable)?.len();
    if len > cap {
        return Err(Reject::Oversize);
    }
    read_capped(file, cap)
}

/// True for a symbolic link or, on Windows, any reparse point (junction, mount point, cloud stub).
pub(crate) fn is_link_or_reparse(meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

// ---- dimensions ------------------------------------------------------------------------------------

/// Width and height must be positive, within `max_edge` (itself clamped to `HARD_MAX_EDGE`) and
/// within `MAX_PIXELS` together. Checked before any pixel buffer is allocated.
pub(crate) fn check_dimensions(
    width: i64,
    height: i64,
    max_edge: u32,
) -> Result<(u32, u32), Reject> {
    if width <= 0 || height <= 0 {
        return Err(Reject::Dimensions);
    }
    let edge = i64::from(max_edge.min(HARD_MAX_EDGE));
    if width > edge || height > edge {
        return Err(Reject::Dimensions);
    }
    let pixels = (width as u64)
        .checked_mul(height as u64)
        .ok_or(Reject::Pixels)?;
    if pixels > MAX_PIXELS {
        return Err(Reject::Pixels);
    }
    Ok((width as u32, height as u32))
}

/// Byte length of an RGBA/BGRA buffer for already checked dimensions, with checked arithmetic.
pub(crate) fn rgba_len(width: u32, height: u32) -> Option<usize> {
    (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)
}

// ---- PNG validation ----------------------------------------------------------------------------------

fn be32(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?))
}

fn valid_color_and_depth(color: u8, depth: u8) -> bool {
    match color {
        0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
        2 | 4 | 6 => matches!(depth, 8 | 16),
        3 => matches!(depth, 1 | 2 | 4 | 8),
        _ => false,
    }
}

fn zlib_header_ok(data: &[u8]) -> bool {
    match data {
        [cmf, flg, ..] => {
            cmf & 0x0F == 8
                && cmf >> 4 <= 7
                && ((u16::from(*cmf) << 8) | u16::from(*flg)) % 31 == 0
                && flg & 0x20 == 0
        }
        _ => false,
    }
}

/// Validate a PNG end to end: signature, byte cap, IHDR first with sane dimensions (checked
/// against `max_edge` and `MAX_PIXELS` before anything is decoded), every chunk length checked
/// against the remaining bytes with checked arithmetic, every chunk CRC, contiguous IDAT, PLTE
/// rules, no unknown critical chunk, IEND last with nothing after it; and then the compressed
/// data: the IDAT payloads are concatenated and inflated with a hard output cap equal to the exact
/// decoded length the header implies (`decode::expected_len`, Adam7 included). A bad zlib stream
/// or checksum, trailing bytes after the zlib stream, a decoded length that is short or long by
/// even one byte, a filter byte above 4, or arithmetic overflow is a rejection. Returns the
/// dimensions.
pub(crate) fn validate_png(
    bytes: &[u8],
    max_edge: u32,
    max_bytes: u64,
) -> Result<(u32, u32), Reject> {
    if bytes.len() as u64 > max_bytes {
        return Err(Reject::Oversize);
    }
    if bytes.len() < PNG_MAGIC.len() || bytes[..PNG_MAGIC.len()] != PNG_MAGIC {
        return Err(Reject::PngSignature);
    }
    let structure = Reject::PngStructure;
    let mut pos = PNG_MAGIC.len();
    let mut chunks = 0usize;
    let mut header: Option<crate::decode::Header> = None;
    let mut palette_entries = 0usize;
    let mut idat = 0u8; // 0 none yet, 1 in progress, 2 finished
    let mut idat_data: Vec<u8> = Vec::new(); // bounded by `bytes.len()`, itself by `max_bytes`
    let mut ended = false;
    while pos < bytes.len() {
        if ended {
            return Err(structure); // data after IEND
        }
        chunks += 1;
        if chunks > MAX_PNG_CHUNKS {
            return Err(structure);
        }
        let head_end = pos.checked_add(8).ok_or(structure)?;
        if head_end > bytes.len() {
            return Err(structure);
        }
        let length = be32(&bytes[pos..]).ok_or(structure)?;
        if length > 0x7FFF_FFFF {
            return Err(structure);
        }
        let length = length as usize;
        let kind: [u8; 4] = bytes[pos + 4..head_end].try_into().map_err(|_| structure)?;
        if !kind.iter().all(u8::is_ascii_alphabetic) {
            return Err(structure);
        }
        let data_end = head_end.checked_add(length).ok_or(structure)?;
        let crc_end = data_end.checked_add(4).ok_or(structure)?;
        if crc_end > bytes.len() {
            return Err(structure);
        }
        let data = &bytes[head_end..data_end];
        let stored = be32(&bytes[data_end..]).ok_or(structure)?;
        if crate::png::crc32(&[&kind, data]) != stored {
            return Err(Reject::PngCrc);
        }
        if &kind != b"IDAT" && idat == 1 {
            idat = 2;
        }
        if chunks == 1 && &kind != b"IHDR" {
            return Err(structure);
        }
        match &kind {
            b"IHDR" => {
                if chunks != 1 || data.len() != 13 {
                    return Err(structure);
                }
                let width = be32(data).ok_or(structure)?;
                let height = be32(&data[4..]).ok_or(structure)?;
                let (width, height) =
                    check_dimensions(i64::from(width), i64::from(height), max_edge)?;
                let (depth, ct, compression, filter, interlace) =
                    (data[8], data[9], data[10], data[11], data[12]);
                if !valid_color_and_depth(ct, depth)
                    || compression != 0
                    || filter != 0
                    || interlace > 1
                {
                    return Err(structure);
                }
                header = Some(crate::decode::Header {
                    width,
                    height,
                    depth,
                    color: ct,
                    interlace,
                });
            }
            b"PLTE" => {
                let h = header.as_ref().ok_or(structure)?;
                if palette_entries != 0
                    || idat != 0
                    || data.is_empty()
                    || data.len() % 3 != 0
                    || data.len() > 768
                    || h.color == 0
                    || h.color == 4
                    // A palette cannot have more entries than the bit depth can index.
                    || (h.color == 3 && data.len() / 3 > (1usize << h.depth))
                {
                    return Err(structure);
                }
                palette_entries = data.len() / 3;
            }
            b"IDAT" => {
                if idat == 2 {
                    return Err(structure); // IDAT chunks must be contiguous
                }
                if idat == 0 {
                    let h = header.as_ref().ok_or(structure)?;
                    // An indexed image needs its palette before the first IDAT. The zlib
                    // header is NOT checked here: a first IDAT chunk may legally be empty or
                    // one byte long (W3C PNG 11.2.3, 12.8); it is checked on the
                    // concatenated stream below.
                    if h.color == 3 && palette_entries == 0 {
                        return Err(structure);
                    }
                    idat = 1;
                }
                idat_data.extend_from_slice(data);
            }
            b"IEND" => {
                if !data.is_empty() || idat == 0 {
                    return Err(structure);
                }
                ended = true;
            }
            _ => {
                if kind[0] & 0x20 == 0 {
                    return Err(structure); // unknown critical chunk
                }
            }
        }
        pos = crc_end;
    }
    if !ended {
        return Err(structure);
    }
    let header = header.ok_or(structure)?;
    // The zlib stream is the concatenation of every IDAT chunk's data.
    if !zlib_header_ok(&idat_data) {
        return Err(structure);
    }
    crate::decode::validate_idat(&header, &idat_data)?;
    Ok((header.width, header.height))
}

// ---- manifest logo path screening ----------------------------------------------------------------------

fn is_reserved_windows_name(segment: &str) -> bool {
    let base = segment
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ');
    let upper = base.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((upper.starts_with("COM") || upper.starts_with("LPT"))
        && upper.len() == 4
        && upper.as_bytes()[3].is_ascii_digit()
        && upper.as_bytes()[3] != b'0')
}

/// Screen a logo path taken from a manifest and return its plain components. Refused: empty,
/// over-long, NUL or control characters, `: * ? " < > |` (drive prefixes, alternate data streams,
/// wildcards), a leading separator (rooted or UNC), `..`, a Windows device name, a trailing dot
/// or space, more than `MAX_LOGO_COMPONENTS` components. Either separator is accepted and `.` and
/// empty segments are dropped. Screening is the first of three layers; the second is
/// `rightkit_fs::contain` through `Roots`, the third a no-link open (see `manifest.rs`).
pub(crate) fn logo_rel_components(logo: &str) -> Result<Vec<String>, Reject> {
    if logo.is_empty() || logo.len() > MAX_LOGO_PATH_BYTES {
        return Err(Reject::BadPath);
    }
    if logo
        .chars()
        .any(|c| c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
    {
        return Err(Reject::BadPath);
    }
    if logo.starts_with(['\\', '/']) {
        return Err(Reject::BadPath);
    }
    let mut out = Vec::new();
    for segment in logo.split(['\\', '/']) {
        match segment {
            "" | "." => continue,
            ".." => return Err(Reject::BadPath),
            _ => {}
        }
        if segment.ends_with('.') || segment.ends_with(' ') || is_reserved_windows_name(segment) {
            return Err(Reject::BadPath);
        }
        out.push(segment.to_string());
        if out.len() > MAX_LOGO_COMPONENTS {
            return Err(Reject::BadPath);
        }
    }
    if out.is_empty() {
        return Err(Reject::BadPath);
    }
    Ok(out)
}

/// Whether a `PackageRootFolder` read from the (user-writable) registry is acceptable as a root to
/// read under: a local drive-letter absolute path, no `..`, no NUL, no UNC or device prefix.
pub(crate) fn package_root_acceptable(root: &str) -> bool {
    let b = root.as_bytes();
    if root.is_empty() || root.len() > 1024 || root.contains('\0') {
        return false;
    }
    if !(b.len() >= 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b[2] == b'\\' || b[2] == b'/'))
    {
        return false;
    }
    !root[3..].split(['\\', '/']).any(|seg| seg == "..")
}

/// A registry string value is read only when its type is REG_SZ (1) or REG_EXPAND_SZ (2) and its
/// size is within `MAX_REG_VALUE_BYTES`.
pub(crate) fn registry_value_ok(kind: u32, size: u32) -> bool {
    (kind == 1 || kind == 2) && size > 0 && size <= MAX_REG_VALUE_BYTES
}

// ---- bounded directory walk ------------------------------------------------------------------------------

/// Walk the files below `roots` within `budget`: every entry of every kind costs one unit, a
/// directory deeper than the budget's maximum depth is skipped, links and reparse points are
/// neither followed nor visited, so a cycle through one cannot loop. `visit` returns `true` to
/// stop. `Err` means the budget ran out (the partial result stands).
pub(crate) fn walk_files(
    roots: Vec<std::path::PathBuf>,
    budget: &mut Budget,
    mut visit: impl FnMut(&std::path::Path, &std::fs::Metadata) -> bool,
) -> Result<(), Stop> {
    let mut stack: Vec<(std::path::PathBuf, u8)> = roots.into_iter().map(|r| (r, 0)).collect();
    while let Some((dir, depth)) = stack.pop() {
        if !budget.depth_allowed(depth) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for item in entries {
            budget.charge(Instant::now())?;
            let Ok(entry) = item else { continue };
            let Ok(meta) = entry.metadata() else { continue };
            if is_link_or_reparse(&meta) {
                continue;
            }
            if meta.is_dir() {
                stack.push((entry.path(), depth.saturating_add(1)));
            } else if meta.is_file() && visit(&entry.path(), &meta) {
                return Ok(());
            }
        }
    }
    Ok(())
}
