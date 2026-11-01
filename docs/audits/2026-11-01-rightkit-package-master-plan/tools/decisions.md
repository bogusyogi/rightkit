## Decisions for Adrian

1. **DOCX editor (ViewRight, 171k lines vendored).** Recommendation: own the `core` & `react` packages as `@rightkit/docx-editor`, drop vendored `agents` & `i18n` (51k). Alternative: keep upstream as an npm dependency & only own ViewRight's integration.
2. **Tree-sitter grammars (Pulse/Membrane, 71k lines vendored).** Recommendation: use crates.io grammar crates through `rightkit-repo-graph`; no RightKit copy.
3. **WasmKit (CodeRight iOS, 27k).** Recommendation: SwiftPM dependency, delete vendored copy.
4. **tauri-runtime-wry fork (HeardRight, 7k).** Recommendation: upstream the patch; until then, one pinned fork owned by `rightkit-tauri-shell`, not per app.
5. **sherpa-onnx & prebuilt xcframeworks (HeardRight iOS).** Recommendation: replace with RightKit's Rust KWS/ASR (`rightkit-asr`) built for iOS.
6. **ONNX Runtime binary (HeardRight iOS).** Keep as binary dependency, resolved only through `rightkit-onnx`.
7. **GenRight legacy Node server (`server/`, `compat-lib/`, `bin/`, 8k lines).** Mapped DROP as superseded by the Rust studio port; confirm nothing still calls it.
8. **Skills live in Legion.** All skill packs, doctrine, personas, skill manifests/loaders & host projection go to Legion (CutRight skills & cutaway, Pulse membrane skill, CodeRight skills/plugins/recipes crates). RightKit has no skills package.
9. **`rightkit-mesh`.** Already named as a RightKit crate but lives inside the Pulse repo; move it to RightKit & publish.
10. **Single-app packages.** 50+ new packages have one consumer today (e.g. `rightkit-code-audit`, `rightkit-terminal-ui`, video editing family). They are listed as RightKit packages per the factory goal; the family grouping above is the order to extract them in.
