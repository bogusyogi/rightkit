# rightkit-overlay 0.2.0 (PK08, draft consumer recipe)

Standalone consumer, outside the workspace, pinning `rightkit-overlay =0.2.0` exactly. It uses the
product-neutral `OverlayModel<Kind>` with a caller-owned `Kind` (apply, hit_test) and the pure
geometry functions `fills_monitor`, `rects_overlap` and `covering_window`, fed with synthetic
candidates.

State: none. No real windows, no Win32 calls, no timers, no network, no files. Run only by the CI
consumer lane.

Not covered: `place`, the Windows window-procedure, style and create functions in `windows`,
`OverlayHost` (rightkit-tauri-shell, PK08-4), and the z-order walk on a live desktop.
