//! `docs.read`: text out of a document inside the approved folders, tier 2 (it needs `remoteDocs`
//! as well as `remoteTools`). PDF, DOCX and XLSX, read by RightKit's `rightkit-docs`, which holds
//! the parsers, the zip and XML hardening, and the macro and encryption refusals. A PDF is read
//! page by page; a DOCX or XLSX is one page holding its whole text. Other formats are a typed
//! `unsupported_format`, never a partial or emulated read.
//!
//! The format comes from the file's bytes (magic), never its name. The file is opened through the
//! same confinement as `files.read` (`confine`: links resolved, approved folder, credential
//! folders, then a no-follow open whose handle is checked again), and a secret-named file
//! (`secret_file`) is refused whichever door asks.
//!
//! The reader is untrusted-input code, so it is bounded and contained:
//! * the file is at most `remoteDocsMaxBytes` (default 20 MiB) and is read once into memory;
//! * extraction runs on its own thread with a deadline (`READ_TIMEOUT`) and a larger stack, and a
//!   panic in the reader is caught (`catch_unwind`);
//! * at most `MAX_WORKERS` extractions run at once. A thread cannot be stopped, so one that
//!   passes the deadline keeps its slot until it ends; while the slots are taken a call is
//!   `busy`, which keeps a hostile file from piling up threads in the engine;
//! * the reply is at most `max_chars` of text (default `DEFAULT_MAX_CHARS`, never above
//!   `HARD_MAX_CHARS`, so a frame stays under the engine contract's 16 MiB).
//!
//! Errors are codes a caller can rely on: `unsupported_format`, `encrypted`, `unreadable`,
//! `timed_out`, `busy`, `active_content` (an Office package with macros, external links, embedded
//! objects or ActiveX; limits in `package`), and the shared `remote_off`, `docs_off`, `no_roots`, `outside_roots`,
//! `not_found`, `too_large`, `special_file`, `sensitive_path`, `is_directory`, `invalid_request`,
//! `read_failed`. No message carries file content, and Activity records no path or text.

use super::confine;
use super::files::secret_file;
use super::package;
use super::settings::{HARD_DOCS_MAX_BYTES, Settings};
use super::{Refusal, Result};
use rightkit_docs::{Content, DocsError, DocumentRead, Limits, ReadOptions, Source};
use serde_json::{Value, json};
use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// Longest one extraction may run before the call gives up on it.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Stack of the extraction thread: the extractor recurses through page trees and fonts.
const WORKER_STACK: usize = 32 * 1024 * 1024;
/// Extractions in flight at once, counting any that passed their deadline and still run.
const MAX_WORKERS: usize = 2;
/// Characters of text a reply holds when the caller does not say.
pub const DEFAULT_MAX_CHARS: u64 = 100_000;
/// The most a caller may ask for. Four bytes a character and six for one JSON escape keep the
/// frame under 16 MiB.
pub const HARD_MAX_CHARS: u64 = 1_000_000;
/// Longest `--pages` text.
const PAGES_MAX_LEN: usize = 32;
/// A PDF may carry junk before `%PDF-`, but only within its first 1024 bytes (ISO 32000-1 H.3).
const PDF_HEADER_WINDOW: usize = 1024;
/// How much of each end of a zip is searched for the entry names that tell DOCX from XLSX.
const ZIP_NAME_WINDOW: usize = 1024 * 1024;

/// What the bytes are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Pdf,
    Docx,
    Xlsx,
    Pptx,
    /// An old binary Office file (DOC, XLS, PPT): an OLE compound file.
    LegacyOffice,
    /// Some other zip.
    Zip,
    Unknown,
}

/// The format of `bytes` from their content.
fn detect(bytes: &[u8]) -> Format {
    if find(&bytes[..bytes.len().min(PDF_HEADER_WINDOW)], b"%PDF-") {
        return Format::Pdf;
    }
    if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        let head = &bytes[..bytes.len().min(ZIP_NAME_WINDOW)];
        let tail = &bytes[bytes.len().saturating_sub(ZIP_NAME_WINDOW)..];
        let has = |name: &[u8]| find(head, name) || find(tail, name);
        return if has(b"word/document.xml") {
            Format::Docx
        } else if has(b"xl/workbook.xml") {
            Format::Xlsx
        } else if has(b"ppt/presentation.xml") {
            Format::Pptx
        } else {
            Format::Zip
        };
    }
    if bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        return Format::LegacyOffice;
    }
    Format::Unknown
}

