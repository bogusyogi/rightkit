# ScreenRight archive RightKit cutover matrix

Reviewed retained source at `b4e2104ed7a32167bb803a93779ebda36df41e9b`. Targets: archived macOS Node recorder & compiled Swift menu helper.

Counts: **1 Y / 1 N / 0 M / 95 NA** across 97 catalog rows.

`npm:@rightkit/release` is routed through declared `right-release` build, upload, publication, signing, & artifact-mirroring commands.

`swift:RightKitDiagnostics` remains local: compiled Swift writes redacted JSONL diagnostics. If archive returns to maintenance, replace it with version-pinned `RightKitDiagnostics` while preserving ScreenRight event schema & redaction.

Recorder, composition, audio, media, filesystem, & updater code remain retained Node/Swift implementation. Their Rust or TypeScript owners have no source dependency or usable bridge from archive's release-shim Cargo target; they are NA, not inferred duplicates. Local Swift DMG updater also differs from `RightKitArtifacts` tar.gz directory installer.

Source-only review; no build, artifact, runtime, or release qualification ran.
