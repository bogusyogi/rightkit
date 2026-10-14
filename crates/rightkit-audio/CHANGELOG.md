# Changelog


## 0.2.3

- Republished on the renamed dependencies: `rightkit-ffmpeg` (was `rightkit-media`). The 0.2.2 on crates.io was packaged before the renames and still named the old crates.
## 0.2.2

- New `Filter::pos(value)`: a positional option rendered as the bare value (`aresample=44100`, `atrim=0:5`, `volume=0.22`), so an app can reproduce an existing FFmpeg filter string byte for byte. Named options are unchanged.
- `rightkit-media` is depended on with `default-features = false`.

## 0.2.1

- `tracing` is now an optional dependency enabled by the `capture` feature. Only `capture::session` and `lifecycle` log, and both are `capture`-gated, so `--no-default-features` (pure filter-graph consumers) no longer builds `tracing`. Default builds are unchanged.
