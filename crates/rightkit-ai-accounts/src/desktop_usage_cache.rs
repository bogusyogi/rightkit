//! Claude Desktop's cached usage responses on Windows, read-only.
//!
//! Desktop keeps the `GET /api/organizations/<org>/usage` responses it fetched in Chromium's
//! HTTP cache, `%APPDATA%\Claude\Cache\Cache_Data`. On Windows that is the older block-file
//! backend (not the Simple Cache the Mac reads): `data_1` holds 256-byte `EntryStore` records
//! (state, key, three stream sizes and block addresses); stream 0 is the response header block
//! (`date`, `content-encoding`) and stream 1 the body, zstd-encoded or plain JSON. The files are
//! held open by Desktop and read through shared handles; nothing is written, and no token,
//! cookie or credential is touched.
//!
//! [`usage_responses`] returns every live usage response cached for the given organizations,
//! with the date from its own `Date:` header and the decoded JSON body. What the numbers mean
//! (windows, freshness, resets) is the host's: this module only finds and decodes the bytes.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// `kBlockMagic` of Chromium's block files, and the size of their header.
const BLOCK_MAGIC: u32 = 0xC104_CAC3;
const BLOCK_HEADER_BYTES: usize = 8192;
/// `EntryStore` is exactly one 256-byte block: the key starts at byte 96 and at most 160
/// key bytes live inside it (a longer key sits in its own stream and is skipped).
const ENTRY_BYTES: usize = 256;
const ENTRY_KEY_OFFSET: usize = 96;
const MAX_INLINE_KEY: usize = 160;
const SCAN_CHUNK_ENTRIES: usize = 1024;
const MAX_SCAN_BYTES: u64 = 64 * 1024 * 1024;
/// How many chained 256-byte block files (`next_file`) one scan follows.
const MAX_CHAIN: usize = 8;
const MAX_HEADER_BYTES: usize = 32 * 1024;
const MAX_BODY_BYTES: usize = 256 * 1024;
/// The decoded body's limit: a usage response is a few kilobytes.
pub const MAX_JSON_BYTES: usize = 256 * 1024;
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// One cached usage response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedUsage {
    /// The organization the request named (lower-case canonical id).
    pub organization: String,
    /// Unix seconds from the response's own `Date:` header: when the numbers were true.
    pub captured: u64,
    /// The response body as JSON bytes (decoded when it was zstd), at most [`MAX_JSON_BYTES`].
    pub body: Vec<u8>,
}

