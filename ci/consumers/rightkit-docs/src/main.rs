// rightkit-docs 0.1.6 consumed by exact registry pin, outside the workspace.
//
// Proves the smallest pure DOCX family: a typed CreateDocx written into a temp directory; a read
// by content (format from the bytes, text, and the SHA-256 of the exact bytes read); an edit made
// with expect_sha256 that publishes one replacement; the refusal of a stale expect_sha256
// (source_changed); the refusal to create over an existing file without overwrite (target_exists);
// and the refusal of bytes that are not an office file (unsupported_format).
// Does not prove: XLSX or PDF reads and writes, Markdown rendering, OOXML loss receipts, macro or
// encryption refusals, symlink targets, or the concurrent-writer race.
// State: files only under a fresh temp directory, removed at the end. No network, no real user data.
use std::fs;

use rightkit_docs::{read, write, Block, DocsError, Format, Op, ReadOptions, Source, WriteOptions};

/// The stable code of a call that must be refused.
fn refusal<T>(result: Result<T, DocsError>) -> &'static str {
    match result {
        Ok(_) => panic!("expected the call to be refused"),
        Err(e) => e.code(),
    }
}

const PNG: &[u8] = b"\x89\x50\x4e\x47\x0d\x0a\x1a\x0a\x00\x00\x00\x0d\x49\x48\x44\x52\
    \x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00\x90\x77\x53\
    \xde\x00\x00\x00\x0c\x49\x44\x41\x54\x78\x9c\x63\x90\x8e\xaa\x00\
    \x00\x01\x81\x00\xee\xf1\x21\x4d\xbe\x00\x00\x00\x00\x49\x45\x4e\
    \x44\xae\x42\x60\x82";
const BAD_ADLER_PNG: &[u8] = b"\x89\x50\x4e\x47\x0d\x0a\x1a\x0a\x00\x00\x00\x0d\x49\x48\x44\x52\
    \x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00\x90\x77\x53\
    \xde\x00\x00\x00\x0c\x49\x44\x41\x54\x78\x9c\x63\x90\x8e\xaa\x00\
    \x00\x01\x81\x00\xef\x86\x26\x7d\x28\x00\x00\x00\x00\x49\x45\x4e\
    \x44\xae\x42\x60\x82";

fn passive_png() -> Result<(), Box<dyn std::error::Error>> {
    use rightkit_docs::passive_image::{canonicalize, PassiveImageError, PassiveImageLimits};

    let limits = PassiveImageLimits {
        max_input_bytes: 1024,
        max_width: 16,
        max_height: 16,
        max_pixels: 256,
        max_working_bytes: 2 * 1024 * 1024,
        max_output_bytes: 1024,
    };
    let image = canonicalize(PNG, "image/png", limits)?;
    assert_eq!((image.width, image.height), (1, 1));
    assert!(image.bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(matches!(
        canonicalize(BAD_ADLER_PNG, "image/png", limits),
        Err(PassiveImageError::Malformed)
    ));
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("rightkit-docs-consumer-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;
    let path = dir.join("report.docx");

    // Create: a new DOCX from typed blocks.
    let created = write(
        &path,
        &[Op::CreateDocx {
            blocks: vec![
                Block::Heading {
                    level: 1,
                    text: "Q3".into(),
                },
                Block::Paragraph {
                    text: "Revenue held.".into(),
                },
            ],
        }],
        &WriteOptions::default(),
    )?;
    assert!(created.created, "the file is new");
    assert_eq!(created.format, Format::Docx);
    assert_eq!(created.ops_applied, 1);

    // Read: the format comes from the bytes, and the hash is of the exact bytes read.
    let doc = read(Source::Path(&path), &ReadOptions::default())?;
    assert_eq!(doc.format, Format::Docx);
    assert_eq!(
        doc.sha256, created.sha256,
        "read and create hash the same bytes"
    );
    assert!(doc.text.contains("Q3"), "heading text is read back");
    assert!(
        doc.text.contains("Revenue held."),
        "paragraph text is read back"
    );

    // Refused: an edit against a hash that is not the file's current hash publishes nothing.
    let stale = refusal(write(
        &path,
        &[Op::ReplaceText {
            find: "Q3".into(),
            replace: "Q4".into(),
        }],
        &WriteOptions {
            expect_sha256: Some("0".repeat(64)),
            ..Default::default()
        },
    ));
    assert_eq!(stale, "source_changed");

    // Edit: against the hash that was read, one replacement is published.
    let edited = write(
        &path,
        &[Op::ReplaceText {
            find: "Q3".into(),
            replace: "Q4".into(),
        }],
        &WriteOptions {
            expect_sha256: Some(doc.sha256.clone()),
            ..Default::default()
        },
    )?;
    assert!(!edited.created, "an edit does not create");
    assert_eq!(edited.replacements, 1);
    assert_eq!(edited.source_sha256.as_deref(), Some(doc.sha256.as_str()));
    assert_ne!(edited.sha256, doc.sha256, "the edit changed the bytes");

    let after = read(Source::Path(&path), &ReadOptions::default())?;
    assert!(after.text.contains("Q4"), "the replacement is read back");

    // Refused: a create over an existing file needs overwrite.
    let exists = refusal(write(
        &path,
        &[Op::CreateDocx {
            blocks: vec![Block::Paragraph {
                text: "again".into(),
            }],
        }],
        &WriteOptions::default(),
    ));
    assert_eq!(exists, "target_exists");

    // Refused: bytes that are not a DOCX, XLSX or PDF are never guessed at.
    let not_office = refusal(read(
        Source::Bytes(b"plain text, not an office file"),
        &ReadOptions::default(),
    ));
    assert_eq!(not_office, "unsupported_format");

    fs::remove_dir_all(&dir)?;
    passive_png()?;
    println!("OK rightkit-docs 0.1.6 (registry consumer)");
    Ok(())
}
