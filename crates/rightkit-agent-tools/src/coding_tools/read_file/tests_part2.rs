#[tokio::test]
async fn read_file_extracts_basic_pdf_text() {
    let _guard = unconfined_rich_worker();
    let dir = tempdir().unwrap();
    let file = dir.path().join("hello.pdf");
    let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >> endobj
4 0 obj << /Length 44 >> stream
BT /F1 12 Tf 10 10 Td (Hello PDF) Tj ET
endstream endobj
trailer << /Root 1 0 R >>
%%EOF";
    std::fs::write(&file, pdf).unwrap();

    let result = ReadFileTool
        .execute(json!({"path": file.to_str().unwrap()}), &ctx(dir.path()))
        .await;

    assert!(!result.is_error);
    assert!(result.content.contains("PDF file:"));
    assert!(result.content.contains("pages: 1"));
    assert!(result.content.contains("Hello PDF"));
}

#[tokio::test]
async fn read_file_rejects_oversized_pdf_before_parsing() {
    let _guard = unconfined_rich_worker();
    use std::io::Write;

    let dir = tempdir().unwrap();
    let file = dir.path().join("huge.pdf");
    let mut handle = std::fs::File::create(&file).unwrap();
    handle.write_all(b"%PDF-1.7\n").unwrap();
    handle.set_len((MAX_PDF_BYTES + 1) as u64).unwrap();

    let result = ReadFileTool
        .execute(json!({"path": file.to_str().unwrap()}), &ctx(dir.path()))
        .await;

    assert!(result.is_error);
    assert!(result.content.contains("maximum supported size"));
}

#[tokio::test]
async fn read_file_bounds_pdf_page_window() {
    let _guard = unconfined_rich_worker();
    let dir = tempdir().unwrap();
    let file = dir.path().join("window.pdf");
    std::fs::write(&file, b"%PDF-1.4\n/Type /Page\n%%EOF").unwrap();

    let result = ReadFileTool
        .execute(
            json!({"path": file.to_str().unwrap(), "page_limit": MAX_PDF_PAGE_WINDOW + 1}),
            &ctx(dir.path()),
        )
        .await;

    assert!(result.is_error);
    assert!(result.content.contains("page_limit must be between"));
}

#[tokio::test]
async fn read_file_extracts_requested_pdf_page() {
    let _guard = unconfined_rich_worker();
    let dir = tempdir().unwrap();
    let file = dir.path().join("two-pages.pdf");
    let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >> endobj
4 0 obj << /Length 45 >> stream
BT /F1 12 Tf 10 10 Td (Page One Text) Tj ET
endstream endobj
5 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 6 0 R >> endobj
6 0 obj << /Length 45 >> stream
BT /F1 12 Tf 10 10 Td (Page Two Text) Tj ET
endstream endobj
trailer << /Root 1 0 R >>
%%EOF";
    std::fs::write(&file, pdf).unwrap();

    let result = ReadFileTool
        .execute(
            json!({"path": file.to_str().unwrap(), "page": 2}),
            &ctx(dir.path()),
        )
        .await;

    assert!(!result.is_error);
    assert!(result.content.contains("pages: 2"));
    assert!(result.content.contains("page 2:"));
    assert!(result.content.contains("Page Two Text"));
    assert!(!result.content.contains("Page One Text"));
}

#[tokio::test]
async fn read_file_extracts_flate_compressed_pdf_text() {
    let _guard = unconfined_rich_worker();
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    let dir = tempdir().unwrap();
    let file = dir.path().join("compressed.pdf");
    let stream = b"BT /F1 12 Tf 10 10 Td (Compressed PDF Text) Tj ET";
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(stream).unwrap();
    let compressed = encoder.finish().unwrap();

    let mut pdf = Vec::new();
    pdf.extend_from_slice(
        b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >> endobj
4 0 obj << /Length ",
    );
    pdf.extend_from_slice(compressed.len().to_string().as_bytes());
    pdf.extend_from_slice(b" /Filter /FlateDecode >> stream\n");
    pdf.extend_from_slice(&compressed);
    pdf.extend_from_slice(
        b"\nendstream endobj
trailer << /Root 1 0 R >>
%%EOF",
    );
    std::fs::write(&file, pdf).unwrap();

    let result = ReadFileTool
        .execute(json!({"path": file.to_str().unwrap()}), &ctx(dir.path()))
        .await;

    assert!(!result.is_error, "{}", result.content);
    assert!(result.content.contains("PDF file:"));
    assert!(result.content.contains("Compressed PDF Text"));

    let page_result = ReadFileTool
        .execute(
            json!({"path": file.to_str().unwrap(), "page": 1}),
            &ctx(dir.path()),
        )
        .await;
    assert!(!page_result.is_error, "{}", page_result.content);
    assert!(page_result.content.contains("page 1:"));
    assert!(page_result.content.contains("Compressed PDF Text"));
}

