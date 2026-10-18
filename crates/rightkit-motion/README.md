# rightkit-motion

Product-neutral motion helpers for Right Suite video. Pure Rust: no browser, no Typst, no webview. Beats and sheets depend on `rightkit-transcript` only (for `TimedWord`) and do no I/O. The `encode` feature adds the render and encode driver, which runs ffmpeg only through `rightkit-ffmpeg`.

Catalog capabilities: `motion-beat-sync`, `motion-inspect-sheets` and `motion-takes` (declaration-only, see below). Version `0.2.0`. The render and encode surface (feature `encode`) is not yet a catalog capability. Decision record: `docs/design/motion-owner.md` (rightkit#264).

## What it owns

- `beats`: `parse_script(&str)` reads a script with `[spans]` (`\[`, `\]` and `\\` escape) into a `BracketedScript`. `snap(&script, &[TimedWord], SnapConfig)` returns one `Beat` per span with `start_ms`, `end_ms` and a word range. Spans map positionally to the caller's animations. A word that does not match is a typed `BeatError`, never a guess; fuzzy ASR matching stays in `rightkit-align`.
- `sheet`: `plan_sheets(SheetSpec)` lays out storyboard inspect sheets and serialises as the `rightkit.motion.sheets.v1` manifest (times, frame numbers, tile positions). Defaults, all **to confirm**: six tiles per sheet in a 2 column by 3 row grid, 960x540 tiles, and a 480 px minimum tile width (a narrower tile is refused). Tile size and grid are configurable on `SheetSpec`.

- `render` (feature `encode`): `FrameSource` is the trait the app implements (`seek(time_s)` then `capture() -> PNG`; headless Chrome, a webview or a test generator). `render` seeks frame `n` to `n / fps`, converts the PNG and writes it to ffmpeg's stdin through `rightkit_ffmpeg::run_with_stdin` (an owned child tree; the token cancels it). `render_with_fallback` adds GenRight's MPEG-4 retry. `capture_still`, `export_still` and `export_poster` give stills and the poster (frame at two fifths).
- `encoder` (feature `encode`): `pick_encoder` chooses hardware H.264 (`rightkit_ffmpeg::pick_h264`), then `libopenh264`, then MPEG-4 only when `allow_mpeg4_fallback` is set (`fallback: true` in the encoder). `libx264` is never picked. `Quality` maps to bits per pixel 0.06 / 0.12 / 0.25 (H.264), `-q:v` 6 / 3 / 1 (MPEG-4) and `-crf` 40 / 32 / 24 (VP9). `AlphaSupport::probe` and `pick_alpha_encoder` cover ProRes 4444 (`prores_ks`, `.mov`, PCM audio) and VP9 alpha (`libvpx-vp9`, `.webm`, video only: LGPL builds carry no libopus or libvorbis); a missing alpha encoder is `alpha_encoder_unavailable`, never an opaque fallback. `build_command` returns the ffmpeg arguments for each path.
- `frame` (feature `encode`): `png_to_ppm`, `png_to_pam` and `png_dimensions`, because the LGPL ffmpeg has no PNG decoder. Alpha encoders get PAM (`RGB_ALPHA`) frames, the rest PPM.

## Features

- `encode`: the render and encode modules above. Adds `rightkit-ffmpeg` (default features off) and `image` (PNG codec only).
- `assemble`: adds `assemble(&SheetPlan, FnMut(&TileSlot) -> Frame)`, composing PNG sheets with `image` (PNG codec only). The caller decodes frames through `rightkit-ffmpeg`; frames must be exactly tile-sized.

## Not built: take format

The decision record also describes a `take` module (append-only checksummed frame container). It is gated until a producer is named, so no module and no `zstd` dependency exist yet. The `motion-takes` capability is declaration-only until then.

## Usage

Illustrative:

```rust
use rightkit_motion::{parse_script, snap, SnapConfig};
use rightkit_transcript::TimedWord;

let script = parse_script("We ship [three] tools")?;
let words = vec![
    TimedWord::new("We", 0, 200),
    TimedWord::new("ship", 200, 500),
    TimedWord::new("three", 600, 900),
    TimedWord::new("tools", 900, 1300),
];
let beats = snap(&script, &words, SnapConfig::default())?;
assert_eq!((beats[0].start_ms, beats[0].end_ms), (600, 900));
# Ok::<(), Box<dyn std::error::Error>>(())
```

Render and encode (feature `encode`), with the app resolving ffmpeg through `rightkit-ffmpeg` and implementing `FrameSource`:

```rust,ignore
use rightkit_ffmpeg::CancellationToken;
use rightkit_motion::{pick_encoder, probe_inventory, render_with_fallback, Quality, RenderJob};

let inventory = probe_inventory(&ffmpeg, std::time::Duration::from_secs(30))?;
let encoder = pick_encoder(&inventory, std::env::consts::OS, allow_mpeg4, &mut |_| true)?;
let job = RenderJob {
    width: 1280, height: 720, fps: 30, duration_s: 4.0, quality: Quality::Standard,
    encoder, audio: vec![], output: "out.mp4".into(), poster: Some("poster.png".into()),
};
let outcome = render_with_fallback(
    &ffmpeg, &mut my_frame_source, &job, allow_mpeg4,
    &CancellationToken::new(), &mut |progress| println!("{progress:?}"),
)?;
```

Tests: the unit tests need no ffmpeg (argument construction, fallback order, quality maps, PNG to PAM, and the driver against a shell stand-in for ffmpeg on Unix). `tests/encode_e2e.rs` holds `#[ignore]`d tests that run a real ffmpeg; a missing ffmpeg or encoder prints a `SKIP` line, and `RIGHTKIT_REQUIRE_ASSETS=1` turns that into a failure.

Do not use it to decode media (`rightkit-ffmpeg`), render pages (`rightkit-browser`) or score sheets (`rightkit-review` takes measurements as data).

## Licence

MIT OR Apache-2.0.
