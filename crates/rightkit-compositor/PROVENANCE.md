# Provenance — native compositor package

## Donors

| Donor | Repository | Revision | Licence | Tarball sha256 |
|---|---|---|---|---|
| OpenCut Classic | github.com/OpenCut-app/opencut-classic | `cf5e79e919144200294fb9fed22a222592a0aeea` | MIT (`crates/cutright-compositor/LICENSE-OpenCut`) | `d08e7e11b21142be4bdf88b8dc8e61524f43304b5c8c21c3abbad1bc1fcd2533` |
| Clypra | github.com/AIEraDev/Clypra | `efd8ab33af0cecc16ce46542ad6dd3953c8aef13` | MIT | `40ae50f3b0e77b2dfdb7b174e405e4a27f3b48ecb0037cbcadd111706eee2cae` |

## Copied files (OpenCut Classic)

`rust/crates/{gpu,compositor,effects,masks}` → `crates/cutright-compositor{-gpu,-effects,-masks,}`. Per-file sha256 of donor and staged copies: `docs/provenance-hashes.txt` (`*` = modified during adaptation; `->` = renamed).

Not copied: `rust/crates/{time,bridge}`, `rust/wasm`, `apps/**`, all TypeScript — CutRight owns time/revision model, `bridge` is a wasm proc-macro, the wasm crate and app are browser/application coupling this package rejects.

## Adaptations from donor source

- **Crate renames**: `gpu`/`compositor`/`effects`/`masks` → `cutright-compositor-*`; internal `use` paths rewritten accordingly.
- **`gpu/src/context.rs`**: all `#[cfg(all(feature = "wasm", target_arch = "wasm32"))]` code removed — `WebDisplay`, `CachedCanvasSurface`, the WebGL/GL-canvas fallback (`try_gl_fallback`, `import_offscreen_canvas_texture`, `render_texture_to_offscreen_canvas*`, `render_texture_to_gl_canvas_surface`, `render_texture_via_gl_canvas`, `gl_canvas`). The native path now fails with `AdapterUnavailable` instead of a GL-canvas fallback.
- **`gpu` additions (CutRight)**: `BYTES_PER_PIXEL`, `texture_from_pixels`/`write_texture_pixels` (native CPU-frame ingestion), `read_texture_pixels` (async readback for offscreen proof), `is_bgra`; `GpuError::{Poll, Readback}`.
- **`effects`/`masks`**: lib roots renamed `effects.rs`/`masks.rs` → `src/lib.rs` (byte-identical content); import renames only otherwise.
- **`compositor`**: import renames only; shader, blend-mode, layer/mask/effect pass semantics unchanged.
- **Edition**: donor `edition = "2024"` manifests rewritten to `edition = "2021"` to match the product workspace; no source changes were needed for that.
- Donor tests: the donor crates shipped no `#[test]`/`tests/` code — nothing to retain there; correctness coverage lives in `cutright-compose/tests/`.

## Derived (not copied) — Clypra

`cutright-compose/src/{identity,cache,scheduler}.rs` reimplement the semantics of Clypra `src-tauri/src/wgpu_compositor/{frame_request,frame_deadline,frame_cache,frame_resource,frame_scheduler}.rs` — FrameRequest/priority tiers, deadline max-heap ordering (earliest `present_by` pops first), expired-realtime drop, LRU byte-budgeted cache — rewritten against `SnapshotIdentity` so staleness is identity mismatch. No Clypra file was copied; nothing app/Tauri/telemetry-specific was reused. `yuv_ring_buffer` was *not* reused: RGBA upload via `write_texture` is the honest current path; YUV/ring transport is a later optimization, not claimed here.
