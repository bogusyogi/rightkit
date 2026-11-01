## Decisions for Adrian

1. **D1, vendored code.** Each row of the vendored table:
   - DOCX: fork & trim (proposed) or keep upstream.
   - Tree-sitter grammars: crates.io.
   - WasmKit: SwiftPM.
   - tauri-runtime-wry: upstream the patch.
   - sherpa-onnx & prebuilt xcframeworks: replace with `rightkit-asr` built for iOS.
   - ONNX Runtime: binary via `rightkit-onnx`.
   - GenRight legacy Node server (8.6k lines): drop once confirmed unused.
2. **D2, borderline extensions.** 23 existing packages gain code from only one app (marked `check`). Confirm each is generic. Otherwise it is product code and stays in the app.
3. **D3, publication hold.** `docs/GATE-CONTRACT.md` records a hold on publication. Either lift it or schedule the waves after it.
4. **D4, Legion as a skills distributor.** CodeRight's skills, plugins & recipes crates move to Legion. Decide how CodeRight consumes Legion's crates (names & route).

Settled: everything extracted is public RightKit (crates.io/npm). Product-specific logic & UI stay in apps (P13). Skills live in Legion. Every NEW/EXTEND package gets an EGM-reviewed design doc before its code moves.