/// A canonical 8-4-4-4-12 hex id, the shape of Claude account and organization ids.
pub fn is_account_id(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8usize, 4, 4, 4, 12])
            .all(|(part, length)| {
                part.len() == length && part.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
}

/// Every live usage response in `cache_dir` (Desktop's `Cache\Cache_Data`) for one of
/// `organizations` (lower-case ids), in cache order. `None` when the entry index (`data_1`)
/// cannot be read as a block file; an entry whose streams cannot be read or decoded is skipped.
pub fn usage_responses(cache_dir: &Path, organizations: &[String]) -> Option<Vec<CachedUsage>> {
    let candidates = scan(cache_dir, organizations)?;
    Some(
        candidates
            .iter()
            .filter_map(|candidate| read_entry(cache_dir, candidate))
            .collect(),
    )
}

/// Where one entry's two streams are: sizes and Chromium block-file addresses.
struct Candidate {
    organization: String,
    sizes: [usize; 2],
    addresses: [u32; 2],
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    let word = bytes.get(at..at + 4)?;
    Some(u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
}

/// Which organization a cache key's usage URL names, or `None` when the key is not a usage
/// request. Keys carry a partition prefix (`1/0/https://claude.ai/...`), so the URL inside is
/// matched; the host check keeps other sites' look-alike paths out.
fn usage_organization(key: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(key).ok()?;
    if !(text.contains("claude.ai") || text.contains("anthropic.com")) {
        return None;
    }
    let marker = "/api/organizations/";
    let rest = &text[text.find(marker)? + marker.len()..];
    let path = rest.split(['?', '#']).next()?;
    let mut segments = path.split('/');
    let id = segments.next()?;
    if segments.next()? != "usage" || segments.next().is_some() || !is_account_id(id) {
        return None;
    }
    Some(id.to_ascii_lowercase())
}

/// An `EntryStore` block as a candidate when it is a live usage entry of one of the
/// organizations.
fn candidate_of(entry: &[u8], organizations: &[String]) -> Option<Candidate> {
    let key_length = le32(entry, 32)? as usize;
    // State 0 is a normal entry (1 is evicted, 2 doomed); a non-zero long-key address means
    // the key is stored elsewhere.
    if le32(entry, 20)? != 0
        || key_length == 0
        || key_length > MAX_INLINE_KEY
        || le32(entry, 36)? != 0
    {
        return None;
    }
    let key = entry.get(ENTRY_KEY_OFFSET..ENTRY_KEY_OFFSET + key_length)?;
    let organization = usage_organization(key)?;
    if !organizations.contains(&organization) {
        return None;
    }
    let sizes = [le32(entry, 40)? as i32, le32(entry, 44)? as i32];
    Some(Candidate {
        organization,
        sizes: [
            usize::try_from(sizes[0]).ok()?,
            usize::try_from(sizes[1]).ok()?,
        ],
        addresses: [le32(entry, 56)?, le32(entry, 60)?],
    })
}

fn fill(file: &mut File, buffer: &mut [u8]) -> usize {
    let mut filled = 0usize;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    filled
}

/// Adds the usage entries of one 256-byte block file to `found`; the number of the next
/// file in the chain (zero for none), or `None` when this one cannot be read as such a file.
fn scan_file(
    dir: &Path,
    number: u32,
    organizations: &[String],
    found: &mut Vec<Candidate>,
) -> Option<u32> {
    let mut file = File::open(dir.join(format!("data_{number}"))).ok()?;
    let mut header = [0u8; BLOCK_HEADER_BYTES];
    file.read_exact(&mut header).ok()?;
    // Magic, major version 2, and 256-byte blocks: anything else is not a layout this reads.
    if le32(&header, 0)? != BLOCK_MAGIC
        || le32(&header, 4)? >> 16 != 2
        || le32(&header, 12)? != ENTRY_BYTES as u32
    {
        return None;
    }
    let next = i16::from_le_bytes([header[10], header[11]]);
    let mut chunk = vec![0u8; SCAN_CHUNK_ENTRIES * ENTRY_BYTES];
    let mut scanned = 0u64;
    loop {
        let filled = fill(&mut file, &mut chunk);
        for entry in chunk[..filled].chunks_exact(ENTRY_BYTES) {
            if let Some(candidate) = candidate_of(entry, organizations) {
                found.push(candidate);
            }
        }
        scanned += filled as u64;
        if filled < chunk.len() || scanned > MAX_SCAN_BYTES {
            break;
        }
    }
    Some(u32::try_from(next).unwrap_or(0))
}

/// Every live usage entry for one of the organizations. `data_1` holds the entry records; a
/// full block file chains to the next. `None` when the first is unreadable.
fn scan(dir: &Path, organizations: &[String]) -> Option<Vec<Candidate>> {
    let mut found = Vec::new();
    let mut number = 1u32;
    for round in 0..MAX_CHAIN {
        match scan_file(dir, number, organizations, &mut found) {
            Some(0) => break,
            Some(next) => number = next,
            None if round == 0 => return None,
            None => break,
        }
    }
    Some(found)
}

/// One stream's bytes from its block-file address. Bit 31 marks the address as set, bits
/// 28..30 are the file type (0 an external `f_XXXXXX` file, 2/3/4 the 256, 1024 and 4096-byte
/// block files), bits 24..25 the block count less one, 16..23 the block file's number and
/// 0..15 the first block.
fn read_stream(dir: &Path, address: u32, size: usize, max: usize) -> Option<Vec<u8>> {
    if size == 0 || size > max || address & 0x8000_0000 == 0 {
        return None;
    }
    let kind = (address >> 28) & 7;
    let (path, offset) = if kind == 0 {
        (dir.join(format!("f_{:06x}", address & 0x0FFF_FFFF)), 0u64)
    } else {
        let block = match kind {
            2 => 256u64,
            3 => 1024,
            4 => 4096,
            _ => return None,
        };
        let blocks = u64::from(((address >> 24) & 3) + 1);
        if size as u64 > blocks * block {
            return None;
        }
        let file = (address >> 16) & 0xFF;
        let start = u64::from(address & 0xFFFF);
        (
            dir.join(format!("data_{file}")),
            BLOCK_HEADER_BYTES as u64 + start * block,
        )
    };
    let mut file = File::open(path).ok()?;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut bytes = vec![0u8; size];
    file.read_exact(&mut bytes).ok()?;
    Some(bytes)
}

/// The response's own `Date:` out of the header block, a run of NUL-separated `name:value`
/// strings (lower-case names over HTTP/2 and 3, original case over HTTP/1.1).
fn header_date(block: &[u8]) -> Option<u64> {
    for needle in [b"\0date:", b"\0Date:"] {
        let Some(found) = block
            .windows(needle.len())
            .position(|window| window == needle)
        else {
            continue;
        };
        let rest = block.get(found + needle.len()..)?;
        let end = rest.iter().position(|byte| *byte == 0)?;
        if let Some(date) =
            rightkit_time::parse_http_date(std::str::from_utf8(&rest[..end]).ok()?.trim())
                .and_then(|seconds| u64::try_from(seconds).ok())
        {
            return Some(date);
        }
    }
    None
}

/// The body as JSON bytes: zstd frames are decoded (bounded), anything else is taken as is.
fn decode_body(body: Vec<u8>) -> Option<Vec<u8>> {
    if body.starts_with(&ZSTD_MAGIC) {
        zstd::bulk::decompress(&body, MAX_JSON_BYTES).ok()
    } else {
        (body.len() <= MAX_JSON_BYTES).then_some(body)
    }
}

/// One entry's date and decoded body, or `None` when anything about it cannot be read.
fn read_entry(dir: &Path, candidate: &Candidate) -> Option<CachedUsage> {
    let headers = read_stream(
        dir,
        candidate.addresses[0],
        candidate.sizes[0],
        MAX_HEADER_BYTES,
    )?;
    let captured = header_date(&headers)?;
    let body = read_stream(
        dir,
        candidate.addresses[1],
        candidate.sizes[1],
        MAX_BODY_BYTES,
    )?;
    Some(CachedUsage {
        organization: candidate.organization.clone(),
        captured,
        body: decode_body(body)?,
    })
}
