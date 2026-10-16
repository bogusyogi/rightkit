//! NTFS USN change journal: query, read, and file-record enumeration of one volume.
//!
//! The parsers and the error classification are pure and compile on every platform. The
//! volume access (`UsnJournal`) exists only on Windows. It opens `\\.\X:` and issues
//! `FSCTL_QUERY_USN_JOURNAL`, `FSCTL_READ_USN_JOURNAL` and `FSCTL_ENUM_USN_DATA` through
//! `DeviceIoControl`. Opening a volume handle needs an elevated token (or a granted
//! volume-read right): without it every call reports `UsnError::PermissionDenied`, and on a
//! volume that is not NTFS/ReFS (FAT, exFAT, network) `UsnError::Unsupported`.
//!
//! Only `USN_RECORD_V2` records are decoded (NTFS, 64-bit file reference numbers). Records of
//! other major versions are skipped by their length and counted in `UsnRecords::skipped`.
//!
//! TODO (not done here): resolving file reference numbers to paths (FRN map built from
//! `enumerate_files`, or `OpenFileById` plus `GetFinalPathNameByHandle`) and feeding
//! `Backend::watch` from this journal. Until then the Windows Search backend has no change
//! feed and callers rescan.

use crate::error::Error;
use std::fmt;

pub const USN_RECORD_V2_HEADER: usize = 60;

pub const USN_REASON_DATA_OVERWRITE: u32 = 0x0000_0001;
pub const USN_REASON_DATA_EXTEND: u32 = 0x0000_0002;
pub const USN_REASON_DATA_TRUNCATION: u32 = 0x0000_0004;
pub const USN_REASON_FILE_CREATE: u32 = 0x0000_0100;
pub const USN_REASON_FILE_DELETE: u32 = 0x0000_0200;
pub const USN_REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;
pub const USN_REASON_RENAME_NEW_NAME: u32 = 0x0000_2000;
pub const USN_REASON_CLOSE: u32 = 0x8000_0000;

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

pub const FSCTL_QUERY_USN_JOURNAL: u32 = 0x0009_00F4;
pub const FSCTL_READ_USN_JOURNAL: u32 = 0x0009_00BB;
pub const FSCTL_ENUM_USN_DATA: u32 = 0x0009_00B3;

const ERROR_INVALID_FUNCTION: u32 = 1;
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_PATH_NOT_FOUND: u32 = 3;
const ERROR_ACCESS_DENIED: u32 = 5;
#[cfg_attr(not(windows), allow(dead_code))]
const ERROR_HANDLE_EOF: u32 = 38;
const ERROR_NOT_SUPPORTED: u32 = 50;
const ERROR_JOURNAL_DELETE_IN_PROGRESS: u32 = 1178;
const ERROR_JOURNAL_NOT_ACTIVE: u32 = 1179;
const ERROR_JOURNAL_ENTRY_DELETED: u32 = 1181;
const ERROR_PRIVILEGE_NOT_HELD: u32 = 1314;

/// Why a USN operation failed.
#[derive(Debug)]
pub enum UsnError {
    /// The volume handle or a journal call was refused: run elevated.
    PermissionDenied,
    /// The volume has no USN journal support (not NTFS/ReFS) or this is not Windows.
    Unsupported(&'static str),
    /// The volume supports a journal but it is not created or being deleted.
    JournalNotActive,
    /// The requested USN fell off the journal: the consumer must rescan.
    JournalEntryDeleted,
    /// A returned buffer was malformed.
    Parse(&'static str),
    /// Any other Win32 failure.
    Io(std::io::Error),
}

impl fmt::Display for UsnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsnError::PermissionDenied => {
                write!(f, "usn journal needs an elevated volume handle (access denied)")
            }
            UsnError::Unsupported(s) => write!(f, "usn journal unsupported: {s}"),
            UsnError::JournalNotActive => write!(f, "usn journal is not active on this volume"),
            UsnError::JournalEntryDeleted => {
                write!(f, "usn fell off the journal; a full rescan is needed")
            }
            UsnError::Parse(s) => write!(f, "usn buffer: {s}"),
            UsnError::Io(e) => write!(f, "usn io: {e}"),
        }
    }
}

