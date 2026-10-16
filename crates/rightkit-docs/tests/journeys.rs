//! One journey per format on real files: create, read back, edit, re-read, then hand the result
//! to the operating system's own consumer where the host has one.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::*;
use rightkit_docs::*;

fn read_path(p: &Path) -> DocumentRead {
    read(Source::Path(p), &ReadOptions::default()).unwrap()
}

fn docx_blocks(d: &DocumentRead) -> &Vec<DocxBlock> {
    match &d.content {
        Content::Docx(c) => &c.blocks,
        other => panic!("not docx: {other:?}"),
    }
}

fn sheet<'a>(d: &'a DocumentRead, name: &str) -> &'a SheetRead {
    match &d.content {
        Content::Xlsx(c) => c.sheets.iter().find(|s| s.name == name).expect("sheet"),
        other => panic!("not xlsx: {other:?}"),
    }
}

fn cell<'a>(s: &'a SheetRead, a1: &str) -> &'a CellRead {
    s.cells
        .iter()
        .find(|c| c.cell == a1)
        .unwrap_or_else(|| panic!("no cell {a1}"))
}

/// Run an OS consumer if this host has it; returns its stdout.
#[cfg(target_os = "macos")]
fn textutil_txt(path: &Path) -> Option<String> {
    let out = std::process::Command::new("textutil")
        .args(["-convert", "txt", "-stdout"])
        .arg(path)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

// ---------------------------------------------------------------------------------------------
// DOCX
// ---------------------------------------------------------------------------------------------

#[test]
fn docx_journey_create_read_edit_reread_and_open_in_the_os() {
    let dir = scratch("docx-journey");
    let path = dir.join("report.docx");
    let created = write(
        &path,
        &[Op::CreateDocx {
            blocks: vec![
                Block::Heading {
                    level: 1,
                    text: "Quarterly Report".into(),
                },
                Block::Paragraph {
                    text: "Revenue grew strongly & margins held <steady>.".into(),
                },
                Block::Bullet {
                    text: "first point".into(),
                },
                Block::Bullet {
                    text: "second point".into(),
                },
                Block::Numbered {
                    text: "step one".into(),
                },
                Block::Numbered {
                    text: "step two".into(),
                },
                Block::Table {
                    rows: vec![
                        vec!["Region".into(), "Sales".into()],
                        vec!["EMEA".into(), "42".into()],
                    ],
                    header: true,
                },
            ],
        }],
        &WriteOptions::default(),
    )
    .unwrap();
    assert!(created.created);
    assert_eq!(created.format, Format::Docx);
    assert!(created.losses.is_empty());

    // Read back: styles by display name, list flags, table, renderings.
    let d = read_path(&path);
    assert_eq!(d.format, Format::Docx);
    assert_eq!(d.sha256, created.sha256);
    let blocks = docx_blocks(&d);
    assert!(
        matches!(&blocks[0], DocxBlock::Paragraph { style: Some(s), text, .. } if s == "heading 1" && text == "Quarterly Report")
    );
    assert!(
        matches!(&blocks[1], DocxBlock::Paragraph { text, .. } if text == "Revenue grew strongly & margins held <steady>.")
    );
    assert!(
        matches!(&blocks[2], DocxBlock::Paragraph { list: true, text, style: Some(s) } if text == "first point" && s == "List Paragraph")
    );
    assert!(
        matches!(blocks.last().unwrap(), DocxBlock::Table { rows } if rows[1] == vec!["EMEA".to_string(), "42".to_string()])
    );
    assert!(d.text.contains("Region\tSales\nEMEA\t42"), "{}", d.text);
    let md = d.markdown.clone().unwrap();
    assert!(md.starts_with("# Quarterly Report"), "{md}");
    assert!(md.contains("- first point\n- second point"), "{md}");
    assert!(
        md.contains("| Region | Sales |\n| --- | --- |\n| EMEA | 42 |"),
        "{md}"
    );
    assert!(!d.truncated);

    // Edit against exactly what was read.
    let r = write(
        &path,
        &[
            Op::ReplaceText {
                find: "Quarterly".into(),
                replace: "Annual".into(),
            },
            Op::AppendBlocks {
                blocks: vec![
                    Block::Heading {
                        level: 2,
                        text: "Appendix".into(),
                    },
                    Block::Paragraph {
                        text: "Added by an edit.".into(),
                    },
                ],
            },
        ],
        &WriteOptions {
            expect_sha256: Some(d.sha256.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!r.created);
    assert_eq!(r.source_sha256.as_deref(), Some(d.sha256.as_str()));
    assert_eq!(r.replacements, 1);
    assert_eq!(r.touched_parts, vec!["word/document.xml".to_string()]);
    assert!(r.losses.is_empty(), "{:?}", r.losses);

    let d2 = read_path(&path);
    assert_ne!(d2.sha256, d.sha256);
    let blocks = docx_blocks(&d2);
    assert!(matches!(&blocks[0], DocxBlock::Paragraph { text, .. } if text == "Annual Report"));
    let n = blocks.len();
    assert!(
        matches!(&blocks[n - 2], DocxBlock::Paragraph { style: Some(s), text, .. } if s == "heading 2" && text == "Appendix")
    );
    assert!(
        matches!(&blocks[n - 1], DocxBlock::Paragraph { text, .. } if text == "Added by an edit.")
    );
    // Untouched content is untouched.
    assert!(
        matches!(blocks.iter().find(|b| matches!(b, DocxBlock::Table { .. })), Some(DocxBlock::Table { rows }) if rows.len() == 2)
    );

    // The same file, through the operating system's own importer.
    #[cfg(target_os = "macos")]
    {
        let txt = textutil_txt(&path).expect("textutil converts the edited docx");
        for want in [
            "Annual Report",
            "Revenue grew strongly & margins held <steady>.",
            "first point",
            "step two",
            "EMEA",
            "Appendix",
            "Added by an edit.",
        ] {
            assert!(txt.contains(want), "textutil output lacks {want:?}:\n{txt}");
        }
        assert!(!txt.contains("Quarterly"), "{txt}");
    }
}

#[test]
fn docx_edit_keeps_every_part_it_does_not_touch_and_replaces_across_runs() {
    let dir = scratch("docx-rich");
    let path = write_file(&dir, "rich.docx", &rich_docx());
    let before = unzip(&std::fs::read(&path).unwrap());

    let d = read_path(&path);
    // Everything the read does not represent is named, not dropped silently.
    let omitted = d.omitted.join(" | ");
    for want in [
        "comments: 1",
        "tracked changes",
        "footnotes: 1",
        "external relationships not followed: 1",
    ] {
        assert!(omitted.contains(want), "{omitted}");
    }
    let Content::Docx(c) = &d.content else {
        panic!()
    };
    assert_eq!(c.headers, vec!["HEADER-ALPHA".to_string()]);
    assert_eq!(c.footers, vec!["FOOTER-OMEGA".to_string()]);
    // The tracked insertion is visible text; the tracked deletion is not.
    assert!(d.text.contains("Revenue grew strongly"), "{}", d.text);
    assert!(
        d.text.contains("INSERTED-TEXT") && !d.text.contains("DELETED-TEXT"),
        "{}",
        d.text
    );
    assert!(d.text.contains("Tom & Jerry <3"));

    // "grew strongly" spans two runs (the second is bold); a header and special characters too.
    let r = write(
        &path,
        &[
            Op::ReplaceText {
                find: "grew strongly".into(),
                replace: "fell R&D <sharply>".into(),
            },
            Op::ReplaceText {
                find: "HEADER-ALPHA".into(),
                replace: "HEADER-BETA".into(),
            },
            Op::ReplaceText {
                find: "Jerry".into(),
                replace: "Spike".into(),
            },
        ],
        &WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(r.replacements, 3);
    assert!(r.losses.is_empty(), "{:?}", r.losses);
    assert!(
        r.warnings.iter().any(|w| w.contains("tracked changes")),
        "{:?}",
        r.warnings
    );
    let mut touched = r.touched_parts.clone();
    touched.sort();
    assert_eq!(
        touched,
        vec![
            "word/document.xml".to_string(),
            "word/header1.xml".to_string()
        ]
    );

    let after = unzip(&std::fs::read(&path).unwrap());
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>(),
        "no part gained or lost"
    );
    for part in before.keys() {
        if part != "word/document.xml" && part != "word/header1.xml" {
            assert_eq!(before[part], after[part], "{part} must be byte-identical");
        }
    }
    let doc = text_of(&after, "word/document.xml");
    for kept in [
        "w:ins ",
        "w:del ",
        "commentRangeStart",
        "commentReference",
        "footnoteReference",
        "INSERTED-TEXT",
        "DELETED-TEXT",
        "w:b/",
        "Heading1",
    ] {
        assert!(doc.contains(kept), "document.xml lost {kept}");
    }
    assert!(
        doc.contains("Revenue fell R&amp;D &lt;sharply&gt;"),
        "{doc}"
    );
    assert!(text_of(&after, "word/header1.xml").contains("HEADER-BETA"));

    let d2 = read_path(&path);
    assert!(
        d2.text.contains("Revenue fell R&D <sharply>"),
        "{}",
        d2.text
    );
    assert!(d2.text.contains("Tom & Spike <3"), "{}", d2.text);
    let Content::Docx(c2) = &d2.content else {
        panic!()
    };
    assert_eq!(c2.headers, vec!["HEADER-BETA".to_string()]);

    // A string that is not there is an error, never a silent no-op, and writes nothing.
    let before_bytes = std::fs::read(&path).unwrap();
    let e = write(
        &path,
        &[Op::ReplaceText {
            find: "absent text".into(),
            replace: "x".into(),
        }],
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert_eq!(e.code(), "not_found");
    assert_eq!(std::fs::read(&path).unwrap(), before_bytes);
    assert_eq!(
        files_in(&dir),
        vec!["rich.docx".to_string()],
        "no temp file left behind"
    );
}

#[test]
fn docx_append_into_a_document_without_numbering_says_what_it_did() {
    let dir = scratch("docx-append-warn");
    let path = write_file(&dir, "rich.docx", &rich_docx());
    let r = write(
        &path,
        &[Op::AppendBlocks {
            blocks: vec![
                Block::Heading {
                    level: 3,
                    text: "Notes".into(),
                },
                Block::Bullet {
                    text: "item".into(),
                },
                Block::PageBreak,
            ],
        }],
        &WriteOptions::default(),
    )
    .unwrap();
    assert!(
        r.warnings.iter().any(|w| w.contains("Heading3")),
        "{:?}",
        r.warnings
    );
    assert!(
        r.warnings.iter().any(|w| w.contains("numbering")),
        "{:?}",
        r.warnings
    );
    let d = read_path(&path);
    assert!(d.text.contains("Notes"));
    assert!(d.text.contains("\u{2022} item"));
    // The final section properties stayed last in the body.
    let parts = unzip(&std::fs::read(&path).unwrap());
    let doc = text_of(&parts, "word/document.xml");
    let notes_at = doc.find("Notes").unwrap();
    let sect_at = doc.rfind("<w:sectPr").unwrap();
    assert!(notes_at < sect_at);
}

// ---------------------------------------------------------------------------------------------
// XLSX
// ---------------------------------------------------------------------------------------------

#[test]
fn xlsx_journey_create_read_edit_reread() {
    let dir = scratch("xlsx-journey");
    let path = dir.join("book.xlsx");
    let injected = ["=1+1", "+SUM(A1)", "-2+3", "@cmd", "=cmd|' /C calc'!A0"];
    let mut rows = vec![
        vec![
            CellInput::Text("Item".into()),
            CellInput::Text("Qty".into()),
            CellInput::Text("Price".into()),
        ],
        vec![
            CellInput::Text("Widget".into()),
            CellInput::Number(3.0),
            CellInput::Number(2.5),
        ],
        vec![
            CellInput::Text("Gadget".into()),
            CellInput::Number(4.0),
            CellInput::Number(1.25),
        ],
        vec![
            CellInput::Text("In stock".into()),
            CellInput::Bool(true),
            CellInput::Blank,
        ],
        vec![
            CellInput::Text("Total".into()),
            CellInput::Formula("=SUM(B2:B3)".into()),
        ],
    ];
    for s in injected {
        rows.push(vec![CellInput::Text(s.into())]);
    }
    let created = write(
        &path,
        &[Op::CreateXlsx {
            sheets: vec![
                SheetSpec {
                    name: "Data".into(),
                    rows,
                },
                SheetSpec {
                    name: "Notes".into(),
                    rows: vec![vec![CellInput::Text("second sheet".into())]],
                },
            ],
        }],
        &WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(created.format, Format::Xlsx);

    let d = read_path(&path);
    assert_eq!(d.format, Format::Xlsx);
    let s = sheet(&d, "Data");
    assert_eq!(s.used_range.as_deref(), Some("A1:C10"));
    assert_eq!(cell(s, "A2").value, CellValue::Text("Widget".into()));
    assert_eq!(cell(s, "B2").value, CellValue::Number(3.0));
    assert_eq!(cell(s, "C3").value, CellValue::Number(1.25));
    assert_eq!(cell(s, "B4").value, CellValue::Bool(true));
    // Written as a formula only because the op said Formula; no result is stored yet.
    let total = cell(s, "B5");
    assert_eq!(total.formula.as_deref(), Some("SUM(B2:B3)"));
    assert_eq!(total.value, CellValue::Empty);
    // Text that starts like a formula is a string, every time.
    for (i, want) in injected.iter().enumerate() {
        let c = cell(s, &format!("A{}", 6 + i));
        assert_eq!(c.value, CellValue::Text((*want).into()));
        assert_eq!(c.formula, None);
    }
    assert!(d.markdown.as_ref().unwrap().contains("## Data"));

    // Edit: overwrite a cell, set new ones (a formula-looking string, a formula), append rows.
    let r = write(
        &path,
        &[
            Op::SetCells {
                sheet: "Data".into(),
                cells: vec![
                    CellSet {
                        cell: "A2".into(),
                        value: CellInput::Text("Widget XL".into()),
                    },
                    CellSet {
                        cell: "B2".into(),
                        value: CellInput::Number(10.0),
                    },
                    CellSet {
                        cell: "E1".into(),
                        value: CellInput::Text("=HYPERLINK(\"http://x\",\"y\")".into()),
                    },
                    CellSet {
                        cell: "E2".into(),
                        value: CellInput::Formula("=B2*C2".into()),
                    },
                    CellSet {
                        cell: "C4".into(),
                        value: CellInput::Number(9.0),
                    },
                ],
            },
            Op::AppendRows {
                sheet: "Data".into(),
                rows: vec![
                    vec![CellInput::Text("Appended".into()), CellInput::Number(7.0)],
                    vec![CellInput::Text("Appended 2".into()), CellInput::Bool(false)],
                ],
            },
        ],
        &WriteOptions {
            expect_sha256: Some(d.sha256.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.cells_set, 5);
    assert_eq!(r.rows_appended, 2);
    assert!(
        r.warnings.iter().any(|w| w.contains("recalculate on open")),
        "{:?}",
        r.warnings
    );

    let d2 = read_path(&path);
    let s = sheet(&d2, "Data");
    assert_eq!(cell(s, "A2").value, CellValue::Text("Widget XL".into()));
    assert_eq!(cell(s, "B2").value, CellValue::Number(10.0));
    assert_eq!(
        cell(s, "C2").value,
        CellValue::Number(2.5),
        "untouched neighbour"
    );
    assert_eq!(
        cell(s, "E1").value,
        CellValue::Text("=HYPERLINK(\"http://x\",\"y\")".into())
    );
    assert_eq!(cell(s, "E2").formula.as_deref(), Some("B2*C2"));
    assert_eq!(cell(s, "C4").value, CellValue::Number(9.0));
    assert_eq!(cell(s, "A11").value, CellValue::Text("Appended".into()));
    assert_eq!(cell(s, "B11").value, CellValue::Number(7.0));
    assert_eq!(cell(s, "B12").value, CellValue::Bool(false));
    assert_eq!(s.used_range.as_deref(), Some("A1:E12"));
    assert_eq!(
        cell(sheet(&d2, "Notes"), "A1").value,
        CellValue::Text("second sheet".into())
    );

    // The same workbook through the operating system's consumer: Quick Look renders it.
    #[cfg(target_os = "macos")]
    {
        let out = dir.join("ql");
        std::fs::create_dir_all(&out).unwrap();
        let st = std::process::Command::new("qlmanage")
            .args(["-t", "-s", "600", "-o"])
            .arg(&out)
            .arg(&path)
            .output()
            .expect("qlmanage runs on macOS");
        assert!(
            st.status.success(),
            "{}",
            String::from_utf8_lossy(&st.stderr)
        );
        let png = out.join("book.xlsx.png");
        assert!(
            png.exists(),
            "Quick Look produced no thumbnail: {}",
            String::from_utf8_lossy(&st.stdout)
        );
        assert!(
            std::fs::metadata(&png).unwrap().len() > 5000,
            "Quick Look thumbnail is blank"
        );
    }
}

#[test]
fn xlsx_edit_keeps_charts_notes_validation_merges_and_names() {
    let dir = scratch("xlsx-rich");
    let path = write_file(&dir, "rich.xlsx", &rich_xlsx());
    let before = unzip(&std::fs::read(&path).unwrap());
    let d = read_path(&path);
    let omitted = d.omitted.join(" | ");
    assert!(omitted.contains("charts: 1"), "{omitted}");
    assert!(omitted.contains("comments (notes): 1"), "{omitted}");
    let s = sheet(&d, "Data");
    // Formula with its cached result, as stored.
    assert_eq!(cell(s, "B5").value, CellValue::Number(59.5));
    assert_eq!(cell(s, "B5").formula.as_deref(), Some("SUM(B2:B3)"));

    let r = write(
        &path,
        &[Op::SetCells {
            sheet: "Data".into(),
            cells: vec![CellSet {
                cell: "A2".into(),
                value: CellInput::Text("EDITED".into()),
            }],
        }],
        &WriteOptions::default(),
    )
    .unwrap();
    assert!(r.losses.is_empty(), "{:?}", r.losses);
    let mut touched = r.touched_parts.clone();
    touched.sort();
    assert_eq!(
        touched,
        vec![
            "xl/workbook.xml".to_string(),
            "xl/worksheets/sheet1.xml".to_string()
        ]
    );

    let after = unzip(&std::fs::read(&path).unwrap());
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>()
    );
    for part in before.keys() {
        if part != "xl/workbook.xml" && part != "xl/worksheets/sheet1.xml" {
            assert_eq!(before[part], after[part], "{part} must be byte-identical");
        }
    }
    let sheet_xml = text_of(&after, "xl/worksheets/sheet1.xml");
    for kept in [
        "<mergeCell ",
        "<dataValidation ",
        "<f>SUM(B2:B3)</f>",
        "<v>59.5</v>",
        "<drawing ",
        "<legacyDrawing ",
    ] {
        assert!(sheet_xml.contains(kept), "sheet1.xml lost {kept}");
    }
    assert!(text_of(&after, "xl/workbook.xml").contains("fullCalcOnLoad=\"1\""));
    assert!(text_of(&after, "xl/workbook.xml").contains("<definedName name=\"Total\">"));
    let d2 = read_path(&path);
    assert_eq!(
        cell(sheet(&d2, "Data"), "A2").value,
        CellValue::Text("EDITED".into())
    );
    assert_eq!(
        cell(sheet(&d2, "Data"), "A3").value,
        CellValue::Text("APAC".into())
    );
}

#[test]
fn xlsx_edit_drops_only_the_derived_calc_chain_and_says_so() {
    let dir = scratch("xlsx-calcchain");
    let base = rich_xlsx();
    let mut parts = unzip(&base);
    parts.insert("xl/calcChain.xml".into(), br#"<?xml version="1.0"?><calcChain xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><c r="B5" i="1"/></calcChain>"#.to_vec());
    let ct = text_of(&parts, "[Content_Types].xml").replace("</Types>", "<Override PartName=\"/xl/calcChain.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.calcChain+xml\"/></Types>");
    parts.insert("[Content_Types].xml".into(), ct.into_bytes());
    let rels = text_of(&parts, "xl/_rels/workbook.xml.rels").replace("</Relationships>", "<Relationship Id=\"rId99\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/calcChain\" Target=\"calcChain.xml\"/></Relationships>");
    parts.insert("xl/_rels/workbook.xml.rels".into(), rels.into_bytes());
    let files: Vec<(&str, &[u8])> = parts
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_slice()))
        .collect();
    let path = write_file(&dir, "chain.xlsx", &zip_of(&files));
    assert!(read_path(&path).format == Format::Xlsx);

    let r = write(
        &path,
        &[Op::SetCells {
            sheet: "Data".into(),
            cells: vec![CellSet {
                cell: "B5".into(),
                value: CellInput::Number(1.0),
            }],
        }],
        &WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(r.losses.len(), 1, "{:?}", r.losses);
    assert_eq!(r.losses[0].kind, "derived_part_dropped");
    assert_eq!(r.losses[0].part, "xl/calcChain.xml");
    let after = unzip(&std::fs::read(&path).unwrap());
    assert!(!after.contains_key("xl/calcChain.xml"));
    assert!(!text_of(&after, "[Content_Types].xml").contains("calcChain"));
    assert!(!text_of(&after, "xl/_rels/workbook.xml.rels").contains("calcChain"));
    // Still a workbook the reader accepts, with the overwritten formula cell now a value.
    let d = read_path(&path);
    assert_eq!(cell(sheet(&d, "Data"), "B5").value, CellValue::Number(1.0));
    assert_eq!(cell(sheet(&d, "Data"), "B5").formula, None);
}

#[test]
fn xlsx_sparse_rows_styles_and_shared_formulas_are_handled_as_stored() {
    let dir = scratch("xlsx-mini");
    let sheet_data = r#"<row r="1" spans="1:2"><c r="A1" t="inlineStr"><is><t>h1</t></is></c><c r="B1" s="3"><v>5</v></c></row><row r="3"><c r="A3"><f t="shared" ref="A3:A4" si="0">B1*2</f><v>10</v></c></row><row r="4"><c r="A4"><f t="shared" si="0"/><v>20</v></c></row><row r="9" s="2" customFormat="1"/>"#;
    let path = write_file(&dir, "mini.xlsx", &mini_xlsx(sheet_data));
    let d = read_path(&path);
    assert_eq!(cell(sheet(&d, "S"), "B1").value, CellValue::Number(5.0));

    // Overwriting the master of a shared formula would break A4: refused, nothing written.
    let before = std::fs::read(&path).unwrap();
    let e = write(
        &path,
        &[Op::SetCells {
            sheet: "S".into(),
            cells: vec![CellSet {
                cell: "A3".into(),
                value: CellInput::Number(1.0),
            }],
        }],
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert_eq!(e.code(), "invalid_op");
    assert_eq!(std::fs::read(&path).unwrap(), before);

    // A style on an overwritten cell is kept; a new cell in a row after a gap lands in order; an
    // appended row goes after the last row that holds a value, not after a styled empty row.
    write(
        &path,
        &[
            Op::SetCells {
                sheet: "S".into(),
                cells: vec![
                    CellSet {
                        cell: "B1".into(),
                        value: CellInput::Number(6.0),
                    },
                    CellSet {
                        cell: "C2".into(),
                        value: CellInput::Text("new".into()),
                    },
                ],
            },
            Op::AppendRows {
                sheet: "S".into(),
                rows: vec![vec![CellInput::Text("tail".into())]],
            },
        ],
        &WriteOptions::default(),
    )
    .unwrap();
    let parts = unzip(&std::fs::read(&path).unwrap());
    let xml = text_of(&parts, "xl/worksheets/sheet1.xml");
    assert!(xml.contains("<c r=\"B1\" s=\"3\"><v>6</v></c>"), "{xml}");
    assert!(
        !xml.contains("spans="),
        "rewritten rows drop the stale spans hint: {xml}"
    );
    assert!(
        xml.contains("<f t=\"shared\" ref=\"A3:A4\" si=\"0\">B1*2</f>"),
        "untouched shared master kept verbatim: {xml}"
    );
    assert!(
        xml.contains("<row r=\"9\" s=\"2\" customFormat=\"1\"/>"),
        "untouched styled row kept verbatim: {xml}"
    );
    let row2 = xml.find("<row r=\"2\">").unwrap();
    assert!(
        row2 > xml.find("<row r=\"1\"").unwrap() && row2 < xml.find("<row r=\"3\"").unwrap(),
        "rows stay in order: {xml}"
    );
    let d = read_path(&path);
    let s = sheet(&d, "S");
    assert_eq!(
        cell(s, "A5").value,
        CellValue::Text("tail".into()),
        "after the last populated row (4), not after styled-empty row 9"
    );
    assert_eq!(cell(s, "C2").value, CellValue::Text("new".into()));
    assert!(xml.contains("<dimension ref=\"A1:C5\"/>"), "{xml}");
}

#[test]
fn xlsx_selection_and_caps() {
    let dir = scratch("xlsx-select");
    let path = write_file(&dir, "rich.xlsx", &rich_xlsx());
    let opts = ReadOptions {
        sheets: Some(vec!["Second".into()]),
        ..Default::default()
    };
    let d = read(Source::Path(&path), &opts).unwrap();
    let Content::Xlsx(c) = &d.content else {
        panic!()
    };
    assert_eq!(c.sheets.len(), 1);
    assert_eq!(c.sheets[0].name, "Second");

    let opts = ReadOptions {
        sheets: Some(vec!["Data".into()]),
        range: Some("A2:B3".into()),
        ..Default::default()
    };
    let d = read(Source::Path(&path), &opts).unwrap();
    let s = sheet(&d, "Data");
    let names: Vec<_> = s.cells.iter().map(|c| c.cell.as_str()).collect();
    assert_eq!(names, vec!["A2", "B2", "A3", "B3"]);
    assert_eq!(
        s.used_range.as_deref(),
        Some("A1:B7"),
        "used range is the sheet's, not the selection's"
    );

    let opts = ReadOptions {
        max_cells: 3,
        ..Default::default()
    };
    let d = read(Source::Path(&path), &opts).unwrap();
    assert!(d.truncated);
    let total: usize = match &d.content {
        Content::Xlsx(c) => c.sheets.iter().map(|s| s.cells.len()).sum(),
        _ => 0,
    };
    assert_eq!(total, 3);

    let opts = ReadOptions {
        max_chars: 10,
        ..Default::default()
    };
    let d = read(Source::Path(&path), &opts).unwrap();
    assert!(d.truncated);
    assert!(d.text.chars().count() <= 10);

    let e = read(
        Source::Path(&path),
        &ReadOptions {
            sheets: Some(vec!["Nope".into()]),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code(), "not_found");
    let e = read(
        Source::Path(&path),
        &ReadOptions {
            range: Some("A1:".into()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code(), "invalid_op");
}

#[test]
fn xlsx_dates_read_as_dates() {
    use rust_xlsxwriter::*;
    let dir = scratch("xlsx-dates");
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet();
    let fmt = Format::new().set_num_format("yyyy-mm-dd");
    ws.write_number_with_format(0, 0, 46_000.0, &fmt).unwrap(); // 2025-12-09
    ws.write_number_with_format(
        1,
        0,
        46_000.75,
        &Format::new().set_num_format("yyyy-mm-dd hh:mm:ss"),
    )
    .unwrap();
    let path = write_file(&dir, "d.xlsx", &wb.save_to_buffer().unwrap());
    let d = read_path(&path);
    let s = sheet(&d, "Sheet1");
    assert_eq!(
        cell(s, "A1").value,
        CellValue::DateTime {
            serial: 46_000.0,
            iso: Some("2025-12-09".into())
        }
    );
    assert_eq!(
        cell(s, "A2").value,
        CellValue::DateTime {
            serial: 46_000.75,
            iso: Some("2025-12-09T18:00:00".into())
        }
    );
}

// ---------------------------------------------------------------------------------------------
// PDF
// ---------------------------------------------------------------------------------------------

#[test]
fn pdf_journey_read_edit_reread_and_open_in_the_os() {
    let dir = scratch("pdf-journey");
    let path = write_file(&dir, "memo.pdf", &hello_pdf("Hello World"));
    let d = read_path(&path);
    assert_eq!(d.format, Format::Pdf);
    assert!(d.text.contains("Hello World"), "{}", d.text);
    let Content::Pdf(c) = &d.content else {
        panic!()
    };
    assert_eq!(c.page_count, 1);
    assert!(d.markdown.as_ref().unwrap().contains("## Page 1"));

    let r = write(
        &path,
        &[
            Op::ReplaceText {
                find: "World".into(),
                replace: "Rustaceans".into(),
            },
            Op::AddPdfText {
                page: 1,
                x: 72.0,
                y: 400.0,
                text: "Reviewed by RightKit".into(),
                size_pt: Some(14.0),
            },
        ],
        &WriteOptions {
            expect_sha256: Some(d.sha256.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.replacements, 1);
    assert_eq!(r.format, Format::Pdf);

    let d2 = read_path(&path);
    assert!(d2.text.contains("Hello Rustaceans"), "{}", d2.text);
    assert!(d2.text.contains("Reviewed by RightKit"), "{}", d2.text);
    assert!(!d2.text.contains("World"));

    // Select pages: a one-page file selecting page 1 again twice makes three pages.
    write(
        &path,
        &[Op::SelectPdfPages {
            pages: vec![1, 1, 1],
        }],
        &WriteOptions::default(),
    )
    .unwrap();
    let d3 = read_path(&path);
    let Content::Pdf(c3) = &d3.content else {
        panic!()
    };
    assert_eq!(c3.page_count, 3);
    let one = read(
        Source::Path(&path),
        &ReadOptions {
            pages: Some(vec![2]),
            ..Default::default()
        },
    )
    .unwrap();
    let Content::Pdf(c) = &one.content else {
        panic!()
    };
    assert_eq!(c.pages.len(), 1);
    assert_eq!(c.pages[0].page, 2);
    let e = read(
        Source::Path(&path),
        &ReadOptions {
            pages: Some(vec![9]),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code(), "not_found");

    // CoreGraphics (the system PDF engine) rasterises the edited file.
    #[cfg(target_os = "macos")]
    {
        let png = dir.join("out.png");
        let st = std::process::Command::new("sips")
            .args(["-s", "format", "png"])
            .arg(&path)
            .arg("--out")
            .arg(&png)
            .output()
            .expect("sips runs");
        assert!(
            st.status.success(),
            "{}",
            String::from_utf8_lossy(&st.stderr)
        );
        assert!(std::fs::metadata(&png).unwrap().len() > 1000);
    }
}

#[test]
fn pdf_missing_text_and_wrong_ops_are_typed_refusals() {
    let dir = scratch("pdf-refuse");
    let path = write_file(&dir, "memo.pdf", &hello_pdf("Hello World"));
    let before = std::fs::read(&path).unwrap();
    let e = write(
        &path,
        &[Op::ReplaceText {
            find: "absent".into(),
            replace: "x".into(),
        }],
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert_eq!(e.code(), "not_found");
    let e = write(
        &path,
        &[Op::SetCells {
            sheet: "S".into(),
            cells: vec![],
        }],
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert_eq!(e.code(), "wrong_format");
    let e = write(
        &path,
        &[Op::AddPdfText {
            page: 4,
            x: 0.0,
            y: 0.0,
            text: "x".into(),
            size_pt: None,
        }],
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert_eq!(e.code(), "not_found");
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

// ---------------------------------------------------------------------------------------------
// Detection, publishing and the wire
// ---------------------------------------------------------------------------------------------

#[test]
fn format_comes_from_the_content_not_the_file_name() {
    let dir = scratch("detect");
    let docx = write_file(&dir, "actually-a-docx.pdf", &rich_docx());
    let xlsx = write_file(&dir, "actually-an-xlsx.txt", &rich_xlsx());
    let pdf = write_file(&dir, "actually-a-pdf.docx", &hello_pdf("Name lies"));
    assert_eq!(read_path(&docx).format, Format::Docx);
    assert_eq!(read_path(&xlsx).format, Format::Xlsx);
    assert_eq!(read_path(&pdf).format, Format::Pdf);
    // An edit follows the content too: a replace on the "docx.pdf" is a DOCX edit.
    let r = write(
        &docx,
        &[Op::ReplaceText {
            find: "Region".into(),
            replace: "Area".into(),
        }],
        &WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(r.format, Format::Docx);
    // Bytes work as a source.
    let d = read(Source::Bytes(&rich_xlsx()), &ReadOptions::default()).unwrap();
    assert_eq!(d.format, Format::Xlsx);
}

#[test]
fn publishing_is_atomic_refuses_stale_and_clobbering_writes_and_keeps_permissions() {
    let dir = scratch("publish");
    let path = dir.join("a.docx");
    let make = || Op::CreateDocx {
        blocks: vec![Block::Paragraph { text: "one".into() }],
    };
    write(&path, &[make()], &WriteOptions::default()).unwrap();

    // A second create does not clobber.
    let e = write(&path, &[make()], &WriteOptions::default()).unwrap_err();
    assert_eq!(e.code(), "target_exists");
    // ...unless asked, and then the stale-hash guard still applies.
    let h = read_path(&path).sha256;
    write(
        &path,
        &[make()],
        &WriteOptions {
            overwrite: true,
            expect_sha256: Some(h.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let e = write(
        &path,
        &[make()],
        &WriteOptions {
            overwrite: true,
            expect_sha256: Some("0".repeat(64)),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code(), "source_changed");

    // Edit with the wrong hash: refused, file untouched, no temp file.
    let before = std::fs::read(&path).unwrap();
    let e = write(
        &path,
        &[Op::ReplaceText {
            find: "one".into(),
            replace: "two".into(),
        }],
        &WriteOptions {
            expect_sha256: Some("f".repeat(64)),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code(), "source_changed");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(files_in(&dir), vec!["a.docx".to_string()]);

    // An edit of a file that is not there.
    let e = write(
        &dir.join("missing.docx"),
        &[Op::ReplaceText {
            find: "a".into(),
            replace: "b".into(),
        }],
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert_eq!(e.code(), "target_missing");
    // A create op in the middle.
    let e = write(
        &path,
        &[
            Op::ReplaceText {
                find: "one".into(),
                replace: "two".into(),
            },
            make(),
        ],
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert_eq!(e.code(), "invalid_op");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "a failing op list writes nothing"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        write(
            &path,
            &[Op::ReplaceText {
                find: "one".into(),
                replace: "two".into(),
            }],
            &WriteOptions::default(),
        )
        .unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        // A symlink target is replaced where it points; the link survives.
        let link = dir.join("link.docx");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        write(
            &link,
            &[Op::ReplaceText {
                find: "two".into(),
                replace: "three".into(),
            }],
            &WriteOptions::default(),
        )
        .unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(read_path(&path).text.contains("three"));
    }
}

#[test]
fn ops_and_errors_round_trip_as_wire_json() {
    let ops: Vec<Op> = serde_json::from_str(
        r#"[
          {"kind":"createXlsx","sheets":[{"name":"S","rows":[[{"type":"text","value":"=1+1"},{"type":"number","value":2},{"type":"formula","value":"=A1"},{"type":"blank"}]]}]},
          {"kind":"setCells","sheet":"S","cells":[{"cell":"A1","value":{"type":"bool","value":true}}]},
          {"kind":"replaceText","find":"a","replace":"b"},
          {"kind":"addPdfText","page":1,"x":1.0,"y":2.0,"text":"t","sizePt":null}
        ]"#,
    )
    .unwrap();
    assert!(
        matches!(&ops[0], Op::CreateXlsx { sheets } if sheets[0].rows[0][0] == CellInput::Text("=1+1".into()))
    );
    let json = serde_json::to_string(&ops).unwrap();
    assert!(json.contains("\"kind\":\"createXlsx\""));

    let e = DocsError::Encrypted;
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(v["code"], "encrypted");
    assert!(v["message"].as_str().unwrap().contains("encrypted"));

    let dir = scratch("wire");
    let path = write_file(&dir, "d.docx", &rich_docx());
    let d = read_path(&path);
    let v = serde_json::to_value(&d).unwrap();
    assert_eq!(v["format"], "docx");
    assert_eq!(v["content"]["kind"], "docx");
    assert!(v["content"]["blocks"][0]["style"].is_string());
    let receipt = write(
        &path,
        &[Op::ReplaceText {
            find: "Region".into(),
            replace: "Area".into(),
        }],
        &WriteOptions::default(),
    )
    .unwrap();
    let v = serde_json::to_value(&receipt).unwrap();
    assert!(v["touchedParts"].is_array() && v["sourceSha256"].is_string());
    let _: BTreeMap<String, serde_json::Value> = serde_json::from_value(v).unwrap();
}
