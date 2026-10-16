//! The OOXML zip package: hostile-input preflight and part-preserving rewrite.
//!
//! A package is opened once through [`Package::open`], which enforces every cap before a part is
//! handed to a parser. An edit never rebuilds the package from a model: untouched entries are
//! copied raw (same bytes, same compression), so whatever this crate does not understand survives.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{Cursor, Read, Write};

use zip::result::ZipError;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::error::{DocsError, Result};
use crate::limits::Limits;
use crate::xmlutil::forbidden_xml;

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub name: String,
    pub is_dir: bool,
}

pub(crate) struct Package<'a> {
    bytes: &'a [u8],
    pub entries: Vec<Entry>,
    limits: Limits,
}

pub(crate) fn map_zip(e: ZipError) -> DocsError {
    match e {
        ZipError::UnsupportedArchive(m) if m.to_ascii_lowercase().contains("password") => {
            DocsError::Encrypted
        }
        ZipError::InvalidPassword => DocsError::Encrypted,
        other => DocsError::Malformed(format!("zip: {other}")),
    }
}

fn is_xml_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with(".xml") || n.ends_with(".rels") || n.ends_with(".vml")
}

fn unsafe_name(name: &str) -> bool {
    name.is_empty()
        || name.contains('\0')
        || name.starts_with('/')
        || name.starts_with('\\')
        || name.split(['/', '\\']).any(|seg| seg == "..")
        || (name.len() >= 2 && name.as_bytes()[1] == b':')
}

impl<'a> Package<'a> {
    /// Open and preflight. Enforces entry count, per-part and total decompressed size, the
    /// compression ratio, name safety, duplicate names, zip-level encryption, and refuses any XML
    /// part carrying a DOCTYPE or entity declaration. XML parts are decompressed once here, under
    /// the caps and bounded by a read limit that does not trust the declared size.
    pub fn open(bytes: &'a [u8], limits: &Limits) -> Result<Self> {
        if bytes.len() as u64 > limits.max_input_bytes {
            return Err(DocsError::TooLarge {
                limit: limits.max_input_bytes,
                actual: bytes.len() as u64,
            });
        }
        let mut zip = ZipArchive::new(Cursor::new(bytes)).map_err(map_zip)?;
        if zip.len() > limits.max_entries {
            return Err(DocsError::ArchiveLimit(format!(
                "{} entries, limit {}",
                zip.len(),
                limits.max_entries
            )));
        }
        let mut entries = Vec::with_capacity(zip.len());
        let mut seen = HashSet::new();
        let mut declared_total: u128 = 0;
        for i in 0..zip.len() {
            let f = zip.by_index_raw(i).map_err(map_zip)?;
            let name = f.name().to_string();
            if f.encrypted() {
                return Err(DocsError::Encrypted);
            }
            if unsafe_name(&name) {
                return Err(DocsError::Malformed(format!("unsafe entry name {name:?}")));
            }
            if !seen.insert(name.to_ascii_lowercase()) {
                return Err(DocsError::Malformed(format!(
                    "duplicate entry name {name:?} (part names are case-insensitive)"
                )));
            }
            let (size, csize) = (f.size(), f.compressed_size());
            if size > limits.max_part_bytes {
                return Err(DocsError::ArchiveLimit(format!(
                    "part {name} declares {size} bytes, limit {}",
                    limits.max_part_bytes
                )));
            }
            if size > (1 << 20) && size > csize.max(1).saturating_mul(limits.max_ratio) {
                return Err(DocsError::ArchiveLimit(format!(
                    "part {name} expands {size} from {csize} bytes, over {}:1",
                    limits.max_ratio
                )));
            }
            declared_total += size as u128;
            entries.push(Entry {
                name,
                is_dir: f.is_dir(),
            });
        }
        if declared_total > limits.max_total_bytes as u128 {
            return Err(DocsError::ArchiveLimit(format!(
                "package declares {declared_total} bytes, limit {}",
                limits.max_total_bytes
            )));
        }
        let compressed = bytes.len().max(1) as u128;
        if declared_total > (4 << 20) && declared_total > compressed * limits.max_ratio as u128 {
            return Err(DocsError::ArchiveLimit(format!(
                "package expands {declared_total} bytes from {compressed}, over {}:1",
                limits.max_ratio
            )));
        }
        let pkg = Package {
            bytes,
            entries,
            limits: limits.clone(),
        };
        // Real decompression of every XML part: bounded by the cap, not by the declared size.
        let mut actual_total: u64 = 0;
        let names: Vec<String> = pkg
            .entries
            .iter()
            .filter(|e| !e.is_dir && is_xml_name(&e.name))
            .map(|e| e.name.clone())
            .collect();
        for name in names {
            let data = pkg.read_bounded(&name)?;
            actual_total = actual_total.saturating_add(data.len() as u64);
            if actual_total > limits.max_total_bytes {
                return Err(DocsError::ArchiveLimit(format!(
                    "XML parts expand past {} bytes",
                    limits.max_total_bytes
                )));
            }
            if let Some(what) = forbidden_xml(&data) {
                return Err(DocsError::ForbiddenXml { part: name, what });
            }
        }
        Ok(pkg)
    }

