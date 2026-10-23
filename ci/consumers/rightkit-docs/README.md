# rightkit-docs 0.1.2 registry consumer (draft, PK16)

Standalone consumer, outside the workspace, pinning `rightkit-docs =0.1.2` exactly. It exercises the
smallest pure DOCX family: `write` with `CreateDocx`, `read` by content, an edit through
`expect_sha256` that publishes one replacement, and three refusals: `source_changed` (stale hash),
`target_exists` (create over a file without `overwrite`) and `unsupported_format` (plain bytes).

State: files only under a fresh temp directory, removed at the end. No network, no real user data.
Pure Rust, so no Word, Excel or LibreOffice is needed. Run only by the CI consumer lane.

Not covered: XLSX and PDF reads and writes, Markdown rendering, OOXML loss receipts, macro and
encryption refusals, symlink targets, and the concurrent-writer race. This recipe must be revisited
if the family list changes.
