# rightkit-compose

The evaluated composition model for Right Suite video (RK9). A `Composition` is a durable, serialisable document of layers in stack order: video and stills, cursor sprites with hotspots that follow a source layer, procedural backgrounds, rounded and circular clipping, shadows, temporal blur, canvas-space badges and captions, and an independently timed webcam. Transforms, crops and opacity are keyframed with easing.

`evaluate(composition, t_ms)` is pure. It maps a composition and a time to a `FrameDescription` with no renderer attached. CutRight's native compositor and its typed FFmpeg export both consume that description, so preview and export agree.

Catalog capability: `composition-model`. Not published (`publish = false`, version `0.0.0`).

## What it owns

- `model`: `Composition` (schema `rightkit.composition.v1`), `Canvas`, `Layer`, `Timing`, `Rect`, `Point`, `Rgba`.
- `track`: keyframed tracks (`Track<T>`, `Keyframe<T>`, `Easing`, `Lerp`, `sample`).
- `eval`: `evaluate` and `FrameDescription` (`FrameLayer`, `Payload`, `ClipPx`, `ShadowPx`, `MotionBlurFrame`).
- `validate`: `validate(&composition)`, run before every evaluation.
- `error`: `CompositionError`.

No rendering, codec or I/O lives here.

## When to use it

- Use it to describe a video composition as data and evaluate it per frame, for both preview and export.

Do not use it to render pixels or to write media files. Those belong to the compositor and to `rightkit-media`.

## Cargo features

None. The crate has no `[features]` table. Dependencies: `serde` and `thiserror`.

## Usage

Illustrative. The types and functions match `src/model.rs` and `src/eval.rs`, and the setup follows `tests/composition.rs`:

```rust
use rightkit_compose::{evaluate, Canvas, Composition};

let canvas = Canvas { width: 1920, height: 1080, frame_rate_num: 30, frame_rate_den: 1 };
let composition = Composition::new(canvas, 5_000);
let frame = evaluate(&composition, 1_000)?;
```

`evaluate` returns an error for a time at or past `duration_ms`. Add layers before evaluating a frame that should show content.

## Platform support

Pure Rust. No platform-specific code.

## Version and changes

Not published. See [INDEX.md](../../INDEX.md). No CHANGELOG.md in this crate.
