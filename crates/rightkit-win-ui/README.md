# rightkit-win-ui

Win32 UI toolkit: RAII window and GDI wrappers, software canvas, GDI surface and text painter, vector glyphs and shell file icons. Layered-window presentation with constant opacity (`surface`) and, with the `wic` feature, PNG re-encoding of an image file (`wic`). Windows only; the crate is empty elsewhere. Failed Win32 calls go to the sink set with `diag::set_error_sink`.

Moved into RightKit from `pulse-windows` (Pulse) on 2026-11-01.
