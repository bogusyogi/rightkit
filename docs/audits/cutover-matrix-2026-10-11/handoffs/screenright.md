# ScreenRight (archive) — RightKit migration handoff

Source root: /Volumes/D/claude/archive/screenright-2026-10-08

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/screenright.json)

## RightKitDiagnostics — N

Built local Swift helper writes redacted JSONL diagnostics; it does not import RightKitDiagnostics, so retained source has an active local implementation overlapping owner diagnostic storage.

If archive is reactivated, replace local helper with version-pinned RightKitDiagnostics integration and preserve ScreenRight event schema/redaction policy.

- Local: [RightKitDiagnostics](/Volumes/D/claude/archive/screenright-2026-10-08/menu/RightKitDiagnostics.swift:6) — Built local helper owns persistent diagnostic sink.
- Local: [record](/Volumes/D/claude/archive/screenright-2026-10-08/menu/RightKitDiagnostics.swift:17) — Local route redacts fields then appends JSONL.
- Local: [swift sources](/Volumes/D/claude/archive/screenright-2026-10-08/scripts/build-app.mjs:84) — Production macOS build compiles RightKitDiagnostics.swift into menu helper.
- Owner: [JSONLDiagnosticStore](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitDiagnostics/JSONLDiagnosticStore.swift:77) — Owner provides Swift JSONL diagnostic store.
- Owner: [record](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitDiagnostics/JSONLDiagnosticStore.swift:105) — Owner records one redacted diagnostic event under a file lock.

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
