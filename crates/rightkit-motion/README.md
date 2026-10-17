# rightkit-motion

Product-neutral motion helpers for Right Suite video. Pure Rust: no browser, no Typst, no media decode, no I/O loop. Depends on `rightkit-transcript` only (for `TimedWord`).

Catalog capabilities: `motion-beat-sync`, `motion-inspect-sheets` and `motion-takes` (declaration-only, see below). Version `0.1.0`. Decision record: `docs/design/motion-owner.md` (rightkit#264).

## What it owns

- `beats`: `parse_script(&str)` reads a script with `[spans]` (`\[`, `\]` and `\\` escape) into a `BracketedScript`. `snap(&script, &[TimedWord], SnapConfig)` returns one `Beat` per span with `start_ms`, `end_ms` and a word range. Spans map positionally to the caller's animations. A word that does not match is a typed `BeatError`, never a guess; fuzzy ASR matching stays in `rightkit-align`.
- `sheet`: `plan_sheets(SheetSpec)` lays out storyboard inspect sheets and serialises as the `rightkit.motion.sheets.v1` manifest (times, frame numbers, tile positions). Defaults, all **to confirm**: six tiles per sheet in a 2 column by 3 row grid, 960x540 tiles, and a 480 px minimum tile width (a narrower tile is refused). Tile size and grid are configurable on `SheetSpec`.

## Cargo features

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

Do not use it to decode or encode media (`rightkit-ffmpeg`), render pages (`rightkit-browser`) or score sheets (`rightkit-review` takes measurements as data).

## Licence

MIT OR Apache-2.0.
