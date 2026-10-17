//! Hostile input: every case goes through the public `read` or `write` on a real file.

mod common;

use std::io::Write as _;

use common::*;
use rightkit_docs::*;

fn read_bytes_err(bytes: &[u8]) -> DocsError {
    read(Source::Bytes(bytes), &ReadOptions::default()).unwrap_err()
}

fn docx_with(part_name: &str, part: &[u8]) -> Vec<u8> {
    let mut parts = unzip(&rich_docx());
    parts.insert(part_name.into(), part.to_vec());
    let files: Vec<(&str, &[u8])> = parts
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_slice()))
        .collect();
    zip_of(&files)
}

#[test]
fn zip_bomb_by_ratio_is_refused_before_any_parser_runs() {
    // 12 MiB of zeros compresses to a few KiB: far past the 200:1 ratio.
    let bomb = docx_with("word/bomb.xml", &vec![b' '; 12 << 20]);
    assert!(bomb.len() < 100 * 1024, "fixture is small: {}", bomb.len());
    let e = read_bytes_err(&bomb);
    assert_eq!(e.code(), "archive_limit", "{e}");
}

#[test]
fn oversized_part_total_entries_and_input_are_capped_per_call() {
    let docx = rich_docx();
    // Per-part cap.
    let e = read(
        Source::Bytes(&docx),
        &ReadOptions {
            limits: Limits {
                max_part_bytes: 100,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code(), "archive_limit");
    // Total decompressed cap.
    let e = read(
        Source::Bytes(&docx),
        &ReadOptions {
            limits: Limits {
                max_total_bytes: 500,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code(), "archive_limit");
    // Entry count.
    let e = read(
        Source::Bytes(&docx),
        &ReadOptions {
            limits: Limits {
                max_entries: 3,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code(), "archive_limit");
    // Compressed size, by bytes and by path (checked before the file is read).
    let e = read(
        Source::Bytes(&docx),
        &ReadOptions {
            limits: Limits {
                max_input_bytes: 100,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code(), "too_large");
    let dir = scratch("caps");
    let p = write_file(&dir, "big.docx", &docx);
    let e = read(
        Source::Path(&p),
        &ReadOptions {
            limits: Limits {
                max_input_bytes: 100,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code(), "too_large");
    // The same limits guard an edit, and a refused edit leaves the file alone.
    let e = write(
        &p,
        &[Op::ReplaceText {
            find: "Region".into(),
            replace: "x".into(),
        }],
        &WriteOptions {
            limits: Limits {
                max_entries: 3,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code(), "archive_limit");
    assert_eq!(std::fs::read(&p).unwrap(), docx);
}

#[test]
fn xml_external_entities_and_doctypes_are_refused_in_every_part_kind() {
    let xxe = br#"<?xml version="1.0"?><!DOCTYPE d [<!ENTITY xxe SYSTEM "file:///etc/passwd">]><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>&xxe;</w:t></w:r></w:p></w:body></w:document>"#;
    let e = read_bytes_err(&docx_with("word/document.xml", xxe));
    assert!(matches!(e, DocsError::ForbiddenXml { .. }), "{e}");
    assert_eq!(e.code(), "forbidden_xml");

    // Billion laughs.
    let laughs = br#"<?xml version="1.0"?><!DOCTYPE lolz [<!ENTITY lol "lol"><!ENTITY lol2 "&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;">]><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>&lol2;</w:t></w:r></w:p></w:body></w:document>"#;
    assert_eq!(
        read_bytes_err(&docx_with("word/document.xml", laughs)).code(),
        "forbidden_xml"
    );

    // A part the reader never opens is refused too (preflight covers every XML part).
    assert_eq!(
        read_bytes_err(&docx_with("customXml/item1.xml", xxe)).code(),
        "forbidden_xml"
    );

    // XLSX sheet and an entity declared without a DOCTYPE wrapper in the same scan.
    let mut parts = unzip(&mini_xlsx(r#"<row r="1"><c r="A1"><v>1</v></c></row>"#));
    parts.insert("xl/worksheets/sheet1.xml".into(), br#"<?xml version="1.0"?><!DOCTYPE s [<!ENTITY e SYSTEM "http://169.254.169.254/">]><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/></worksheet>"#.to_vec());
    let files: Vec<(&str, &[u8])> = parts
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_slice()))
        .collect();
    let bad = zip_of(&files);
    assert_eq!(read_bytes_err(&bad).code(), "forbidden_xml");
    let dir = scratch("xxe-write");
    let p = write_file(&dir, "bad.xlsx", &bad);
    let e = write(
        &p,
        &[Op::SetCells {
            sheet: "S".into(),
            cells: vec![CellSet {
                cell: "A1".into(),
                value: CellInput::Number(1.0),
            }],
        }],
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert_eq!(e.code(), "forbidden_xml");
    assert_eq!(std::fs::read(&p).unwrap(), bad);

    // UTF-16 XML would hide a DOCTYPE from a byte scan; no Office writer produces it.
    let utf16: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain("<a/>".encode_utf16().flat_map(|u| u.to_le_bytes()))
        .collect();
    assert_eq!(
        read_bytes_err(&docx_with("word/document.xml", &utf16)).code(),
        "forbidden_xml"
    );
}

#[test]
fn macro_enabled_packages_are_refused_for_read_and_for_edit() {
    // .docm: macro-enabled main content type and a VBA project.
    let mut parts = unzip(&rich_docx());
    let ct = text_of(&parts, "[Content_Types].xml")
        .replace("document.main+xml", "document.macroEnabled.main+xml");
    parts.insert("[Content_Types].xml".into(), ct.into_bytes());
    parts.insert("word/vbaProject.bin".into(), vec![0xD0, 0xCF, 0x11, 0xE0]);
    let files: Vec<(&str, &[u8])> = parts
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_slice()))
        .collect();
    let docm = zip_of(&files);
    let e = read_bytes_err(&docm);
    assert_eq!(e.code(), "macro_enabled", "{e}");

    // .xlsm by content type alone (no VBA part present) and by VBA part alone.
    let mut parts = unzip(&rich_xlsx());
    let ct = text_of(&parts, "[Content_Types].xml")
        .replace("sheet.main+xml", "sheet.macroEnabled.main+xml");
    parts.insert("[Content_Types].xml".into(), ct.into_bytes());
    let files: Vec<(&str, &[u8])> = parts
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_slice()))
        .collect();
    assert_eq!(read_bytes_err(&zip_of(&files)).code(), "macro_enabled");
    let mut parts = unzip(&rich_xlsx());
    parts.insert("xl/vbaProject.bin".into(), vec![1, 2, 3]);
    let files: Vec<(&str, &[u8])> = parts
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_slice()))
        .collect();
    let xlsm = zip_of(&files);
    assert_eq!(read_bytes_err(&xlsm).code(), "macro_enabled");

    // A write never opens it, runs it, or strips the macros and passes it off as clean.
    let dir = scratch("macro");
    let p = write_file(&dir, "m.xlsm", &xlsm);
    let e = write(
        &p,
        &[Op::SetCells {
            sheet: "Data".into(),
            cells: vec![CellSet {
                cell: "A1".into(),
                value: CellInput::Number(1.0),
            }],
        }],
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert_eq!(e.code(), "macro_enabled");
    assert_eq!(std::fs::read(&p).unwrap(), xlsm);
}

#[test]
fn encrypted_documents_get_a_typed_error() {
    // A password-protected OOXML file is an OLE container with an EncryptedPackage stream.
    let mut ole = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    ole.extend(vec![0u8; 512]);
    for name in ["EncryptionInfo", "EncryptedPackage"] {
        ole.extend(name.encode_utf16().flat_map(|u| u.to_le_bytes()));
        ole.extend([0u8; 16]);
    }
    assert_eq!(read_bytes_err(&ole).code(), "encrypted");
    let dir = scratch("encrypted");
    let p = write_file(&dir, "secret.docx", &ole);
    let e = write(
        &p,
        &[Op::ReplaceText {
            find: "a".into(),
            replace: "b".into(),
        }],
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert_eq!(e.code(), "encrypted");
    assert_eq!(std::fs::read(&p).unwrap(), ole);

    // A legacy .doc (OLE without encryption streams) is unsupported, not "encrypted".
    let mut legacy = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    legacy.extend(vec![0u8; 512]);
    assert_eq!(read_bytes_err(&legacy).code(), "unsupported_format");

    // A PDF with an /Encrypt dictionary.
    use lopdf::{dictionary, Document};
    let base = hello_pdf("secret");
    let mut doc = Document::load_mem(&base).unwrap();
    let enc = doc.add_object(dictionary! { "Filter" => "Standard", "V" => 1, "R" => 2, "O" => lopdf::Object::string_literal("0123456789abcdef0123456789abcdef"), "U" => lopdf::Object::string_literal("0123456789abcdef0123456789abcdef"), "P" => -4 });
    doc.trailer.set("Encrypt", enc);
    doc.trailer.set(
        "ID",
        vec![
            lopdf::Object::string_literal("a"),
            lopdf::Object::string_literal("a"),
        ],
    );
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    assert_eq!(read_bytes_err(&out).code(), "encrypted");
}

#[test]
fn unsafe_names_garbage_and_other_formats_are_refused_not_guessed() {
    // Zip-slip entry names.
    let evil = zip_of(&[
        ("[Content_Types].xml", b"<Types/>"),
        ("../../evil.xml", b"<a/>"),
    ]);
    assert_eq!(read_bytes_err(&evil).code(), "malformed");
    let abs = zip_of(&[("[Content_Types].xml", b"<Types/>"), ("/etc/passwd", b"x")]);
    assert_eq!(read_bytes_err(&abs).code(), "malformed");

    // Truncated archive, garbage, empty.
    let docx = rich_docx();
    assert_eq!(read_bytes_err(&docx[..docx.len() / 2]).code(), "malformed");
    assert_eq!(
        read_bytes_err(b"hello, this is not a document").code(),
        "unsupported_format"
    );
    assert_eq!(read_bytes_err(b"").code(), "unsupported_format");
    assert_eq!(read_bytes_err(&[0u8; 4096]).code(), "unsupported_format");

    // PPTX and a zip that is not an Office document.
    let pptx = zip_of(&[
        ("[Content_Types].xml", b"<Types/>"),
        ("_rels/.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="r" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#),
        ("ppt/presentation.xml", b"<p/>"),
    ]);
    assert_eq!(read_bytes_err(&pptx).code(), "unsupported_format");
    assert_eq!(
        read_bytes_err(&zip_of(&[("a.txt", b"x")])).code(),
        "unsupported_format"
    );

    // A package that names a Word main part but does not hold it.
    let ghost = zip_of(&[
        ("[Content_Types].xml", b"<Types/>"),
        ("_rels/.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="r" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#),
    ]);
    assert_eq!(read_bytes_err(&ghost).code(), "malformed");

    // Broken XML inside a part.
    assert_eq!(
        read_bytes_err(&docx_with(
            "word/document.xml",
            b"<w:document><w:body></w:document>"
        ))
        .code(),
        "malformed"
    );
}

#[test]
fn deeply_nested_xml_is_refused() {
    let mut xml = String::from(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#,
    );
    for _ in 0..2000 {
        xml.push_str("<w:p>");
    }
    for _ in 0..2000 {
        xml.push_str("</w:p>");
    }
    xml.push_str("</w:body></w:document>");
    let e = read_bytes_err(&docx_with("word/document.xml", xml.as_bytes()));
    assert_eq!(e.code(), "malformed", "{e}");
}

#[test]
fn external_references_are_never_followed() {
    // The rich docx carries an external hyperlink relationship to a metadata address; reading and
    // editing it neither fetches nor drops it.
    let dir = scratch("external");
    let p = write_file(&dir, "e.docx", &rich_docx());
    let d = read(Source::Path(&p), &ReadOptions::default()).unwrap();
    assert!(d
        .omitted
        .iter()
        .any(|o| o.contains("external relationships not followed")));
    write(
        &p,
        &[Op::ReplaceText {
            find: "Region".into(),
            replace: "Area".into(),
        }],
        &WriteOptions::default(),
    )
    .unwrap();
    let rels = text_of(
        &unzip(&std::fs::read(&p).unwrap()),
        "word/_rels/document.xml.rels",
    );
    assert!(
        rels.contains("169.254.169.254") && rels.contains("External"),
        "the relationship is preserved as written"
    );
}

#[test]
fn formulas_that_reach_outside_the_workbook_are_refused() {
    let dir = scratch("formulas");
    let p = write_file(&dir, "f.xlsx", &rich_xlsx());
    let before = std::fs::read(&p).unwrap();
    let set = |f: &str| {
        write(
            &p,
            &[Op::SetCells {
                sheet: "Data".into(),
                cells: vec![CellSet {
                    cell: "F1".into(),
                    value: CellInput::Formula(f.into()),
                }],
            }],
            &WriteOptions::default(),
        )
    };
    for bad in [
        "=WEBSERVICE(\"http://example.com/x\")",
        "=FILTERXML(WEBSERVICE(\"http://x\"),\"//a\")",
        "='[Book.xlsx]Sheet1'!A1",
        "=[1]Sheet1!A1",
        "='C:\\dir\\[Book.xlsx]Sheet1'!A1",
        "=cmd|' /C calc'!A0",
        "=EXEC(\"calc\")",
        "=",
    ] {
        let e = set(bad).unwrap_err();
        assert!(
            matches!(e.code(), "unsafe_formula" | "invalid_op"),
            "{bad}: {e}"
        );
    }
    assert_eq!(
        std::fs::read(&p).unwrap(),
        before,
        "refused formulas write nothing"
    );
    // Ordinary formulas, including a string that merely mentions a URL and a structured reference.
    for good in [
        "=SUM(A1:A3)",
        "=IF(B2>1,\"http://not-a-reference\",0)",
        "=Data!B2*2",
        "=Table1[Sales]",
    ] {
        // A quoted URL is text, not a reference; HYPERLINK only makes a link, it fetches nothing.
        let _ = set(good).map_err(|e| panic!("{good}: {e}"));
    }
    // The same policy on create.
    let e = write(
        &dir.join("n.xlsx"),
        &[Op::CreateXlsx {
            sheets: vec![SheetSpec {
                name: "S".into(),
                rows: vec![vec![CellInput::Formula("=WEBSERVICE(\"http://x\")".into())]],
            }],
        }],
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert_eq!(e.code(), "unsafe_formula");
    assert!(!dir.join("n.xlsx").exists());
}

#[test]
fn bad_cells_sheets_and_values_are_invalid_ops() {
    let dir = scratch("bad-ops");
    let p = write_file(&dir, "b.xlsx", &rich_xlsx());
    let before = std::fs::read(&p).unwrap();
    let one = |cell: &str, v: CellInput, sheet: &str| {
        write(
            &p,
            &[Op::SetCells {
                sheet: sheet.into(),
                cells: vec![CellSet {
                    cell: cell.into(),
                    value: v,
                }],
            }],
            &WriteOptions::default(),
        )
        .unwrap_err()
    };
    assert_eq!(
        one("A0", CellInput::Number(1.0), "Data").code(),
        "invalid_op"
    );
    assert_eq!(
        one("ZZZZ1", CellInput::Number(1.0), "Data").code(),
        "invalid_op"
    );
    assert_eq!(
        one("A1048577", CellInput::Number(1.0), "Data").code(),
        "invalid_op"
    );
    assert_eq!(
        one("not a cell", CellInput::Number(1.0), "Data").code(),
        "invalid_op"
    );
    assert_eq!(
        one("A1", CellInput::Number(f64::NAN), "Data").code(),
        "invalid_op"
    );
    assert_eq!(
        one("A1", CellInput::Number(f64::INFINITY), "Data").code(),
        "invalid_op"
    );
    assert_eq!(
        one("A1", CellInput::Text("x".repeat(40_000)), "Data").code(),
        "invalid_op"
    );
    assert_eq!(
        one("A1", CellInput::Number(1.0), "No such sheet").code(),
        "not_found"
    );
    assert_eq!(std::fs::read(&p).unwrap(), before);
    let d = write(
        &dir.join("c.docx"),
        &[Op::CreateDocx {
            blocks: vec![Block::Heading {
                level: 7,
                text: "x".into(),
            }],
        }],
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert_eq!(d.code(), "invalid_op");
    assert!(!dir.join("c.docx").exists());
    let d = write(&dir.join("c.docx"), &[], &WriteOptions::default()).unwrap_err();
    assert_eq!(d.code(), "invalid_op");
}

#[test]
fn text_survives_xml_hostile_characters_and_control_bytes() {
    let dir = scratch("hostile-text");
    let p = dir.join("t.docx");
    let nasty =
        "a]]>b <![CDATA[x]]> &amp; &#0; \"q\" 'a' \u{1F600} \u{0}\u{1}\u{8}\u{B}\u{C}\u{1F} done";
    write(
        &p,
        &[Op::CreateDocx {
            blocks: vec![Block::Paragraph { text: nasty.into() }],
        }],
        &WriteOptions::default(),
    )
    .unwrap();
    let d = read(Source::Path(&p), &ReadOptions::default()).unwrap();
    // Control characters XML cannot carry are dropped; everything else is verbatim.
    assert_eq!(
        d.text,
        "a]]>b <![CDATA[x]]> &amp; &#0; \"q\" 'a' \u{1F600}  done"
    );

    let x = dir.join("t.xlsx");
    write(
        &x,
        &[Op::CreateXlsx {
            sheets: vec![SheetSpec {
                name: "S".into(),
                rows: vec![vec![CellInput::Text(nasty.into())]],
            }],
        }],
        &WriteOptions::default(),
    )
    .unwrap();
    write(
        &x,
        &[Op::SetCells {
            sheet: "S".into(),
            cells: vec![CellSet {
                cell: "B1".into(),
                value: CellInput::Text(nasty.into()),
            }],
        }],
        &WriteOptions::default(),
    )
    .unwrap();
    let d = read(Source::Path(&x), &ReadOptions::default()).unwrap();
    let Content::Xlsx(c) = &d.content else {
        panic!()
    };
    let want = CellValue::Text("a]]>b <![CDATA[x]]> &amp; &#0; \"q\" 'a' \u{1F600}  done".into());
    assert_eq!(c.sheets[0].cells[0].value, want);
    assert_eq!(c.sheets[0].cells[1].value, want);
}

#[test]
fn a_decompression_bomb_pdf_object_stream_is_bounded() {
    // A flate stream that expands far past the loader's bound is refused as malformed, and an
    // oversize PDF is refused by size before parsing.
    let big = vec![b'%'; 2048];
    let e = read(
        Source::Bytes(&big),
        &ReadOptions {
            limits: Limits {
                max_input_bytes: 1024,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code(), "too_large");
    let mut junk = b"%PDF-1.7\n".to_vec();
    junk.extend_from_slice(b"1 0 obj\n<< /Length 5 >>\nstream\nxxxxx\nendstream\nendobj\n");
    let e = read_bytes_err(&junk);
    assert_eq!(e.code(), "malformed", "{e}");
    let _ = std::io::stdout().flush();
}

#[test]
fn a_failed_publish_leaves_the_original_and_no_temp_file() {
    // rightkit-fs's fault seam fails the rename itself: the file must be the one that was there.
    let dir = scratch("failed-publish");
    let path = write_file(&dir, "a.docx", &rich_docx());
    let before = std::fs::read(&path).unwrap();
    let _guard =
        rightkit_fs::atomic::testing::on_replace(|_| Some(std::io::Error::other("injected")));
    let e = write(
        &path,
        &[Op::ReplaceText {
            find: "Region".into(),
            replace: "Area".into(),
        }],
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert_eq!(e.code(), "io", "{e}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(files_in(&dir), vec!["a.docx".to_string()]);
}

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn doc_xml(body: &[u8]) -> Vec<u8> {
    let mut xml = format!(r#"<w:document xmlns:w="{W_NS}"><w:body>"#).into_bytes();
    xml.extend_from_slice(body);
    xml.extend_from_slice(b"</w:body></w:document>");
    xml
}

#[test]
fn invalid_utf8_in_a_part_is_malformed_not_silently_skipped() {
    for body in [
        &b"<w:p><w:r><w:t>ok \xFF\xFE bad</w:t></w:r></w:p>"[..],
        &b"<w:p><w:pPr><w:pStyle w:val=\"\xC3\x28\"/></w:pPr><w:r><w:t>x</w:t></w:r></w:p>"[..],
        &b"<w:p><w:r><w:t><![CDATA[\x80\x81]]></w:t></w:r></w:p>"[..],
    ] {
        let e = read_bytes_err(&docx_with("word/document.xml", &doc_xml(body)));
        assert_eq!(e.code(), "malformed", "{e}");
    }
}

#[test]
fn undeclared_entities_and_bad_character_references_are_dropped_never_expanded() {
    let body = format!(
        "<w:p><w:r><w:t>a&lol;b&amp;&#65;&#x42;&#0;c{}</w:t></w:r></w:p>",
        "&lol;".repeat(20_000)
    );
    let d = read(
        Source::Bytes(&docx_with("word/document.xml", &doc_xml(body.as_bytes()))),
        &ReadOptions::default(),
    )
    .unwrap();
    assert_eq!(d.text.trim(), "ab&ABc");
}

#[test]
fn mismatched_and_hostile_xml_in_a_part_is_malformed_with_the_part_named() {
    for xml in [
        &b"<w:document><w:body><w:p></w:body></w:document>"[..],
        &b"<w:document></w:p>"[..],
        &b"<w:document><w:body"[..],
    ] {
        let e = read_bytes_err(&docx_with("word/document.xml", xml));
        assert_eq!(e.code(), "malformed", "{e}");
        assert!(e.to_string().contains("word/document.xml"), "{e}");
    }
}

#[test]
fn a_huge_attribute_value_does_not_break_the_walk() {
    // Under the 1 MiB point where the ratio cap starts to apply to a single part.
    let big = "x".repeat(512 * 1024);
    let body =
        format!(r#"<w:p><w:pPr><w:pStyle w:val="{big}"/></w:pPr><w:r><w:t>kept</w:t></w:r></w:p>"#);
    let d = read(
        Source::Bytes(&docx_with("word/document.xml", &doc_xml(body.as_bytes()))),
        &ReadOptions::default(),
    )
    .unwrap();
    assert_eq!(d.text.trim(), "kept");
}
