# rightkit-pdf 0.3.3 (PK08, draft consumer recipe)

Standalone consumer, outside the workspace, pinning `rightkit-pdf =0.3.3` exactly with the `host`
feature (default features stay on: merge, edit, fonts). It exercises a one-page PDF built in code
with `write::Document`, then `inspect::page_graph_from_bytes` (page 1, no text islands),
`compose::compose_candidate` with an empty command list, `save::stage_prepared_save` and
`commit::commit_pdf_save` into a temp directory (the token is single-use), and
`host::PdfHost::new`, whose `open` answers `Unsupported` on every platform without the macOS
PDFKit backend.

State: a temp directory only, removed on exit. No network, no real user data, no files outside
the temp directory. Run only by the CI consumer lane.

Not covered: AddText and ReplaceSourceText (need a font resolver and real text), merge, extract,
raster, redact, forms, optimize, the macOS PDFKit view, and `commit` onto the source path (the
same-file recheck). The recipe must be revisited if the PDF public surface named above changes.
