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

/// Embedded OLE objects and ActiveX controls: parts that embed or run foreign code. Returns the
/// reason the part is refused.
fn active_content(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    let mut segments = lower.split('/');
    let base = lower.rsplit('/').next().unwrap_or_default();
    if base.starts_with("oleobject") {
        return Some("OLE object part");
    }
    if segments.clone().any(|seg| seg == "activex") {
        return Some("ActiveX control part");
    }
    if segments.any(|seg| seg == "embeddings") {
        return Some("embedded object part");
    }
    None
}

/// Lowercased `/`-separated path with `\\` folded, `.` dropped and `..` resolved, so a relationship
/// target or part name compares the same however it is spelled.
fn normalize_target(target: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let folded = target.trim().replace('\\', "/");
    for seg in folded.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/").to_ascii_lowercase()
}

/// Relationship types that embed or run foreign code, matched on the final type segment.
fn active_relationship(rel_type: &str) -> Option<&'static str> {
    let lower = rel_type.trim().to_ascii_lowercase();
    match lower.rsplit('/').next().unwrap_or_default() {
        "oleobject" => Some("OLE object relationship"),
        "package" => Some("embedded package relationship"),
        "control" | "activexcontrolbinary" => Some("ActiveX control relationship"),
        "vbaproject" | "vbaprojectsignature" => Some("VBA project relationship"),
        _ => None,
    }
}

fn active_content_type(content_type: &str) -> Option<&'static str> {
    let lower = content_type.to_ascii_lowercase();
    let base = lower.split(';').next().unwrap_or_default().trim();
    match base {
        "application/vnd.openxmlformats-officedocument.oleobject" | "application/x-oleobject" => {
            Some("OLE object content type")
        }
        "application/vnd.ms-office.vbaproject" => Some("VBA project content type"),
        b if b.starts_with("application/vnd.ms-office.activex") => {
            Some("ActiveX control content type")
        }
        _ => None,
    }
}

/// Refuse by declaration, not filename: a `.rels` part naming an embedded-object or ActiveX
/// relationship type (or a target the filename heuristics refuse once normalized), or a
/// `[Content_Types].xml` entry with an active-content type.
fn declared_active_content(name: &str, data: &[u8]) -> Option<&'static str> {
    use quick_xml::events::Event;
    let lower = name.to_ascii_lowercase();
    let is_rels = lower.ends_with(".rels");
    let is_types = lower == "[content_types].xml";
    if !is_rels && !is_types {
        return None;
    }
    let mut reader = quick_xml::Reader::from_reader(data);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if is_rels {
                    if let Some(t) = crate::xmlutil::attr(&e, "Type") {
                        if let Some(what) = active_relationship(&t) {
                            return Some(what);
                        }
                    }
                    if let Some(t) = crate::xmlutil::attr(&e, "Target") {
                        if let Some(what) = active_content(&normalize_target(&t)) {
                            return Some(what);
                        }
                    }
                } else if let Some(c) = crate::xmlutil::attr(&e, "ContentType") {
                    if let Some(what) = active_content_type(&c) {
                        return Some(what);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
    }
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
    /// compression ratio, name safety, duplicate names, zip-level encryption, and refuses embedded
    /// OLE objects and ActiveX parts and any XML part carrying a DOCTYPE or entity declaration. XML parts are decompressed once here, under
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
            let name = f.name().map_err(map_zip)?.into_owned();
            if f.encrypted() {
                return Err(DocsError::Encrypted);
            }
            if unsafe_name(&name) {
                return Err(DocsError::Malformed(format!("unsafe entry name {name:?}")));
            }
            if let Some(what) = active_content(&name) {
                return Err(DocsError::ActiveContent { part: name, what });
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
            if let Some(what) = declared_active_content(&name, &data) {
                return Err(DocsError::ActiveContent { part: name, what });
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
            let name = f.name().map_err(map_zip)?.into_owned();
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

#[cfg(test)]
mod active_content_tests {
    use super::{active_content, declared_active_content};

    #[test]
    fn renamed_embedded_object_is_refused_by_relationship_type() {
        let rels = br#"<Relationships xmlns="x"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/oleObject" Target="media/payload.bin"/></Relationships>"#;
        assert_eq!(active_content("word/media/payload.bin"), None);
        assert_eq!(
            declared_active_content("word/_rels/document.xml.rels", rels),
            Some("OLE object relationship")
        );
        let pkg = br#"<Relationships><Relationship Type="HTTP://schemas.openxmlformats.org/officeDocument/2006/relationships/PACKAGE" Target="x"/></Relationships>"#;
        assert!(declared_active_content("word/_rels/document.xml.rels", pkg).is_some());
        let vba = br#"<Relationships><Relationship Type="http://schemas.microsoft.com/office/2006/relationships/vbaProject" Target="a.bin"/></Relationships>"#;
        assert!(declared_active_content("word/_rels/document.xml.rels", vba).is_some());
        let ok = br#"<Relationships><Relationship Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/a.png"/></Relationships>"#;
        assert_eq!(
            declared_active_content("word/_rels/document.xml.rels", ok),
            None
        );
    }

    #[test]
    fn normalized_targets_reach_filename_heuristics() {
        let rels = br#"<Relationships><Relationship Type="t/other" Target="../word/Media/../EMBEDDINGS/a.bin"/></Relationships>"#;
        assert!(declared_active_content("word/_rels/document.xml.rels", rels).is_some());
    }

    #[test]
    fn active_content_types_are_refused_regardless_of_name() {
        for ct in [
            "application/vnd.openxmlformats-officedocument.oleObject",
            "application/vnd.ms-office.activeX+xml",
            "application/vnd.ms-office.vbaProject",
            "application/x-oleObject",
        ] {
            let xml = format!(
                r#"<Types><Override PartName="/word/media/payload.bin" ContentType="{ct}"/></Types>"#
            );
            assert!(
                declared_active_content("[Content_Types].xml", xml.as_bytes()).is_some(),
                "{ct}"
            );
        }
        let ok = br#"<Types><Default Extension="png" ContentType="image/png"/></Types>"#;
        assert_eq!(declared_active_content("[Content_Types].xml", ok), None);
    }

    #[test]
    fn embedded_objects_and_activex_parts_are_named_refusals() {
        assert_eq!(
            active_content("word/embeddings/Microsoft_Excel_Worksheet.xlsx"),
            Some("embedded object part")
        );
        assert_eq!(
            active_content("word/media/oleObject2.bin"),
            Some("OLE object part")
        );
        assert_eq!(
            active_content("word/activeX/activeX1.xml"),
            Some("ActiveX control part")
        );
        assert_eq!(active_content("word/document.xml"), None);
        assert_eq!(active_content("word/media/image1.png"), None);
    }
}
