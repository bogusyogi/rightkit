# Changelog

## 0.1.1
- Add the macOS backend `mac::MacBackend` (moved from HeardRight `media_control_mac.rs`). The probe script path is a host input; diagnostics go through the crate's event sink.

## 0.1.0

- First release. Lifecycle, worker and Windows backend extracted from HeardRight `tauri-app-next/src-tauri/src/media_control*` (2026-11-19).
- Diagnostics go to a host-installed `EventSink`.
- `Backend` is public; `install_backend` lets a host supply a backend where none is built in.
