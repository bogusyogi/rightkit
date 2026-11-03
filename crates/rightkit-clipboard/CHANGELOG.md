# Changelog

## 0.1.0 (unreleased)

- First release (rightkit#956): the raw clipboard layer split out of `rightkit-tauri-shell` 0.7.3 with no behaviour change: `Board`, `ClipboardContent`, `ClipboardOptions`, `ImagePreference`, `ClipboardSnapshot`, `read`, `read_with`, `write`, `formats`, `named_board_refusal`, `purge_stale`, `dib_to_bmp`, `clipboard_temp_dir`, and the platform backends. `platform_snapshot` (the shell's private function) is public and returns `Result<_, String>`; the shell maps it to its own `ShellError`. The Windows backend uses `windows` 0.62 directly. Source-level move only: not compiled in this pass.
