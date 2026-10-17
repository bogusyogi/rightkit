# rightkit-compose

The evaluated composition model for Right Suite video (RK9). A `Composition` is a durable, serialisable document of layers in stack order: video and stills, cursor sprites with hotspots that follow a source layer, procedural backgrounds, rounded and circular clipping, shadows, temporal blur, canvas-space badges and captions, click effects (ripple and highlight), and an independently timed webcam. Transforms, crops and opacity are keyframed with easing.

`evaluate(composition, t)` is pure. It maps a composition and a time in `Ticks` (705,600,000 per second, the CutRight clock) to a `FrameDescription` with no renderer attached. `evaluate_ms` is a thin wrapper for millisecond times. CutRight's native compositor and its typed FFmpeg export are meant to consume that description, so preview and export agree by design. This repository does not show either consuming it: `docs/audits/2026-10-10-audit-evidence/apps-g2.md:86` lists the CutRight mapper as a target, not a consumer.

Catalog capability: `composition-model`. Publishable on crates.io (wave 2.8), version `0.1.0`. See [CHANGELOG.md](CHANGELOG.md) for time, retime, opacity & fade changes.

## What it owns

- `model`: `Composition` (schema `rightkit.composition.v2`), `Canvas`, `Layer`, `Timing`, `Rect`, `Point`, `Rgba`.
- `ticks`: `Ticks`, `SignedTicks`, `TICKS_PER_SECOND`, `TICKS_PER_MS`, `Rational`.
- `retime`: `RetimeSegment` (`Play`, `Hold`, `Reverse`, `FramePlay`, `SignedHold`), `source_time_at` & `source_time_at_signed`.
- `opacity`: `Opacity`, `OpacityError`, `FrameFade` & `FrameFadeError`.
- `track`: keyframed tracks (`Track<T>`, `Keyframe<T>`, `Easing` including `Cosine` and `CameraSpring`, `Lerp`, `sample`).
- `eval`: `evaluate`, `evaluate_ms` and `FrameDescription` (`FrameLayer`, `Payload`, `ClipPx`, `ShadowPx`, `MotionBlurFrame`).
- `legacy` (internal): converts `rightkit.composition.v1` documents when read through `Composition::from_json_str`.
- `validate`: `validate(&composition)`, run before every evaluation.
- `error`: `CompositionError`.

No rendering, codec or I/O lives here.

## When to use it

- Use it to describe a video composition as data and evaluate it per frame, for both preview and export.

Do not use it to render pixels or to write media files. Those belong to the compositor and to `rightkit-ffmpeg`.

## Cargo features

None. The crate has no `[features]` table. Dependencies: `serde`, `serde_json` and `thiserror` (`Cargo.toml:17-19`).

## Usage

Illustrative. The types and functions match `src/model.rs` and `src/eval.rs`, and the setup follows `tests/composition.rs`:

```rust
use rightkit_compose::{evaluate, Canvas, Composition, Rational, Ticks};

let canvas = Canvas { width: 1920, height: 1080 };
let composition = Composition::new(canvas, Rational::new(30000, 1001), Ticks::from_ms(5_000));
let frame = evaluate(&composition, Ticks::from_ms(1_000))?;
```

`evaluate` returns an error for a time at or past `duration`. Add layers before evaluating a frame that should show content.

## Platform support

Pure Rust. No platform-specific code.

## Version and changes

See [CHANGELOG.md](CHANGELOG.md) and [INDEX.md](../../INDEX.md).

## Frame-exact source time & fades (0.2)

`Ticks::from_frames_rounded(frame, fps)` rounds nearest with exact ties toward
lower integer, matching CutRight. `Rational::frame_to_ticks(i64)` returns
`Option<SignedTicks>` with that rule, including negative indices;
`ticks_to_frame` rounds nearest, while `frame_at_ticks` returns owning frame
under `[ft(f), ft(f + 1))`. Invalid rates & overflow return `None`.
`Ticks::from_frames` keeps its existing floor rule.

Use `RetimeSegment::FramePlay` for frame-quantised playback. Its `start_frame`
is segment's absolute output frame; its `fps` must equal composition fps &
its rounded start must match layer start plus preceding retime durations.
Validation rejects misalignment, invalid rates, empty spans & source overflow.
At output frame `f`, source is `source_in + ft(f - start_frame)`, preserving
relative rounding even at 11 fps. Between frame starts it holds that source
frame. Use legacy `Play` for continuous tick-based playback & speeds.

```rust
use rightkit_compose::{FrameFade, Opacity, Rational, RetimeSegment, SignedTicks, Ticks};

let fps = Rational::new(11, 1);
let start = Ticks::from_frames_rounded(1, fps).unwrap();
let end = Ticks::from_frames_rounded(9, fps).unwrap();
let retime = vec![RetimeSegment::FramePlay {
    source_in: SignedTicks(-705_600_000),
    fps,
    start_frame: 1,
    frames: 8,
}]; // assign to Content::Video::retime; layer timing is [start, end)
let opacity = Opacity::new(f64::from(0.5f32)).unwrap().constant_track();
let fade = FrameFade {
    start_frame: 1, frames: 8, fade_in_frames: 3, fade_out_frames: 4,
};
let title_opacity = fade.opacity_track(fps, 1, 9).unwrap();
// Assign opacity or title_opacity to Layer::opacity.
```

`SignedHold { source_at: SignedTicks(-17), duration }` freezes negative source
time too. `source_time_at_signed` evaluates every retime kind losslessly;
existing `source_time_at` returns `None` when signed result cannot fit `Ticks`.
Frame schema v2 is unchanged and the default: `evaluate` returns
`rightkit.frame.v2` with `Payload::Video::source: Ticks`, and a negative signed
source is a `CompositionError::Invalid`. Opt in to signed source with
`evaluate_v3` / `evaluate_ms_v3`, which return `FrameDescriptionV3` tagged
`FRAME_SCHEMA_V3` (`rightkit.frame.v3`) whose `PayloadV3::Video::source` is
`SignedTicks(i128)`. Known asset duration caps the upper source bound only;
negative pre-roll stays negative and consumers choose decoder policy. Webcam
source time stays unsigned. `SignedTicks` serialises as a bare integer bounded
to `SignedTicks::MIN..=SignedTicks::MAX` (`i64::MIN..=u64::MAX`); values outside
fail to serialise or deserialise. The composition schema remains
`rightkit.composition.v2`. `RetimeSegment` gains `FramePlay` & `SignedHold`;
update exhaustive retime matches.

`Opacity::new` rejects values outside `0..=1`; `Opacity::clamped` explicitly
clamps finite values. Both reject NaN & infinity. Serde deserialisation validates
`Opacity` too. For existing raw opacity tracks, composition validation is unchanged and
returns `CompositionError::Invalid`.
Keyframe ordering & easing checks still apply.

`FrameFade::opacity_at(frame)` returns typed opacity. Positive ramps use
`(into + 1) as f32 / fade_in as f32` &
`(remaining + 1) as f32 / fade_out as f32`, combined with `min` & clamped.
Nonpositive lengths disable their ramp. Empty items & outside frames yield
zero. `opacity_track(fps, visible_start, visible_end)` emits layer-local Hold
keys using rounded absolute frame starts minus visible start. It keeps fade
phase when titles start before frame zero or end beyond visible interval.
Overlapping fades & one-frame ramps preserve CutRight's `f32` bits. For incoming
cross-fades, set only fade-in & multiply returned key values by validated static
opacity in `f64`, then cast evaluated result to `f32`, matching CutRight.
