# rightkit-clipboard

Rich clipboard without Tauri (rightkit#956): text, HTML, PNG image, file lists and app-defined formats in one transaction.

- macOS: `NSPasteboard` (one primary item with every representation, plus one item per file). `Board::Named` targets a private pasteboard that never touches what the person copied; `Apple CFPasteboard ...` names are refused.
- Windows: the Win32 clipboard (`CF_UNICODETEXT`, `HTML Format`, `PNG`, `CF_HDROP`) with a message-only owner window for writes. No private clipboard: a named board is rejected.
- `read`, `read_with`, `write`, `formats`, `platform_snapshot` (files, a bitmap, text, with the AppleScript and `CF_DIB` fallbacks), `dib_to_bmp`, `purge_stale`, `clipboard_temp_dir`.

`rightkit-tauri-shell` keeps the typed `take_item` contract (it grants files through its path broker) over this crate and re-exports it as `rightkit_tauri_shell::clipboard`.

Not published (0.1.0, unreleased).
