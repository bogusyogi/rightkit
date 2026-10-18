# Changelog

## 0.2.0 - 2026-10-18

Render and encode owner (rightkit#476, GenRight port; ported from `genright-engine` `motion_service.rs` and `motion_encode.rs`). New feature `encode`; without it the crate is unchanged.

- `render`: `FrameSource` trait (`seek(time_s)`, `capture() -> PNG`) so the crate depends on no browser; `render` seeks each frame at `n / fps`, converts it and pipes it to one ffmpeg child through `rightkit_ffmpeg::run_with_stdin`; `render_with_fallback` applies the MPEG-4 retry rule; `capture_still`, `export_still`, `export_poster` (poster frame at two fifths, as GenRight); cancellation through `CancellationToken`; typed `RenderError` whose `code()` keeps GenRight's codes (`h264_encoder_unavailable`, `capture_size_mismatch`, `capture_decode`, `ffmpeg_pipe`, `cancelled`) plus `alpha_encoder_unavailable`.
- `encoder`: `pick_encoder` (hardware H.264 via `rightkit_ffmpeg::pick_h264`, then `libopenh264`, then MPEG-4 only when allowed), the bits-per-pixel quality map (0.06 / 0.12 / 0.25) as `target_bitrate`, MPEG-4 `-q:v` 6 / 3 / 1, `pick_alpha_encoder` and `AlphaSupport` for ProRes 4444 (`prores_ks`, `.mov`) and VP9 alpha (`libvpx-vp9`, `.webm`), `build_command` for each path.
- `frame`: `png_to_ppm`, `png_to_pam` (RGB_ALPHA) and `png_dimensions`; the LGPL ffmpeg has no PNG decoder.
- Behaviour changes from GenRight, all deliberate: `libx264` is never picked (GPL; `rightkit-ffmpeg` excludes it); hardware order on Windows is now NVENC then Media Foundation (`pick_h264`); bit rate is integer arithmetic; an encoder-start failure is also detected for alpha encoders; VP9 alpha is video-only and refuses audio inputs; ProRes ignores `Quality`.
- New optional dependency `rightkit-ffmpeg` 0.2.9 (default features off); `image` is now also pulled by `encode`; dev-dependency `tempfile`. The lockfile needs the CI patch.
- Written without a compile; CI is the first compiler. The `encode_e2e` tests are `#[ignore]` and need a real ffmpeg.

## 0.1.0 - 2026-10-17

Initial version (rightkit#264).

- `beats`: bracketed-script parsing and snapping to `rightkit-transcript` word timings, with typed errors.
- `sheet`: inspect-sheet planning with the `rightkit.motion.sheets.v1` manifest; defaults (2x3 grid, 960x540 tiles, 480 px minimum tile width) are to confirm. Feature `assemble` composes PNG sheets.
- The `take` module is not built; it stays gated until a producer is named.
- Written without a compile; CI is the first compiler.