    pub fn has(&self, name: &str) -> bool {
        self.entries.iter().any(|e| e.name == name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    fn read_bounded(&self, name: &str) -> Result<Vec<u8>> {
        let mut zip = ZipArchive::new(Cursor::new(self.bytes)).map_err(map_zip)?;
        let f = zip.by_name(name).map_err(map_zip)?;
        let cap = self.limits.max_part_bytes;
        let mut out = Vec::new();
        f.take(cap + 1)
            .read_to_end(&mut out)
            .map_err(|e| DocsError::Malformed(format!("part {name}: {e}")))?;
        if out.len() as u64 > cap {
            return Err(DocsError::ArchiveLimit(format!(
                "part {name} expands past {cap} bytes"
            )));
        }
        Ok(out)
    }

    pub fn read_opt(&self, name: &str) -> Result<Option<Vec<u8>>> {
        if !self.has(name) {
            return Ok(None);
        }
        self.read_bounded(name).map(Some)
    }

    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        self.read_opt(name)?
            .ok_or_else(|| DocsError::Malformed(format!("missing part {name}")))
    }

    /// Rebuild the package: `replace` swaps a part's bytes, `remove` drops parts, `add` appends
    /// new ones. Every other entry is copied raw, byte for byte and compression for compression.
    pub fn rewrite(
        &self,
        replace: &BTreeMap<String, Vec<u8>>,
        remove: &BTreeSet<String>,
        add: &[(String, Vec<u8>)],
    ) -> Result<Vec<u8>> {
        let mut zip = ZipArchive::new(Cursor::new(self.bytes)).map_err(map_zip)?;
        let mut out = ZipWriter::new(Cursor::new(Vec::new()));
        for i in 0..zip.len() {
            let f = zip.by_index_raw(i).map_err(map_zip)?;
            let name = f.name().to_string();
            if remove.contains(&name) {
                continue;
            }
            if let Some(data) = replace.get(&name) {
                let mut opts =
                    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
                if let Some(t) = f.last_modified() {
                    opts = opts.last_modified_time(t);
                }
                out.start_file(&name, opts).map_err(map_zip)?;
                out.write_all(data)
                    .map_err(|e| DocsError::Io(e.to_string()))?;
            } else {
                out.raw_copy_file(f).map_err(map_zip)?;
            }
        }
        for (name, data) in add {
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
            out.start_file(name, opts).map_err(map_zip)?;
            out.write_all(data)
                .map_err(|e| DocsError::Io(e.to_string()))?;
        }
        let cursor = out.finish().map_err(map_zip)?;
        Ok(cursor.into_inner())
    }
}

/// Build a package from scratch (create paths).
pub(crate) fn build(files: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut out = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in files {
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        out.start_file(name, opts).map_err(map_zip)?;
        out.write_all(data)
            .map_err(|e| DocsError::Io(e.to_string()))?;
    }
    Ok(out.finish().map_err(map_zip)?.into_inner())
}