impl std::error::Error for UsnError {}

impl From<UsnError> for Error {
    fn from(e: UsnError) -> Self {
        match e {
            UsnError::PermissionDenied => Error::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "usn journal needs an elevated volume handle",
            )),
            UsnError::Unsupported(s) => Error::Unsupported(s),
            UsnError::JournalNotActive => Error::Unsupported("usn journal is not active"),
            UsnError::JournalEntryDeleted => Error::State("usn fell off the journal".into()),
            UsnError::Parse(s) => Error::State(format!("usn buffer: {s}")),
            UsnError::Io(e) => Error::Io(e),
        }
    }
}

/// Map a Win32 error code from the volume open or a journal ioctl to a `UsnError`.
pub fn classify_win32_error(code: u32) -> UsnError {
    match code {
        ERROR_ACCESS_DENIED | ERROR_PRIVILEGE_NOT_HELD => UsnError::PermissionDenied,
        ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED => {
            UsnError::Unsupported("volume does not support a USN journal (not NTFS or ReFS)")
        }
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => UsnError::Unsupported("no such volume"),
        ERROR_JOURNAL_NOT_ACTIVE | ERROR_JOURNAL_DELETE_IN_PROGRESS => UsnError::JournalNotActive,
        ERROR_JOURNAL_ENTRY_DELETED => UsnError::JournalEntryDeleted,
        other => UsnError::Io(std::io::Error::from_raw_os_error(other as i32)),
    }
}

/// `FSCTL_QUERY_USN_JOURNAL` result (`USN_JOURNAL_DATA_V0`, 56 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsnJournalInfo {
    pub journal_id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
    pub lowest_valid_usn: i64,
    pub max_usn: i64,
    pub maximum_size: u64,
    pub allocation_delta: u64,
}

/// One decoded `USN_RECORD_V2`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsnRecord {
    pub frn: u64,
    pub parent_frn: u64,
    pub usn: i64,
    /// FILETIME (100 ns ticks since 1601-01-01 UTC).
    pub timestamp: i64,
    pub reason: u32,
    pub source_info: u32,
    pub file_attributes: u32,
    pub name: String,
}

impl UsnRecord {
    pub fn is_dir(&self) -> bool {
        self.file_attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }
    pub fn is_create(&self) -> bool {
        self.reason & USN_REASON_FILE_CREATE != 0
    }
    pub fn is_delete(&self) -> bool {
        self.reason & USN_REASON_FILE_DELETE != 0
    }
    pub fn is_rename(&self) -> bool {
        self.reason & (USN_REASON_RENAME_OLD_NAME | USN_REASON_RENAME_NEW_NAME) != 0
    }
}

/// Decoded output of one read or enumeration ioctl.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsnRecords {
    /// First eight bytes of the output: the next USN to read (journal read) or the next file
    /// reference number to enumerate from (MFT enumeration). Reinterpret as `i64` for USNs.
    pub next: u64,
    pub records: Vec<UsnRecord>,
    /// Records of a major version other than 2, skipped by length.
    pub skipped: usize,
}

fn rd16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn rd32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn rd64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// Decode a `USN_JOURNAL_DATA_V0` output buffer.
pub fn parse_journal_data(buf: &[u8]) -> std::result::Result<UsnJournalInfo, UsnError> {
    if buf.len() < 56 {
        return Err(UsnError::Parse("journal data shorter than 56 bytes"));
    }
    Ok(UsnJournalInfo {
        journal_id: rd64(buf, 0),
        first_usn: rd64(buf, 8) as i64,
        next_usn: rd64(buf, 16) as i64,
        lowest_valid_usn: rd64(buf, 24) as i64,
        max_usn: rd64(buf, 32) as i64,
        maximum_size: rd64(buf, 40),
        allocation_delta: rd64(buf, 48),
    })
}

