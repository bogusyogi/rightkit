// rightkit-pdf 0.3.3 consumed by exact registry pin (feature `host`), outside the workspace.
//
// Proves: `write::Document` builds a one-page PDF in code; `inspect::page_graph_from_bytes` reads
// it (page numbers are one-based) and finds no text islands; `compose::compose_candidate` with an
// empty command list returns the base bytes unchanged; `save::stage_prepared_save` issues a token;
// `commit::commit_pdf_save` writes the verified candidate to a file and consumes the token once;
// `host::PdfHost::new` succeeds on every platform, and `open` answers Unsupported wherever the
// macOS PDFKit backend is absent.
// Does not prove: text edits (AddText and ReplaceSourceText need a font resolver and real text),
// merge, extract, raster, redact, forms, optimize, or a live PDFKit view on macOS.
// State: a temp directory only, removed on exit. No network, no real user data.
use std::fs;
use std::path::Path;

use rightkit_pdf::commit::commit_pdf_save;
use rightkit_pdf::compose::compose_candidate;
use rightkit_pdf::host::{
    AuthorizedPdfPath, HostConfig, HostError, HostFrame, HostHooks, NativeParent, OpenRequest,
    PdfHost,
};
use rightkit_pdf::inspect::page_graph_from_bytes;
use rightkit_pdf::model::{
    document_sha256, GlyphId, PdfEditError, PdfFontResolver, ResolvedPdfFont,
};
use rightkit_pdf::save::{stage_prepared_save, PdfSaveTokenStore};
use rightkit_pdf::write::Document;

// Refuses every font lookup. Safe here because the recipe composes an empty command list, which
// never asks for a font.
struct NoFonts;

impl PdfFontResolver for NoFonts {
    fn resolve(&self, _id: &str) -> Result<ResolvedPdfFont, PdfEditError> {
        Err(PdfEditError::MalformedContent)
    }

    fn glyphs_for_text(
        &self,
        _font: &ResolvedPdfFont,
        _text: &str,
    ) -> Result<Vec<GlyphId>, PdfEditError> {
        Err(PdfEditError::MalformedContent)
    }
}

fn s<E: std::fmt::Debug>(e: E) -> String {
    format!("{e:?}")
}

fn tiny_pdf() -> Result<Vec<u8>, String> {
    let mut doc = Document::new("1.7").map_err(s)?;
    let _page = doc.add_page(612.0, 792.0).map_err(s)?;
    doc.try_finish().map_err(s)
}

fn run(dir: &Path) -> Result<(), String> {
    let bytes = tiny_pdf()?;
    let source_sha = document_sha256(&bytes);

    // inspect: the blank page is page 1 and has no text islands; the graph is bound to the bytes.
    let graph = page_graph_from_bytes(&bytes, 1).map_err(s)?;
    assert_eq!(graph.page, 1);
    assert!(graph.islands.is_empty(), "a blank page has no text islands");
    assert_eq!(graph.document_sha256, source_sha);

    // compose: an empty command list returns the base bytes unchanged.
    let composed = compose_candidate(&bytes, &[], &NoFonts).map_err(s)?;
    assert_eq!(composed.bytes, bytes);
    assert!(composed.affected_pages.is_empty());

    // save: stage against the unchanged source, then commit to a different path in the temp dir.
    let source = dir.join("source.pdf");
    let target = dir.join("saved.pdf");
    fs::write(&source, &bytes).map_err(s)?;
    let source_str = source
        .to_str()
        .ok_or_else(|| "temp path is not UTF-8".to_string())?;
    let store = PdfSaveTokenStore::default();
    let staged = stage_prepared_save(
        &store,
        source_str,
        &source_sha,
        &source_sha,
        composed.bytes.clone(),
        composed.affected_pages.clone(),
        Vec::new(),
    )
    .map_err(s)?;
    let committed = commit_pdf_save(&store, &staged.token, Some(target.as_path())).map_err(s)?;
    assert_eq!(committed.document_sha256, source_sha);
    assert_eq!(fs::read(&target).map_err(s)?, bytes);

    // The token is single-use: a second commit with it is refused.
    assert!(commit_pdf_save(&store, &staged.token, Some(target.as_path())).is_err());

    // host: `new` succeeds on every platform; `supported` is true only with the macOS backend.
    // Without it every call answers Unsupported before it looks at its arguments.
    let host = PdfHost::new(HostConfig::default(), HostHooks::default()).map_err(s)?;
    assert_eq!(host.supported(), cfg!(target_os = "macos"));
    if !host.supported() {
        let path = AuthorizedPdfPath::from_authorized(&source).map_err(s)?;
        let request = OpenRequest {
            owner: "recipe-owner".to_owned(),
            path,
            frame: HostFrame {
                x: 0.0,
                y: 0.0,
                width: 640.0,
                height: 480.0,
            },
            reader_rgb: None,
            initial_view: None,
            password: None,
        };
        let parent = NativeParent::detached();
        let refused = host.open("recipe-window", &parent, request);
        assert!(
            matches!(refused, Err(HostError::Unsupported)),
            "open must answer Unsupported without the macOS backend"
        );
    }
    Ok(())
}

fn main() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("rightkit-pdf-recipe-{}", std::process::id()));
    fs::create_dir_all(&dir).map_err(s)?;
    let result = run(&dir);
    let _ = fs::remove_dir_all(&dir);
    result?;
    println!("OK rightkit-pdf 0.3.3 (registry consumer)");
    Ok(())
}
