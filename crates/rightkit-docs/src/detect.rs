//! Format detection from content. The file name is never consulted.

use crate::error::{DocsError, Result};
use crate::limits::Limits;
use crate::package::Package;
use crate::xmlutil::{parse_rels, resolve_target};

pub(crate) struct Ooxml<'a> {
    pub pkg: Package<'a>,
    /// The main part, resolved from the package root relationships (`word/document.xml`).
    pub main: String,
}

pub(crate) enum Opened<'a> {
    Pdf,
    Docx(Ooxml<'a>),
    Xlsx(Ooxml<'a>),
}

const CFB_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

fn utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

pub(crate) fn detect<'a>(bytes: &'a [u8], limits: &Limits) -> Result<Opened<'a>> {
    if bytes.is_empty() {
        return Err(DocsError::UnsupportedFormat("empty input".into()));
    }
    if bytes.len() as u64 > limits.max_input_bytes {
        return Err(DocsError::TooLarge {
            limit: limits.max_input_bytes,
            actual: bytes.len() as u64,
        });
    }
    let head = &bytes[..bytes.len().min(1024)];
    if contains(head, b"%PDF-") {
        return Ok(Opened::Pdf);
    }
    if bytes.starts_with(&CFB_MAGIC) {
        // A password-protected OOXML file is an OLE container holding an EncryptedPackage stream.
        if contains(bytes, &utf16le("EncryptedPackage"))
            || contains(bytes, &utf16le("EncryptionInfo"))
        {
            return Err(DocsError::Encrypted);
        }
        return Err(DocsError::UnsupportedFormat(
            "legacy OLE compound file (.doc, .xls, .ppt)".into(),
        ));
    }
    if !(bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06")) {
        return Err(DocsError::UnsupportedFormat(
            "not a PDF, DOCX or XLSX".into(),
        ));
    }
    let pkg = Package::open(bytes, limits)?;

    if let Some(n) = pkg
        .names()
        .find(|n| n.to_ascii_lowercase().ends_with("vbaproject.bin"))
    {
        return Err(DocsError::MacroEnabled(format!("VBA project {n}")));
    }
    if let Some(ct) = pkg.read_opt("[Content_Types].xml")? {
        let s = String::from_utf8_lossy(&ct);
        if s.contains("macroEnabled") || s.contains("vbaProject") {
            return Err(DocsError::MacroEnabled("macro-enabled content type".into()));
        }
    }

    let mut main: Option<String> = None;
    if let Some(rels) = pkg.read_opt("_rels/.rels")? {
        for rel in parse_rels(&rels, "_rels/.rels")? {
            if rel.ty.ends_with("/officeDocument") && !rel.external {
                main = Some(resolve_target("", &rel.target));
                break;
            }
        }
    }
    let main = match main {
        Some(m) => m,
        None => {
            let w = pkg.has("word/document.xml");
            let x = pkg.has("xl/workbook.xml");
            match (w, x) {
                (true, false) => "word/document.xml".to_string(),
                (false, true) => "xl/workbook.xml".to_string(),
                (true, true) => {
                    return Err(DocsError::Malformed(
                        "package holds both a Word and an Excel main part and names neither".into(),
                    ))
                }
                _ => {
                    return Err(DocsError::UnsupportedFormat(
                        "zip archive that is not an Office document".into(),
                    ))
                }
            }
        }
    };
    let lower = main.to_ascii_lowercase();
    if lower.starts_with("word/") && lower.ends_with(".xml") {
        if !pkg.has(&main) {
            return Err(DocsError::Malformed(format!("missing main part {main}")));
        }
        Ok(Opened::Docx(Ooxml { pkg, main }))
    } else if lower.starts_with("xl/") && lower.ends_with("workbook.xml") {
        if !pkg.has(&main) {
            return Err(DocsError::Malformed(format!("missing main part {main}")));
        }
        Ok(Opened::Xlsx(Ooxml { pkg, main }))
    } else if lower.starts_with("ppt/") {
        Err(DocsError::UnsupportedFormat("PowerPoint (.pptx)".into()))
    } else if lower.ends_with(".bin") {
        Err(DocsError::UnsupportedFormat("binary Excel (.xlsb)".into()))
    } else {
        Err(DocsError::UnsupportedFormat(format!(
            "Office package with main part {main}"
        )))
    }
}