/// Whether `needle` occurs in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> bool {
    let Some((&first, rest)) = needle.split_first() else {
        return true;
    };
    let mut from = 0;
    while let Some(at) = hay[from..].iter().position(|b| *b == first) {
        let start = from + at + 1;
        if hay.len() - start < rest.len() {
            return false;
        }
        if &hay[start..start + rest.len()] == rest {
            return true;
        }
        from = start;
    }
    false
}

/// `docs.read`: the text of a document, by page. `pages` is `N` or `A-B` (1-based, inclusive;
/// all pages when absent). `max_chars` caps the text returned (`DEFAULT_MAX_CHARS` when absent;
/// above `HARD_MAX_CHARS` is `too_large`).
///
/// The answer: `format`, `size`, `pageCount`, `pages` (a list of `{page, text, chars, truncated}`),
/// `truncated` (some text was left out), `nextPage` (the first selected page not returned, or
/// null), `chars`, `emptyPages` (returned pages with no text in a document that has some elsewhere;
/// a document with no text on any page is refused `unreadable`, and OCR is not part of this tool), plus `bytes` and `returned` for Activity.
pub fn read(
    settings: &Settings,
    path: &str,
    pages: Option<&str>,
    max_chars: Option<u64>,
) -> Result<Value> {
    // The switches and the folders come first: a refused caller learns nothing else.
    settings.require_docs()?;
    let roots = confine::approved_roots(settings)?;
    let budget = match max_chars {
        Some(0) => {
            return Err(Refusal::new(
                "invalid_request",
                "--max-chars must be 1 or more",
            ));
        }
        Some(n) if n > HARD_MAX_CHARS => {
            return Err(Refusal::new(
                "too_large",
                format!("a reply is limited to {HARD_MAX_CHARS} characters; ask for less"),
            ));
        }
        Some(n) => n,
        None => DEFAULT_MAX_CHARS,
    };
    let range = parse_pages(pages)?;
    let resolved = confine::resolve(&roots, path)?;
    // The name as asked and the name the path resolves to (a link can point at a secret file).
    if secret_file(&resolved.path) || secret_file(Path::new(path)) {
        return Err(Refusal::new(
            "sensitive_path",
            "that file usually holds secrets and is not a document this tool reads",
        ));
    }
    let (file, meta) = confine::open_regular(&roots, &resolved)?;
    let cap = settings.docs_max_bytes;
    let too_large = || {
        Refusal::new(
            "too_large",
            format!("a document is limited to {cap} bytes (remoteDocsMaxBytes)"),
        )
    };
    if meta.len() > cap {
        return Err(too_large());
    }
    // One byte more than the cap tells a file that grew after the check.
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| Refusal::new("read_failed", format!("could not read: {}", e.kind())))?;
    if bytes.len() as u64 > cap {
        return Err(too_large());
    }
    if bytes.is_empty() {
        return Err(Refusal::new("unreadable", "the file is empty"));
    }
    let size = bytes.len();
    let format = detect(&bytes);
    // An Office package or a protected Office file is checked against the limits of `package`
    // (entries, unpacked size, ratio, encryption, active content) before anything names it, so
    // an oversized, encrypted or macro-bearing file is refused for that reason, not as an
    // unsupported format.
    match format {
        Format::Docx | Format::Xlsx | Format::Pptx => {
            package::check(&bytes)?;
        }
        Format::LegacyOffice if package::ole_encrypted(&bytes) => {
            return Err(Refusal::new(
                "encrypted",
                "the Office file is password protected; the app does not open protected documents",
            ));
        }
        _ => {}
    }
    match format {
        Format::Pdf | Format::Docx | Format::Xlsx => {}
        Format::Pptx | Format::LegacyOffice | Format::Zip | Format::Unknown => {
            return Err(Refusal::new(
                "unsupported_format",
                "docs.read reads PDF, DOCX and XLSX; this file is none of those",
            ));
        }
    }
    let (kind, all) = extract(bytes)?;
    let total = all.len() as u64;
    // A document whose every page has no text is a scan (or blank): there is nothing to read and
    // no OCR here, so it is named `unreadable` instead of returned as a list of empty pages.
    if all.iter().all(|page| page.trim().is_empty()) {
        return Err(Refusal::new(
            "unreadable",
            if kind == "pdf" {
                "the PDF has no text on any page (a scan); the app does not run OCR"
            } else {
                "the document has no text; the app does not run OCR"
            },
        ));
    }
    let (first, last) = match range {
        None => (1, total),
        Some((first, last)) => {
            if first > total {
                let what = if kind == "pdf" {
                    "the PDF"
                } else {
                    "the document"
                };
                return Err(Refusal::new(
                    "invalid_request",
                    format!("{what} has {total} pages; --pages starts after the last one"),
                ));
            }
            (first, last.min(total))
        }
    };
    let slice = window(&all, first, last, budget);
    let chars: usize = slice.pages.iter().map(|p| p.chars).sum();
    let text_bytes: usize = slice.pages.iter().map(|p| p.text.len()).sum();
    let empty = slice.pages.iter().filter(|p| p.text.is_empty()).count();
    let returned = slice.pages.len();
    let rows: Vec<Value> = slice
        .pages
        .into_iter()
        .map(|p| {
            json!({
                "page": p.page,
                "text": p.text,
                "chars": p.chars,
                "truncated": p.cut,
            })
        })
        .collect();
    Ok(json!({
        "ok": true,
        "format": kind,
        "path": confine::display(&resolved.path),
        "root": roots[resolved.root].index,
        "size": size,
        "pageCount": total,
        "pages": rows,
        "truncated": slice.cut || slice.next.is_some(),
        "nextPage": slice.next,
        "chars": chars,
        "emptyPages": empty,
        "bytes": text_bytes,
        "returned": returned,
    }))
}

