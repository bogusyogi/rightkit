## Decisions (resolved 2026-11-01 by Adrian, except where marked)

1. **Scope.** Every capability goes to RightKit, including ones a single app uses today. That way a new app can find it. "Product-specific" means only how each app looks & composes the shared shell, plus declared per-app exceptions. Example: ViewRight's devtools in the HTML iframe is an app declaration over a `rightkit-tauri-shell` option. Everything is public on crates.io/npm.
2. **DOCX (eigenpal).** Do not fork the 171k lines.
   - `core` and `i18n` are identical to the published npm 1.9.0, so use those packages.
   - `agents` is unused, so drop it.
   - `react` carries ViewRight's host-toolbar bridge. Use the published package plus that small patch, carried by `@rightkit/docx-editor`, and send the patch upstream as a PR (Apache-2.0, active repo). ViewRight's own DOCX integration (`src/documents/docx`) becomes `@rightkit/docx-editor`.
3. **sherpa-onnx.** It is already rewritten in `rightkit-asr`, and Mac & Windows are tested. Delete the iOS xcframework once the iOS build of `rightkit-asr` is tested.
4. **GenRight Node server.** Drop it; the suite runs no Node services.
5. **Tree-sitter grammars, WasmKit, ONNX Runtime, tauri-runtime-wry.** Recommendation: do not own them. Use the upstream crates, SwiftPM package and binary as plain dependencies, and upstream the Tauri patch. Owning them would add maintenance for no product gain, unless one of them needs changes we make.
6. **Publication.** The hold is lifted. Each package is its own unit with its own `Cargo.lock`, and it is published separately: compile locally, qualify on GitHub, then publish automatically. The go-ahead is given. `publish.yml` needs the local broker's signed receipts, so each unit's broker step runs on the Mac or Dell.
7. **Skills.** Legion owns its own skills. App-specific recipes and agent skills stay in their apps (CutRight and GenRight recipes, Pulse's chat skill). CodeRight's `skills`, `plugins` and `recipes` crates are deleted, because CodeRight uses the installed Legion.
8. **Shell.** At most 2% per app. Shell code that every app re-implements becomes shared packages (`rightkit-engine-host`, `rightkit-tauri-shell`, `RightKitUI`, `rightkit-android`, `@rightkit/react-shell`, `rightkit-daemon`).
