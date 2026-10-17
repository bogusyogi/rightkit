#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

pub fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rightkit-docs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A zip from (name, bytes) pairs, in order.
pub fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default();
    for (n, c) in files {
        w.start_file(*n, o).unwrap();
        w.write_all(c).unwrap();
    }
    w.finish().unwrap().into_inner()
}

/// Every entry of a zip: name -> decompressed bytes.
pub fn unzip(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut out = BTreeMap::new();
    for i in 0..z.len() {
        let mut f = z.by_index(i).unwrap();
        let mut v = Vec::new();
        f.read_to_end(&mut v).unwrap();
        out.insert(f.name().expect("a readable entry name").to_string(), v);
    }
    out
}

pub fn text_of(parts: &BTreeMap<String, Vec<u8>>, name: &str) -> String {
    String::from_utf8_lossy(&parts[name]).into_owned()
}

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// A Word-shaped package with everything an editor must not lose: styles, a header and a footer,
/// comments, footnotes, a tracked insertion and deletion, a custom XML part, a table, a text
/// split across runs, and an external relationship.
pub fn rich_docx() -> Vec<u8> {
    let ct = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/><Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/><Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/></Types>"#;
    let root = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/><Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/><Relationship Id="rId6" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="http://169.254.169.254/latest/meta-data" TargetMode="External"/></Relationships>"#;
    let styles = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles xmlns:w="{W}"><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/></w:style></w:styles>"#
    );
    let header = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:hdr xmlns:w="{W}"><w:p><w:r><w:t>HEADER-ALPHA</w:t></w:r></w:p></w:hdr>"#
    );
    let footer = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:ftr xmlns:w="{W}"><w:p><w:r><w:t>FOOTER-OMEGA</w:t></w:r></w:p></w:ftr>"#
    );
    let comments = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:comments xmlns:w="{W}"><w:comment w:id="0" w:author="Rev"><w:p><w:r><w:t>COMMENT-BODY</w:t></w:r></w:p></w:comment></w:comments>"#
    );
    let footnotes = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:footnotes xmlns:w="{W}"><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:r><w:t>FOOTNOTE-BODY</w:t></w:r></w:p></w:footnote></w:footnotes>"#
    );
    let doc = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="{W}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Quarterly Report</w:t></w:r></w:p><w:p><w:commentRangeStart w:id="0"/><w:r><w:t xml:space="preserve">Revenue grew </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>strongly</w:t></w:r><w:commentRangeEnd w:id="0"/><w:r><w:commentReference w:id="0"/></w:r><w:ins w:id="10" w:author="Rev"><w:r><w:t>INSERTED-TEXT</w:t></w:r></w:ins><w:del w:id="11" w:author="Rev"><w:r><w:delText>DELETED-TEXT</w:delText></w:r></w:del><w:r><w:footnoteReference w:id="1"/></w:r></w:p><w:p><w:r><w:t>Tom &amp; Jerry &lt;3</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>Region</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Sales</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>EMEA</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>42</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr><w:headerReference w:type="default" r:id="rId2"/><w:footerReference w:type="default" r:id="rId3"/></w:sectPr></w:body></w:document>"#
    );
    zip_of(&[
        ("[Content_Types].xml", ct.as_bytes()),
        ("_rels/.rels", root.as_bytes()),
        ("word/_rels/document.xml.rels", rels.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
        ("word/header1.xml", header.as_bytes()),
        ("word/footer1.xml", footer.as_bytes()),
        ("word/comments.xml", comments.as_bytes()),
        ("word/footnotes.xml", footnotes.as_bytes()),
        (
            "customXml/item1.xml",
            br#"<?xml version="1.0"?><root xmlns="urn:rk">CUSTOM-PART</root>"#,
        ),
        ("word/document.xml", doc.as_bytes()),
    ])
}

/// A workbook from rust_xlsxwriter with a chart, a note, validation, a merged range and a name.
pub fn rich_xlsx() -> Vec<u8> {
    use rust_xlsxwriter::*;
    let mut wb = Workbook::new();
    let ws = wb.add_worksheet().set_name("Data").unwrap();
    ws.write_string(0, 0, "Region").unwrap();
    ws.write_string(0, 1, "Sales").unwrap();
    ws.write_string(1, 0, "EMEA").unwrap();
    ws.write_number(1, 1, 42.5).unwrap();
    ws.write_string(2, 0, "APAC").unwrap();
    ws.write_number(2, 1, 17.0).unwrap();
    ws.write_boolean(3, 0, true).unwrap();
    ws.write_formula(4, 1, Formula::new("=SUM(B2:B3)").set_result("59.5"))
        .unwrap();
    ws.merge_range(6, 0, 6, 1, "Merged", &Format::new())
        .unwrap();
    ws.insert_note(1, 1, &Note::new("NOTE-BODY")).unwrap();
    let dv = DataValidation::new().allow_whole_number(DataValidationRule::Between(1, 9));
    ws.add_data_validation(0, 3, 5, 3, &dv).unwrap();
    let mut chart = Chart::new(ChartType::Column);
    chart
        .add_series()
        .set_categories(("Data", 1, 0, 2, 0))
        .set_values(("Data", 1, 1, 2, 1));
    ws.insert_chart(8, 0, &chart).unwrap();
    let ws2 = wb.add_worksheet().set_name("Second").unwrap();
    ws2.write_string(0, 0, "two").unwrap();
    wb.define_name("Total", "=Data!$B$5").unwrap();
    wb.save_to_buffer().unwrap()
}

pub const SHEET_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";

/// The smallest workbook: one sheet named `S` whose `<sheetData>` is `rows` verbatim.
pub fn mini_xlsx(sheet_data: &str) -> Vec<u8> {
    let ct = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#;
    let root = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
    let wb = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="{SHEET_NS}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="S" sheetId="1" r:id="rId1"/></sheets></workbook>"#
    );
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
    let sheet = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="{SHEET_NS}"><dimension ref="A1:B3"/><sheetData>{sheet_data}</sheetData></worksheet>"#
    );
    zip_of(&[
        ("[Content_Types].xml", ct.as_bytes()),
        ("_rels/.rels", root.as_bytes()),
        ("xl/workbook.xml", wb.as_bytes()),
        ("xl/_rels/workbook.xml.rels", rels.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
    ])
}

/// A one-page PDF with a base-14 Helvetica line, editable in place.
pub fn hello_pdf(text: &str) -> Vec<u8> {
    use lopdf::{dictionary, Document, Object, Stream};
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let content = doc.add_object(Stream::new(
        dictionary! {},
        format!("BT /F1 18 Tf 72 700 Td ({text}) Tj ET").into_bytes(),
    ));
    let page = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        "Contents" => content,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1,
        }),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

pub fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

pub fn files_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}