#[test]
fn pdf_flate_decoding_has_one_aggregate_document_budget() {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    let mut pdf = b"%PDF-1.4\n".to_vec();
    let inflated = vec![b'A'; 1024 * 1024];
    for object_id in 1..=12 {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&inflated).unwrap();
        let compressed = encoder.finish().unwrap();
        pdf.extend_from_slice(
            format!("{object_id} 0 obj << /Filter /FlateDecode >> stream\n").as_bytes(),
        );
        pdf.extend_from_slice(&compressed);
        pdf.extend_from_slice(b"\nendstream endobj\n");
    }
    pdf.extend_from_slice(b"%%EOF");

    let mut budget = PdfDecodeBudget::default();
    let text = String::from_utf8_lossy(&pdf);
    let _ = extract_pdf_text(&pdf, &text, &mut budget);

    assert!(budget.exhausted);
    assert_eq!(budget.remaining, 0);
}

#[tokio::test]
async fn read_file_extracts_hex_encoded_pdf_text() {
    let _guard = unconfined_rich_worker();
    let dir = tempdir().unwrap();
    let file = dir.path().join("hex.pdf");
    let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >> endobj
4 0 obj << /Length 60 >> stream
BT /F1 12 Tf 10 10 Td <486578205044462054657874> Tj ET
endstream endobj
trailer << /Root 1 0 R >>
%%EOF";
    std::fs::write(&file, pdf).unwrap();

    let result = ReadFileTool
        .execute(json!({"path": file.to_str().unwrap()}), &ctx(dir.path()))
        .await;

    assert!(!result.is_error, "{}", result.content);
    assert!(result.content.contains("Hex PDF Text"));
}

#[tokio::test]
async fn read_file_extracts_utf16_hex_pdf_text() {
    let _guard = unconfined_rich_worker();
    let dir = tempdir().unwrap();
    let file = dir.path().join("utf16-hex.pdf");
    let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >> endobj
4 0 obj << /Length 80 >> stream
BT /F1 12 Tf 10 10 Td <FEFF005500540046003100360020005000440046> Tj ET
endstream endobj
trailer << /Root 1 0 R >>
%%EOF";
    std::fs::write(&file, pdf).unwrap();

    let result = ReadFileTool
        .execute(json!({"path": file.to_str().unwrap()}), &ctx(dir.path()))
        .await;

    assert!(!result.is_error, "{}", result.content);
    assert!(result.content.contains("UTF16 PDF"));
}

#[tokio::test]
async fn read_file_extracts_pdf_text_with_tounicode_cmap() {
    let _guard = unconfined_rich_worker();
    let dir = tempdir().unwrap();
    let file = dir.path().join("cmap.pdf");
    let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >> endobj
4 0 obj << /Length 32 >> stream
BT /F1 12 Tf 10 10 Td <010203> Tj ET
endstream endobj
5 0 obj << /Type /Font /Subtype /Type0 /ToUnicode 6 0 R >> endobj
6 0 obj << /Length 160 >> stream
/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
1 beginbfchar
<01> <0048>
endbfchar
1 beginbfrange
<02> <03> <0069>
endbfrange
endcmap
CMapName currentdict /CMap defineresource pop
end
end
endstream endobj
trailer << /Root 1 0 R >>
%%EOF";
    std::fs::write(&file, pdf).unwrap();

    let result = ReadFileTool
        .execute(json!({"path": file.to_str().unwrap()}), &ctx(dir.path()))
        .await;

    assert!(!result.is_error, "{}", result.content);
    assert!(result.content.contains("Hij"), "{}", result.content);
}

#[tokio::test]
async fn read_file_reports_encrypted_pdf_without_empty_extraction() {
    let _guard = unconfined_rich_worker();
    let dir = tempdir().unwrap();
    let file = dir.path().join("encrypted.pdf");
    let pdf = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >> endobj
4 0 obj << /Length 32 >> stream
BT /F1 12 Tf 10 10 Td (Secret) Tj ET
endstream endobj
trailer << /Root 1 0 R /Encrypt 5 0 R >>
%%EOF";
    std::fs::write(&file, pdf).unwrap();

    let result = ReadFileTool
        .execute(json!({"path": file.to_str().unwrap()}), &ctx(dir.path()))
        .await;

    assert!(!result.is_error, "{}", result.content);
    assert!(result.content.contains("encrypted PDF detected"));
    assert!(result.content.contains("cannot decrypt"));
}

#[tokio::test]
async fn read_file_decodes_pdf_literal_octal_escapes() {
    let _guard = unconfined_rich_worker();
    let dir = tempdir().unwrap();
    let file = dir.path().join("octal.pdf");
    let pdf = b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >> endobj
4 0 obj << /Length 60 >> stream
BT /F1 12 Tf 10 10 Td (Octal\\040PDF) Tj ET
endstream endobj
trailer << /Root 1 0 R >>
%%EOF";
    std::fs::write(&file, pdf).unwrap();

    let result = ReadFileTool
        .execute(json!({"path": file.to_str().unwrap()}), &ctx(dir.path()))
        .await;

    assert!(!result.is_error, "{}", result.content);
    assert!(result.content.contains("Octal PDF"));
}
