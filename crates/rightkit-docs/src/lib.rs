//! RightKit documents: read and write DOCX, XLSX and PDF behind one typed API, for agents and
//! apps that must not trust the files they open.
//!
//! ```no_run
//! use std::path::Path;
//! use rightkit_docs::{read, write, Block, Op, ReadOptions, Source, WriteOptions};
//!
//! let path = Path::new("report.docx");
//! write(path, &[Op::CreateDocx { blocks: vec![Block::Heading { level: 1, text: "Q3".into() }] }],
//!       &WriteOptions::default())?;
//! let doc = read(Source::Path(path), &ReadOptions::default())?;
//! // Edit against exactly what was read: refuses if the file changed in between.
//! write(path, &[Op::ReplaceText { find: "Q3".into(), replace: "Q4".into() }],
//!       &WriteOptions { expect_sha256: Some(doc.sha256), ..Default::default() })?;
//! # Ok::<(), rightkit_docs::DocsError>(())
//! ```
//!
//! | module   | responsibility                                                              |
//! |----------|-----------------------------------------------------------------------------|
//! | `detect` | format from content: PDF, DOCX, XLSX; refuses macros, encryption, the rest   |
//! | `package`| OOXML zip preflight (entries, sizes, ratio, names, DOCTYPE) and raw rewrite  |
//! | `docx`   | read, create, append blocks, replace text by byte-range patch                |
//! | `xlsx`   | read (calamine), create (rust_xlsxwriter), set cells and append rows in place|
//! | `pdf`    | facade over `rightkit-pdf`: extract, replace text, add text, select pages    |
//!
//! Every write is built in memory, validated by re-detecting it, and published once through
//! `rightkit_fs::AtomicWriter` after a last source-hash recheck.

mod detect;
mod docx;
mod error;
mod limits;
mod model;
mod package;
mod pdf;
mod xlsx;
mod xmlutil;

use std::collections::BTreeSet;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

pub use error::{DocsError, Result};
pub use limits::Limits;
pub use model::*;