/// Decode the output of `FSCTL_READ_USN_JOURNAL` / `FSCTL_ENUM_USN_DATA`: an 8-byte cursor
/// followed by 8-byte-aligned `USN_RECORD_V2` records. Malformed lengths are an error; a
/// buffer holding only the cursor is a valid empty batch.
pub fn parse_usn_records(buf: &[u8]) -> std::result::Result<UsnRecords, UsnError> {
    if buf.len() < 8 {
        return Err(UsnError::Parse("buffer shorter than the 8-byte cursor"));
    }
    let next = rd64(buf, 0);
    let mut records = Vec::new();
    let mut skipped = 0usize;
    let mut at = 8usize;
    while at < buf.len() {
        if buf.len() - at < 8 {
            return Err(UsnError::Parse("truncated record header"));
        }
        let len = rd32(buf, at) as usize;
        if len < 8 || at + len > buf.len() {
            return Err(UsnError::Parse("record length out of range"));
        }
        let major = rd16(buf, at + 4);
        if major != 2 {
            skipped += 1;
        } else {
            if len < USN_RECORD_V2_HEADER {
                return Err(UsnError::Parse("v2 record shorter than its header"));
            }
            let r = &buf[at..at + len];
            let name_len = rd16(r, 56) as usize;
            let name_off = rd16(r, 58) as usize;
            if name_len % 2 != 0 || name_off < USN_RECORD_V2_HEADER || name_off + name_len > len {
                return Err(UsnError::Parse("file name outside the record"));
            }
            let units: Vec<u16> = r[name_off..name_off + name_len]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            records.push(UsnRecord {
                frn: rd64(r, 8),
                parent_frn: rd64(r, 16),
                usn: rd64(r, 24) as i64,
                timestamp: rd64(r, 32) as i64,
                reason: rd32(r, 40),
                source_info: rd32(r, 44),
                file_attributes: rd32(r, 52),
                name: String::from_utf16_lossy(&units),
            });
        }
        // Records are 8-byte aligned; RecordLength already includes the padding.
        at += (len + 7) & !7;
    }
    Ok(UsnRecords {
        next,
        records,
        skipped,
    })
}

/// Input of `FSCTL_READ_USN_JOURNAL`: `READ_USN_JOURNAL_DATA_V1` pinned to V2 records, 48 bytes.
pub fn read_journal_input(
    start_usn: i64,
    reason_mask: u32,
    journal_id: u64,
    bytes_to_wait_for: u64,
    timeout_100ns: u64,
) -> [u8; 48] {
    let mut b = [0u8; 48];
    b[0..8].copy_from_slice(&start_usn.to_le_bytes());
    b[8..12].copy_from_slice(&reason_mask.to_le_bytes());
    // ReturnOnlyOnClose = 0
    b[16..24].copy_from_slice(&timeout_100ns.to_le_bytes());
    b[24..32].copy_from_slice(&bytes_to_wait_for.to_le_bytes());
    b[32..40].copy_from_slice(&journal_id.to_le_bytes());
    b[40..42].copy_from_slice(&2u16.to_le_bytes()); // MinMajorVersion
    b[42..44].copy_from_slice(&2u16.to_le_bytes()); // MaxMajorVersion
    b
}

/// Input of `FSCTL_ENUM_USN_DATA`: `MFT_ENUM_DATA_V0`, 24 bytes.
pub fn enum_data_input(start_frn: u64, low_usn: i64, high_usn: i64) -> [u8; 24] {
    let mut b = [0u8; 24];
    b[0..8].copy_from_slice(&start_frn.to_le_bytes());
    b[8..16].copy_from_slice(&low_usn.to_le_bytes());
    b[16..24].copy_from_slice(&high_usn.to_le_bytes());
    b
}

