# Apps whose live data root is a legacy home directory (M6)

Decision for rightkit#792 (CodeRight `~/.coderight`, `CODERIGHT_HOME`, a `daemon.port` file read by four processes). Evidence: `crates/rightkit-daemon/src/root_cutover.rs` (module docs, `RootCutover::{new, run, recover, status, selected_root, retire_old_root}`, lines 1-50, 964-1030, 1193, 1295, 1694) and `crates/rightkit-daemon/CHANGELOG.md` 0.5.3.

## Rule 1: no `adopt_legacy`; cut over to the M6 root

M6 stays `<platform data dir>/Orthic Labs/<App>`. `AppRoot` has one root and no mode that keeps a legacy path as the root, because that would let an app satisfy M6 on paper while its data lives elsewhere. An app with a legacy live root moves to the M6 root with `RootCutover`, which is the supported cutover that leaves a marker and forwards the old path:

- The marker is the record `.rightkit-root-cutover.json` at the new root (`CUTOVER_RECORD`). Until its phase is `switched`, readers get the legacy root; after, the new one. A crash at any step is recovered by `RootCutover::recover`, run at startup before any root is opened.
- Every process (CLI, daemon, Tauri shell, iOS) resolves its root with `RootCutover::selected_root(&app_root, &legacy_path)`. That one call is the forwarder: no process reads the legacy path or the new path directly, so no two processes can disagree about which root is live.
- User-edited config, hooks and agents travel with the tree move (`FsMove::Tree`); a discovery file such as `daemon.port` is recreated by the daemon at the selected root on its next start, and a reader that finds none under the selected root treats the daemon as not running. Files that must not travel are named by the participant in `owned_paths`.
- The old root is never modified by the cutover; `retire_old_root` renames it aside or trashes it only after the cutover is `completed` and settled, and there is no copy kept beyond that rename (the dev-install rule: no previous copy).
- A CLI-first app runs the same cutover from the CLI at its first start under the cutover lock; the app does not need a GUI to migrate.

A root that is not cut over yet is an M6 exception in the app's `.rightkit/ownership-exceptions.json` with an expiry, not a supported steady state.

## Rule 2: release builds read no root override

No build (release, dev or QA) reads an environment override of the app root. `rightkit-daemon` 0.5.3 removed every selector (`RIGHTKIT_SUITE_ROOT`, `RIGHTKIT_QA_DATA_DIR` and the rest), and `mode::decide` returns `Mode::Release` without reading the environment. An app's own override variable (`CODERIGHT_HOME`) follows the same rule: the app removes it in non-mobile builds. The only thing that chooses a root other than the platform default is the persisted selection record of Rule 1, which the user's data, not a variable, carries. Development uses the real data under the `rightkit-qa` snapshot and restore (rightkit#1437).