use detect::{detect, Opened};

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn read_file(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let meta = fs::metadata(path).map_err(|e| DocsError::Io(format!("{}: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(DocsError::Io(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if meta.len() > limit {
        return Err(DocsError::TooLarge {
            limit,
            actual: meta.len(),
        });
    }
    let file =
        fs::File::open(path).map_err(|e| DocsError::Io(format!("{}: {e}", path.display())))?;
    let mut buf = Vec::with_capacity(meta.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut buf)
        .map_err(|e| DocsError::Io(format!("{}: {e}", path.display())))?;
    if buf.len() as u64 > limit {
        return Err(DocsError::TooLarge {
            limit,
            actual: buf.len() as u64,
        });
    }
    Ok(buf)
}

/// Cut `s` to at most `max` characters on a character boundary.
fn cap(mut s: String, max: usize) -> (String, bool) {
    match s.char_indices().nth(max) {
        Some((i, _)) => {
            s.truncate(i);
            (s, true)
        }
        None => (s, false),
    }
}

/// Read a DOCX, XLSX or PDF. The format comes from the bytes; the file name is never consulted.
pub fn read(source: Source<'_>, options: &ReadOptions) -> Result<DocumentRead> {
    let owned;
    let bytes: &[u8] = match source {
        Source::Bytes(b) => b,
        Source::Path(p) => {
            owned = read_file(p, options.limits.max_input_bytes)?;
            &owned
        }
    };
    let sha = sha256_hex(bytes);
    let size = bytes.len() as u64;
    match detect(bytes, &options.limits)? {
        Opened::Pdf => {
            let r = pdf::read(bytes, &options.limits, &options.pages, options.max_chars)?;
            Ok(DocumentRead {
                format: Format::Pdf,
                sha256: sha,
                size_bytes: size,
                text: r.text,
                markdown: options.markdown.then_some(r.markdown),
                truncated: r.truncated,
                content: Content::Pdf(r.content),
                omitted: r.omitted,
            })
        }
        Opened::Docx(o) => {
            let r = docx::read(&o, options.max_chars)?;
            let (text, t1) = cap(docx::plain_blocks(&r.content.blocks), options.max_chars);
            let (md, t2) = if options.markdown {
                let (m, t) = cap(docx::markdown_blocks(&r.content.blocks), options.max_chars);
                (Some(m), t)
            } else {
                (None, false)
            };
            Ok(DocumentRead {
                format: Format::Docx,
                sha256: sha,
                size_bytes: size,
                text,
                markdown: md,
                truncated: r.truncated || t1 || t2,
                content: Content::Docx(r.content),
                omitted: r.omitted,
            })
        }
        Opened::Xlsx(o) => {
            let r = xlsx::read(&o, bytes, options)?;
            let (text, md) = xlsx::render(&r.content);
            let (text, t1) = cap(text, options.max_chars);
            let (md, t2) = if options.markdown {
                let (m, t) = cap(md, options.max_chars);
                (Some(m), t)
            } else {
                (None, false)
            };
            Ok(DocumentRead {
                format: Format::Xlsx,
                sha256: sha,
                size_bytes: size,
                text,
                markdown: md,
                truncated: r.truncated || t1 || t2,
                content: Content::Xlsx(r.content),
                omitted: r.omitted,
            })
        }
    }
}

fn op_name(op: &Op) -> &'static str {
    match op {
        Op::CreateDocx { .. } => "createDocx",
        Op::CreateXlsx { .. } => "createXlsx",
        Op::ReplaceText { .. } => "replaceText",
        Op::AppendBlocks { .. } => "appendBlocks",
        Op::SetCells { .. } => "setCells",
        Op::AppendRows { .. } => "appendRows",
        Op::AddPdfText { .. } => "addPdfText",
        Op::SelectPdfPages { .. } => "selectPdfPages",
    }
}

#[derive(Default)]
struct Totals {
    replacements: usize,
    cells_set: usize,
    rows_appended: usize,
    touched: BTreeSet<String>,
    losses: Vec<Loss>,
    warnings: Vec<String>,
}

impl Totals {
    fn absorb(&mut self, o: docx::EditOutcome) -> Vec<u8> {
        self.touched.extend(o.touched);
        for l in o.losses {
            if !self.losses.contains(&l) {
                self.losses.push(l);
            }
        }
        for w in o.warnings {
            if !self.warnings.contains(&w) {
                self.warnings.push(w);
            }
        }
        o.bytes
    }
}

fn wrong(op: &Op, fmt: Format) -> DocsError {
    DocsError::WrongFormat {
        op: op_name(op),
        format: fmt.as_str(),
    }
}

/// Apply one edit op to `current`, whose format is detected fresh.
fn apply(
    op: &Op,
    current: &[u8],
    limits: &Limits,
    totals: &mut Totals,
) -> Result<(Vec<u8>, Format)> {
    match (op, detect(current, limits)?) {
        (Op::ReplaceText { find, replace }, Opened::Docx(o)) => {
            let out = docx::replace_text(&o, find, replace)?;
            totals.replacements += out.count;
            Ok((totals.absorb(out), Format::Docx))
        }
        (Op::AppendBlocks { blocks }, Opened::Docx(o)) => {
            let out = docx::append_blocks(&o, blocks)?;
            Ok((totals.absorb(out), Format::Docx))
        }
        (Op::SetCells { sheet, cells }, Opened::Xlsx(o)) => {
            let (out, n) = xlsx::set_cells(&o, sheet, cells)?;
            totals.cells_set += n;
            Ok((totals.absorb(out), Format::Xlsx))
        }
        (Op::AppendRows { sheet, rows }, Opened::Xlsx(o)) => {
            let (out, n) = xlsx::append_rows(&o, sheet, rows)?;
            totals.rows_appended += n;
            Ok((totals.absorb(out), Format::Xlsx))
        }
        (Op::ReplaceText { find, replace }, Opened::Pdf) => {
            let out = pdf::replace_text(current, limits, find, replace)?;
            totals.replacements += out.count;
            Ok((totals.absorb(out), Format::Pdf))
        }
        (
            Op::AddPdfText {
                page,
                x,
                y,
                text,
                size_pt,
            },
            Opened::Pdf,
        ) => {
            let out = pdf::add_text(current, limits, *page, *x, *y, text, *size_pt)?;
            Ok((totals.absorb(out), Format::Pdf))
        }
        (Op::SelectPdfPages { pages }, Opened::Pdf) => {
            let out = pdf::select_pages(current, limits, pages)?;
            Ok((totals.absorb(out), Format::Pdf))
        }
        (Op::CreateDocx { .. } | Op::CreateXlsx { .. }, _) => Err(DocsError::InvalidOp(
            "a create op is only valid as the first op".into(),
        )),
        (op, Opened::Pdf) => Err(wrong(op, Format::Pdf)),
        (op, Opened::Docx(_)) => Err(wrong(op, Format::Docx)),
        (op, Opened::Xlsx(_)) => Err(wrong(op, Format::Xlsx)),
    }
}

fn format_of(bytes: &[u8], limits: &Limits) -> Result<Format> {
    Ok(match detect(bytes, limits)? {
        Opened::Pdf => Format::Pdf,
        Opened::Docx(_) => Format::Docx,
        Opened::Xlsx(_) => Format::Xlsx,
    })
}

fn fs_err(e: rightkit_fs::FsError) -> DocsError {
    if matches!(e, rightkit_fs::FsError::AlreadyExists(_)) {
        return DocsError::TargetExists;
    }
    if e.target_replaced() {
        return DocsError::SavedWithWarning(e.to_string());
    }
    DocsError::Io(e.to_string())
}

/// Apply typed ops to `target` and publish the result atomically.
///
/// A leading `CreateDocx` or `CreateXlsx` makes a new file (the target must not exist unless
/// `options.overwrite`); any other first op edits the existing file. Ops apply in order to one
/// in-memory document; nothing touches the disk until every op has succeeded and the result has
/// been re-detected as the same format. An edit then rechecks the target's SHA-256 (against
/// `options.expect_sha256` when given, always against the bytes this call started from) as the
/// last step before the rename, so a file changed meanwhile is never overwritten
/// (`source_changed`). The receipt lists everything the edit could not keep.
pub fn write(target: &Path, ops: &[Op], options: &WriteOptions) -> Result<WriteReceipt> {
    let limits = &options.limits;
    let first = ops
        .first()
        .ok_or_else(|| DocsError::InvalidOp("no ops".into()))?;
    let creating = matches!(first, Op::CreateDocx { .. } | Op::CreateXlsx { .. });

    // One resolution of the path: a symlink target is replaced where it points, the link stays.
    let existing = fs::symlink_metadata(target).is_ok();
    let real: PathBuf = if existing {
        fs::canonicalize(target).map_err(|e| DocsError::Io(format!("{}: {e}", target.display())))?
    } else {
        target.to_path_buf()
    };

    let mut totals = Totals::default();
    let (mut current, mut format, source_sha): (Vec<u8>, Format, Option<String>) = if creating {
        let source_sha = if existing {
            if !options.overwrite {
                return Err(DocsError::TargetExists);
            }
            let old = read_file(&real, limits.max_input_bytes)?;
            let h = sha256_hex(&old);
            if let Some(expect) = &options.expect_sha256 {
                if *expect != h {
                    return Err(DocsError::SourceChanged);
                }
            }
            Some(h)
        } else {
            None
        };
        match first {
            Op::CreateDocx { blocks } => (docx::create(blocks)?, Format::Docx, source_sha),
            Op::CreateXlsx { sheets } => (xlsx::create(sheets)?, Format::Xlsx, source_sha),
            _ => unreachable!("creating implies a create op"),
        }
    } else {
        if !existing {
            return Err(DocsError::TargetMissing);
        }
        let bytes = read_file(&real, limits.max_input_bytes)?;
        let h = sha256_hex(&bytes);
        if let Some(expect) = &options.expect_sha256 {
            if *expect != h {
                return Err(DocsError::SourceChanged);
            }
        }
        let fmt = format_of(&bytes, limits)?;
        (bytes, fmt, Some(h))
    };

    let start = usize::from(creating);
    for op in &ops[start..] {
        let (next, fmt) = apply(op, &current, limits, &mut totals)?;
        current = next;
        format = fmt;
    }
    if creating {
        totals.touched.insert("(new document)".into());
    }

    // The output must be a document this crate itself accepts, in the format it started as.
    if format_of(&current, limits)? != format {
        return Err(DocsError::Malformed(
            "the edit produced a different kind of document; nothing was written".into(),
        ));
    }
    if current.len() as u64 > limits.max_input_bytes {
        return Err(DocsError::TooLarge {
            limit: limits.max_input_bytes,
            actual: current.len() as u64,
        });
    }

    // Publish. Last source recheck runs inside the writer, after the temp file is complete and
    // synced and immediately before the rename.
    let write_opts = rightkit_fs::atomic::AtomicWriteOptions {
        replace: if existing {
            rightkit_fs::atomic::ReplaceMode::Rename
        } else {
            rightkit_fs::atomic::ReplaceMode::NoReplace
        },
        ..Default::default()
    };
    let mut writer = rightkit_fs::AtomicWriter::create_with(&real, write_opts).map_err(fs_err)?;
    if existing {
        if let Ok(meta) = fs::metadata(&real) {
            // Keep the permission bits (and the read-only attribute) of the file being replaced.
            let _ = writer.file_mut().set_permissions(meta.permissions());
        }
    }
    writer.write_all(&current).map_err(fs_err)?;
    #[cfg(test)]
    tests::run_before_publish_hook();
    let changed = AtomicBool::new(false);
    let expected = source_sha.clone();
    let recheck_path = real.clone();
    let limit = limits.max_input_bytes;
    let publish = writer.commit_checked(|_temp| {
        if let Some(h0) = &expected {
            let now = read_file(&recheck_path, limit).map(|b| sha256_hex(&b)).ok();
            if now.as_deref() != Some(h0.as_str()) {
                changed.store(true, Ordering::SeqCst);
                return Err(std::io::Error::other("source changed"));
            }
        } else if recheck_path.exists() {
            changed.store(true, Ordering::SeqCst);
            return Err(std::io::Error::other("target appeared"));
        }
        Ok(())
    });
    if let Err(e) = publish {
        if changed.load(Ordering::SeqCst) {
            return Err(if source_sha.is_some() {
                DocsError::SourceChanged
            } else {
                DocsError::TargetExists
            });
        }
        return Err(fs_err(e));
    }

    Ok(WriteReceipt {
        path: target.to_path_buf(),
        format,
        created: !existing,
        source_sha256: source_sha,
        sha256: sha256_hex(&current),
        bytes_written: current.len() as u64,
        ops_applied: ops.len(),
        replacements: totals.replacements,
        cells_set: totals.cells_set,
        rows_appended: totals.rows_appended,
        touched_parts: totals.touched.into_iter().collect(),
        losses: totals.losses,
        warnings: totals.warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    type Hook = Box<dyn Fn()>;
    thread_local! {
        static BEFORE_PUBLISH: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    pub(super) fn run_before_publish_hook() {
        BEFORE_PUBLISH.with(|h| {
            if let Some(f) = h.borrow().as_ref() {
                f();
            }
        });
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rightkit-docs-unit-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Another process rewrites the file after this call has read it and built its result, and
    /// before the rename: the last recheck sees it, nothing is published, and the other writer's
    /// bytes survive.
    #[test]
    fn a_file_changed_between_read_and_publish_is_never_overwritten() {
        let dir = scratch("race");
        let path = dir.join("a.docx");
        let make = |t: &str| Op::CreateDocx { blocks: vec![Block::Paragraph { text: t.into() }] };
        write(&path, &[make("original")], &WriteOptions::default()).unwrap();

        let theirs = docx::create(&[Block::Paragraph { text: "someone else".into() }]).unwrap();
        let (p, t) = (path.clone(), theirs.clone());
        BEFORE_PUBLISH.with(|h| *h.borrow_mut() = Some(Box::new(move || fs::write(&p, &t).unwrap())));
        let e = write(
            &path,
            &[Op::ReplaceText { find: "original".into(), replace: "mine".into() }],
            &WriteOptions::default(),
        )
        .unwrap_err();
        BEFORE_PUBLISH.with(|h| *h.borrow_mut() = None);

        assert_eq!(e.code(), "source_changed");
        assert_eq!(fs::read(&path).unwrap(), theirs, "the concurrent writer's file is intact");
        let names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "no temp file left behind: {names:?}");
    }

    /// The target file is created by someone else between the existence check and the rename of a
    /// create: the create does not clobber it.
    #[test]
    fn a_create_never_clobbers_a_file_that_appears_first() {
        let dir = scratch("create-race");
        let path = dir.join("new.docx");
        let (p, t) = (path.clone(), b"theirs".to_vec());
        BEFORE_PUBLISH.with(|h| *h.borrow_mut() = Some(Box::new(move || fs::write(&p, &t).unwrap())));
        let e = write(
            &path,
            &[Op::CreateDocx { blocks: vec![Block::Paragraph { text: "x".into() }] }],
            &WriteOptions::default(),
        )
        .unwrap_err();
        BEFORE_PUBLISH.with(|h| *h.borrow_mut() = None);
        assert_eq!(e.code(), "target_exists");
        assert_eq!(fs::read(&path).unwrap(), b"theirs");
    }
}
