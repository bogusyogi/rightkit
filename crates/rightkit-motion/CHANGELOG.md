# Changelog

## 0.3.0 (unpublished)

- rightkit#1144: the loopback project host takes the app's policy. BREAKING (`server` feature): `project_server::Project` replaces `gsap_js` with `injection: Injection`; new `Injection { runtime, swaps }`, `ScriptSwap { url_contains, url_ends_with, route, body }` (`ScriptSwap::gsap`), `Injection::deterministic(gsap)` (the previous behaviour: bundled `hf-runtime.js` plus the GSAP swap), `swap_scripts`, `serve(dir, entry, Injection) -> Handle { url, port(), logs(), shutdown() }` and `ProjectServer::start_with`. `ProjectServer::start(dir, entry, gsap_js)` is unchanged. Swap routes must be under `/__hf/`. Host guard, containment, byte ranges, log sink unchanged.
- rightkit#1144: new feature `browser-source`: `browser_source::BrowserFrameSource`, a `render::FrameSource` over a managed `rightkit-browser` page (open, size, transparent background for alpha, wait for `__hf_meta`; seek with paint wait; CDP PNG capture with omit-background for alpha). `CompositionMeta` returns duration and size. Adds `rightkit-browser` and `tokio` (rt, time) behind the feature only.

## 0.2.3 (unpublished)

- Added caller-supplied encoder settings on `EncodeSpec` (#803, GenRight parity), defaults unchanged:
  - `vp9_crf: Option<u32>`: VP9 alpha `-crf` 0 to `VP9_CRF_MAX` (63). `None` keeps `Quality::vp9_crf()` (40, 32, 24). Refused for any other output.
  - `audio_codec: Option<AudioCodec>`: `AudioCodec::Opus` (libopus 128k) is the VP9 alpha audio codec and lifts the former video-only refusal. The caller must choose it only when the build lists `libopus`; the render then fails at ffmpeg start if it does not. `AudioCodec::Aac` is for H.264 and MPEG-4. `None` keeps PCM for ProRes and AAC otherwise.
- Breaking for struct-literal callers of `EncodeSpec` (new fields); the only in-repo literal, `render.rs`, now passes `None`.

## 0.2.2 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.2.1 (unreleased)

- Fix: feature `engine-commands` declares the `chrono` dependency its contact sheet uses.
- New, additive (feature `engine-commands`, off by default): `cmd::thumb` (`media.thumbnail`: cached typed-FFmpeg frame extraction from a source or render, with a host-supplied `GradeLookup` for graded source frames) and `cmd::contactsheet` (`contactsheet.make`: a PNG contact sheet of a render bound to its revision and hash), moved from CutRight's `cutright-engine`. The on-disk sheet schema id `cutright.contactsheet.v1` is kept.

## 0.2.0 — unpublished staging (2026-10-18)

- New, additive (GenRight EXTEND, 2026-11-01): `composition`, the composition document a render takes (frames are a pure function of `n / fps`; HTML projects are one `html` layer; the beat template names in `KNOWN_TEMPLATES`), from `genright-engine/src/motion_doc.rs`. Feature `server` (off by default): `project_server`, the loopback HTTP host for one HTML composition project (127.0.0.1 only, `Host` origin check against DNS rebinding, never a path outside the project lexically or through a link, byte ranges for media, `/__hf/log` capture, the deterministic runtime `RUNTIME_JS` injected into the entry page), from `genright-engine/src/motion_server.rs`. GSAP is not bundled: `ProjectServer::start` takes the GSAP build to serve in place of remote GSAP URLs, or `None` to leave them. Adds optional `rightkit-http` 0.3.9 (default features off).

Render and encode owner (rightkit#476, GenRight port; ported from `genright-engine` `motion_service.rs` and `motion_encode.rs`). New feature `encode`; without it the crate is unchanged.

- Dependency `rightkit-transcript` moves from 0.3.0 to 0.4.0 (`Cargo.toml`). The registry's latest `rightkit-motion` is 0.1.0.

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