/// `\\.\X:` for a drive letter, or None when it is not an ASCII letter.
pub fn volume_device_path(letter: char) -> Option<String> {
    letter
        .is_ascii_alphabetic()
        .then(|| format!(r"\\.\{}:", letter.to_ascii_uppercase()))
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::IO::DeviceIoControl;

    const GENERIC_READ: u32 = 0x8000_0000;
    const OUT_BUF: usize = 64 * 1024;

    fn win32_code(e: &windows::core::Error) -> u32 {
        let c = e.code().0 as u32;
        // HRESULT_FROM_WIN32 carries the code in the low 16 bits under facility 7.
        if c >> 16 == 0x8007 {
            c & 0xFFFF
        } else {
            c
        }
    }

    /// A read-only handle to one volume's USN journal.
    pub struct UsnJournal {
        h: HANDLE,
        pub letter: char,
    }

    // The handle is only used through `&self` ioctls, which the kernel serialises.
    unsafe impl Send for UsnJournal {}
    unsafe impl Sync for UsnJournal {}

    impl Drop for UsnJournal {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.h);
            }
        }
    }

    impl UsnJournal {
        /// Open the volume of `letter` (`C` opens `\\.\C:`). Not elevated: `PermissionDenied`.
        pub fn open(letter: char) -> std::result::Result<UsnJournal, UsnError> {
            let path = volume_device_path(letter)
                .ok_or(UsnError::Unsupported("not a drive letter"))?;
            let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
            let h = unsafe {
                CreateFileW(
                    PCWSTR(wide.as_ptr()),
                    GENERIC_READ,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                )
            }
            .map_err(|e| classify_win32_error(win32_code(&e)))?;
            Ok(UsnJournal { h, letter })
        }

        fn ioctl(
            &self,
            code: u32,
            input: &[u8],
            out: &mut [u8],
        ) -> std::result::Result<usize, u32> {
            let mut returned = 0u32;
            let r = unsafe {
                DeviceIoControl(
                    self.h,
                    code,
                    Some(input.as_ptr() as *const _),
                    input.len() as u32,
                    Some(out.as_mut_ptr() as *mut _),
                    out.len() as u32,
                    Some(&mut returned),
                    None,
                )
            };
            match r {
                Ok(()) => Ok(returned as usize),
                Err(e) => Err(win32_code(&e)),
            }
        }

        /// `FSCTL_QUERY_USN_JOURNAL`: journal id and USN range.
        pub fn query(&self) -> std::result::Result<UsnJournalInfo, UsnError> {
            let mut out = [0u8; 80];
            let n = self
                .ioctl(FSCTL_QUERY_USN_JOURNAL, &[], &mut out)
                .map_err(classify_win32_error)?;
            parse_journal_data(&out[..n])
        }

        /// `FSCTL_READ_USN_JOURNAL`: records from `start_usn` on. Returns an empty batch at
        /// the end of the journal; resume with `UsnRecords::next as i64`. Pass the
        /// `journal_id` from `query()`: a different id means the journal was recreated and the
        /// caller must rescan. `JournalEntryDeleted` means `start_usn` fell off the journal.
        pub fn read(
            &self,
            start_usn: i64,
            reason_mask: u32,
            journal_id: u64,
        ) -> std::result::Result<UsnRecords, UsnError> {
            let input = read_journal_input(start_usn, reason_mask, journal_id, 0, 0);
            let mut out = vec![0u8; OUT_BUF];
            let n = self
                .ioctl(FSCTL_READ_USN_JOURNAL, &input, &mut out)
                .map_err(classify_win32_error)?;
            parse_usn_records(&out[..n])
        }

        /// `FSCTL_ENUM_USN_DATA`: stream every file and folder record of the volume's MFT to
        /// `sink` (name, FRN, parent FRN, attributes). Ends cleanly at `ERROR_HANDLE_EOF`.
        /// Returns the number of records delivered. Paths are not resolved here (see the
        /// module TODO).
        pub fn enumerate_files(
            &self,
            sink: &mut dyn FnMut(&UsnRecord),
        ) -> std::result::Result<u64, UsnError> {
            let info = self.query()?;
            let mut start = 0u64;
            let mut total = 0u64;
            let mut out = vec![0u8; OUT_BUF];
            loop {
                let input = enum_data_input(start, 0, info.next_usn);
                match self.ioctl(FSCTL_ENUM_USN_DATA, &input, &mut out) {
                    Ok(n) => {
                        let batch = parse_usn_records(&out[..n])?;
                        for r in &batch.records {
                            sink(r);
                        }
                        total += batch.records.len() as u64;
                        if batch.next == start || n <= 8 {
                            return Ok(total);
                        }
                        start = batch.next;
                    }
                    Err(ERROR_HANDLE_EOF) => return Ok(total),
                    Err(c) => return Err(classify_win32_error(c)),
                }
            }
        }
    }
}