/// `--pages`: `N` or `A-B`, both 1 or more and `A` not above `B`.
fn parse_pages(text: Option<&str>) -> Result<Option<(u64, u64)>> {
    let Some(text) = text.map(str::trim).filter(|t| !t.is_empty()) else {
        return Ok(None);
    };
    let bad = || {
        Refusal::new(
            "invalid_request",
            "--pages is a page number or a range such as 3-7 (pages start at 1)",
        )
    };
    if text.len() > PAGES_MAX_LEN {
        return Err(bad());
    }
    let number = |t: &str| -> Result<u64> {
        let t = t.trim();
        if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        t.parse::<u64>().ok().filter(|n| *n >= 1).ok_or_else(bad)
    };
    let (first, last) = match text.split_once('-') {
        Some((a, b)) => (number(a)?, number(b)?),
        None => {
            let n = number(text)?;
            (n, n)
        }
    };
    if first > last {
        return Err(bad());
    }
    Ok(Some((first, last)))
}

/// One returned page.
struct PageText {
    page: u64,
    text: String,
    chars: usize,
    /// The page's own text was cut to fit the budget.
    cut: bool,
}

/// The pages of `first..=last` that fit `budget` characters.
struct Window {
    pages: Vec<PageText>,
    /// The first selected page that was not returned.
    next: Option<u64>,
    /// The last page returned was cut.
    cut: bool,
}

/// Whole pages in order until the next would pass `budget`. A first page that alone passes it is
/// cut at a character boundary, so a call always makes progress.
fn window(all: &[String], first: u64, last: u64, budget: u64) -> Window {
    let budget = usize::try_from(budget).unwrap_or(usize::MAX);
    let mut out = Window {
        pages: Vec::new(),
        next: None,
        cut: false,
    };
    let mut used = 0usize;
    for page in first..=last {
        let Some(text) = all.get((page - 1) as usize) else {
            break;
        };
        let chars = text.chars().count();
        if used.saturating_add(chars) <= budget {
            used += chars;
            out.pages.push(PageText {
                page,
                text: text.clone(),
                chars,
                cut: false,
            });
            continue;
        }
        if out.pages.is_empty() {
            let kept: String = text.chars().take(budget).collect();
            out.pages.push(PageText {
                page,
                chars: budget,
                text: kept,
                cut: true,
            });
            out.cut = true;
            if page < last {
                out.next = Some(page + 1);
            }
        } else {
            out.next = Some(page);
        }
        break;
    }
    out
}

