# rightkit-compose

The evaluated composition model for Right Suite video (RK9). A `Composition` is a durable, serialisable document of layers in stack order: video and stills, cursor sprites with hotspots that follow a source layer, procedural backgrounds, rounded and circular clipping, shadows, temporal blur, canvas-space badges and captions, click effects (ripple and highlight), and an independently timed webcam. Transforms, crops and opacity are keyframed with easing.

`evaluate(composition, t)` is pure. It maps a composition and a time in `Ticks` (705,600,000 per second, the CutRight clock) to a `FrameDescription` with no renderer attached. `evaluate_ms` is a thin wrapper for millisecond times. CutRight's native compositor and its typed FFmpeg export are meant to consume that description, so preview and export agree by design. This repository does not show either consuming it: `docs/audits/2026-10-10-audit-evidence/apps-g2.md:86` lists the CutRight mapper as a target, not a consumer.

Catalog capability: `composition-model`. Publishable on crates.io (wave 2.8), version `0.1.0`. See [CHANGELOG.md](CHANGELOG.md) for the 0.1.0 time, retime and easing changes.

## What it owns

- `model`: `Composition` (schema `rightkit.composition.v2`), `Canvas`, `Layer`, `Timing`, `Rect`, `Point`, `Rgba`.
- `ticks`: `Ticks`, `TICKS_PER_SECOND`, `TICKS_PER_MS`, `Rational`.
- `retime`: `RetimeSegment` (`Play`, `Hold`, `Reverse`) and `source_time_at`.
- `track`: keyframed tracks (`Track<T>`, `Keyframe<T>`, `Easing` including `Cosine` and `CameraSpring`, `Lerp`, `sample`).
- `eval`: `evaluate`, `evaluate_ms` and `FrameDescription` (`FrameLayer`, `Payload`, `ClipPx`, `ShadowPx`, `MotionBlurFrame`).
- `legacy` (internal): converts `rightkit.composition.v1` documents when read through `Composition::from_json_str`.
- `validate`: `validate(&composition)`, run before every evaluation.
- `error`: `CompositionError`.

No rendering, codec or I/O lives here.

## When to use it

- Use it to describe a video composition as data and evaluate it per frame, for both preview and export.

Do not use it to render pixels or to write media files. Those belong to the compositor and to `rightkit-media`.

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