#[cfg(windows)]
pub use imp::UsnJournal;

#[cfg(test)]
mod tests {
    use super::*;

    fn record(frn: u64, parent: u64, usn: i64, reason: u32, attrs: u32, name: &str) -> Vec<u8> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let name_len = units.len() * 2;
        let len = (USN_RECORD_V2_HEADER + name_len + 7) & !7;
        let mut r = vec![0u8; len];
        r[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        r[4..6].copy_from_slice(&2u16.to_le_bytes());
        r[8..16].copy_from_slice(&frn.to_le_bytes());
        r[16..24].copy_from_slice(&parent.to_le_bytes());
        r[24..32].copy_from_slice(&usn.to_le_bytes());
        r[32..40].copy_from_slice(&123i64.to_le_bytes());
        r[40..44].copy_from_slice(&reason.to_le_bytes());
        r[52..56].copy_from_slice(&attrs.to_le_bytes());
        r[56..58].copy_from_slice(&(name_len as u16).to_le_bytes());
        r[58..60].copy_from_slice(&(USN_RECORD_V2_HEADER as u16).to_le_bytes());
        for (i, u) in units.iter().enumerate() {
            r[60 + i * 2..62 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        r
    }

    fn buffer(next: u64, recs: &[Vec<u8>]) -> Vec<u8> {
        let mut b = next.to_le_bytes().to_vec();
        for r in recs {
            b.extend_from_slice(r);
        }
        b
    }

    #[test]
    fn parses_aligned_records_in_order() {
        let b = buffer(
            4096,
            &[
                record(10, 5, 100, USN_REASON_FILE_CREATE | USN_REASON_CLOSE, 0x20, "a.txt"),
                record(11, 5, 200, USN_REASON_FILE_DELETE, 0x10, "dir\u{e9}"),
                record(12, 5, 300, USN_REASON_RENAME_NEW_NAME, 0x20, "x"),
            ],
        );
        let p = parse_usn_records(&b).unwrap();
        assert_eq!(p.next, 4096);
        assert_eq!(p.skipped, 0);
        assert_eq!(p.records.len(), 3);
        assert_eq!(p.records[0].name, "a.txt");
        assert_eq!(p.records[0].frn, 10);
        assert_eq!(p.records[0].parent_frn, 5);
        assert_eq!(p.records[0].usn, 100);
        assert_eq!(p.records[0].timestamp, 123);
        assert!(p.records[0].is_create() && !p.records[0].is_delete());
        assert!(!p.records[0].is_dir());
        assert_eq!(p.records[1].name, "dir\u{e9}");
        assert!(p.records[1].is_dir() && p.records[1].is_delete());
        assert!(p.records[2].is_rename());
    }

    #[test]
    fn cursor_only_buffer_is_an_empty_batch() {
        let p = parse_usn_records(&77u64.to_le_bytes()).unwrap();
        assert_eq!(p.next, 77);
        assert!(p.records.is_empty());
    }

    #[test]
    fn other_major_versions_are_skipped_by_length() {
        let mut v3 = record(1, 1, 1, 0, 0, "v3");
        v3[4..6].copy_from_slice(&3u16.to_le_bytes());
        let b = buffer(9, &[v3, record(2, 1, 2, 0, 0, "ok")]);
        let p = parse_usn_records(&b).unwrap();
        assert_eq!(p.skipped, 1);
        assert_eq!(p.records.len(), 1);
        assert_eq!(p.records[0].name, "ok");
    }

    #[test]
    fn malformed_buffers_are_errors_not_panics() {
        assert!(parse_usn_records(&[1, 2, 3]).is_err());
        // length runs past the buffer
        let mut r = record(1, 1, 1, 0, 0, "abc");
        let l = r.len() as u32 + 8;
        r[0..4].copy_from_slice(&l.to_le_bytes());
        assert!(parse_usn_records(&buffer(0, &[r])).is_err());
        // zero length would loop forever
        let mut z = record(1, 1, 1, 0, 0, "abc");
        z[0..4].copy_from_slice(&0u32.to_le_bytes());
        assert!(parse_usn_records(&buffer(0, &[z])).is_err());
        // name outside the record
        let mut n = record(1, 1, 1, 0, 0, "abc");
        n[56..58].copy_from_slice(&200u16.to_le_bytes());
        assert!(parse_usn_records(&buffer(0, &[n])).is_err());
        // odd name length
        let mut o = record(1, 1, 1, 0, 0, "abc");
        o[56..58].copy_from_slice(&5u16.to_le_bytes());
        assert!(parse_usn_records(&buffer(0, &[o])).is_err());
        // trailing bytes shorter than a record header
        let mut t = buffer(0, &[record(1, 1, 1, 0, 0, "a")]);
        t.extend_from_slice(&[0, 0, 0]);
        assert!(parse_usn_records(&t).is_err());
    }

    #[test]
    fn journal_data_decodes_every_field() {
        let mut b = Vec::new();
        for v in [7u64, 10, 20, 5, 99, 1 << 20, 1 << 16] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        let j = parse_journal_data(&b).unwrap();
        assert_eq!(
            j,
            UsnJournalInfo {
                journal_id: 7,
                first_usn: 10,
                next_usn: 20,
                lowest_valid_usn: 5,
                max_usn: 99,
                maximum_size: 1 << 20,
                allocation_delta: 1 << 16,
            }
        );
        assert!(parse_journal_data(&b[..55]).is_err());
    }

    #[test]
    fn ioctl_inputs_have_the_documented_layout() {
        let r = read_journal_input(-5, 0xFFFF_FFFF, 0x1122_3344_5566_7788, 0, 0);
        assert_eq!(r.len(), 48);
        assert_eq!(i64::from_le_bytes(r[0..8].try_into().unwrap()), -5);
        assert_eq!(u32::from_le_bytes(r[8..12].try_into().unwrap()), 0xFFFF_FFFF);
        assert_eq!(
            u64::from_le_bytes(r[32..40].try_into().unwrap()),
            0x1122_3344_5566_7788
        );
        assert_eq!(&r[40..44], &[2, 0, 2, 0]);
        let e = enum_data_input(5, 0, 1234);
        assert_eq!(e.len(), 24);
        assert_eq!(u64::from_le_bytes(e[0..8].try_into().unwrap()), 5);
        assert_eq!(i64::from_le_bytes(e[16..24].try_into().unwrap()), 1234);
    }

    #[test]
    fn win32_errors_classify_into_the_reported_states() {
        assert!(matches!(classify_win32_error(5), UsnError::PermissionDenied));
        assert!(matches!(classify_win32_error(1314), UsnError::PermissionDenied));
        assert!(matches!(classify_win32_error(1), UsnError::Unsupported(_)));
        assert!(matches!(classify_win32_error(1179), UsnError::JournalNotActive));
        assert!(matches!(
            classify_win32_error(1181),
            UsnError::JournalEntryDeleted
        ));
        assert!(matches!(classify_win32_error(9999), UsnError::Io(_)));
        let e: Error = UsnError::PermissionDenied.into();
        assert!(matches!(e, Error::Io(ref io) if io.kind() == std::io::ErrorKind::PermissionDenied));
        let e: Error = UsnError::Unsupported("x").into();
        assert!(matches!(e, Error::Unsupported(_)));
    }

    #[test]
    fn volume_paths_need_a_drive_letter() {
        assert_eq!(volume_device_path('c').as_deref(), Some(r"\\.\C:"));
        assert_eq!(volume_device_path('1'), None);
        assert_eq!(volume_device_path('\\'), None);
    }
}