/// One slot of the extraction pool, held by the worker thread until it ends.
pub(super) struct Slot;

static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

impl Slot {
    pub(super) fn take() -> Result<Slot> {
        let mut now = IN_FLIGHT.load(Ordering::Acquire);
        loop {
            if now >= MAX_WORKERS {
                return Err(Refusal::new(
                    "busy",
                    "document readers are still busy with earlier files; try again shortly",
                ));
            }
            match IN_FLIGHT.compare_exchange(now, now + 1, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Ok(Slot),
                Err(seen) => now = seen,
            }
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The text of every page (a DOCX or XLSX is one page), read by `rightkit-docs` on a worker thread
/// with a deadline. The reader owns a copy of the bytes, so a worker that outlives its deadline
/// holds nothing of the caller's. The format name is the reply's `format`.
fn extract(bytes: Vec<u8>) -> Result<(&'static str, Vec<String>)> {
    let slot = Slot::take()?;
    let (send, receive) = mpsc::channel();
    std::thread::Builder::new()
        .name("rightkit-docs-read".into())
        .stack_size(WORKER_STACK)
        .spawn(move || {
            let _slot = slot;
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                rightkit_docs::read(Source::Bytes(&bytes), &read_options())
            }));
            let _ = send.send(outcome);
        })
        .map_err(|e| {
            Refusal::new(
                "read_failed",
                format!("could not start the reader: {}", e.kind()),
            )
        })?;
    match receive.recv_timeout(READ_TIMEOUT) {
        Ok(Ok(Ok(read))) => Ok(pages_of(read)),
        Ok(Ok(Err(error))) => Err(refusal(error)),
        Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => Err(Refusal::new(
            "unreadable",
            "the document could not be read (it is damaged or not a document the app can read)",
        )),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(Refusal::new(
            "timed_out",
            format!(
                "reading the document took longer than {} seconds and was given up",
                READ_TIMEOUT.as_secs()
            ),
        )),
    }
}

/// The reader's limits: the package limits of `package` (which hold the same numbers the check
/// above enforces) and the file cap. Text is not capped here; `window` applies `max_chars`.
fn read_options() -> ReadOptions {
    ReadOptions {
        limits: Limits {
            max_input_bytes: HARD_DOCS_MAX_BYTES,
            max_total_bytes: package::MAX_TOTAL_BYTES,
            max_part_bytes: package::MAX_ENTRY_BYTES,
            max_entries: package::MAX_ENTRIES,
            max_ratio: package::MAX_RATIO,
            ..Limits::default()
        },
        max_chars: usize::MAX,
        max_cells: usize::MAX,
        markdown: false,
        ..ReadOptions::default()
    }
}

/// The page texts of a read: a PDF's pages in order, or the one text of a DOCX or XLSX.
fn pages_of(read: DocumentRead) -> (&'static str, Vec<String>) {
    match read.content {
        Content::Pdf(pdf) => ("pdf", pdf.pages.into_iter().map(|page| page.text).collect()),
        Content::Docx(_) => ("docx", vec![read.text]),
        Content::Xlsx(_) => ("xlsx", vec![read.text]),
    }
}

/// The refusal code for a `rightkit-docs` error. Macros and encryption keep their codes;
/// limits are `too_large`, anything the reader cannot parse is `unreadable`.
pub(super) fn refusal(error: DocsError) -> Refusal {
    match error {
        DocsError::Encrypted => Refusal::new(
            "encrypted",
            "the document is encrypted; the app does not open protected documents",
        ),
        DocsError::MacroEnabled(_) => Refusal::new(
            "active_content",
            "the document carries macros, which the app does not open or write",
        ),
        DocsError::UnsupportedFormat(_) => Refusal::new(
            "unsupported_format",
            "docs.read reads PDF, DOCX and XLSX; this file is none of those",
        ),
        DocsError::TooLarge { .. } | DocsError::ArchiveLimit(_) => {
            Refusal::new("too_large", "the document is larger than the app reads")
        }
        DocsError::Io(_) => Refusal::new("read_failed", "could not read the document"),
        _ => Refusal::new(
            "unreadable",
            "the document could not be read (it is damaged or not a document the app can read)",
        ),
    }
}
