## Decisions for Adrian (D1–D6, preconditions in the design draft)

1. **D1, two tiers & P13.** Public RightKit stays as it is: generic, MIT/Apache, published by CI to crates.io/npm. Product domain code (45 packages marked `private` above, including `rightkit-privileged-helper`) goes to a private tier. P13 is amended: "app-specific product domain logic and presentation is a private-tier package; the app keeps only its shell." Without this, moving product code would publish proprietary source.
2. **D2, private channel.** Proposed: a private `rightkit-private` repo with its own catalog, qualified locally (Mac + Dell), distributed as git dependencies pinned to tagged commits for Cargo, npm & SwiftPM. Legion uses the same channel.
3. **D3, vendored code.** Each row of the vendored table:
   - DOCX: fork & trim (proposed) or keep upstream.
   - Tree-sitter grammars: crates.io.
   - WasmKit: SwiftPM.
   - tauri-runtime-wry: upstream the patch.
   - sherpa-onnx & prebuilt xcframeworks: replace with `rightkit-asr` built for iOS.
   - ONNX Runtime: binary via `rightkit-onnx`.
   - GenRight legacy Node server (8.6k lines): drop once confirmed unused.
4. **D4, confirm tiers.** Confirm each new package's tier (column above) and each extended package marked `check`. Check them against import edges, so that no public package imports private code.
5. **D5, publication hold.** `docs/GATE-CONTRACT.md` records a hold on publication. Either lift it or schedule the waves after it.
6. **D6, Legion as a skills distributor (via the private channel).** CodeRight's skills, plugins & recipes crates move to Legion, so CodeRight needs Legion's crates under published names (crate names & registry).

Also settled: skills never become RightKit components. Legion owns them. Every NEW/EXTEND package gets an EGM-reviewed design doc before its code moves.
