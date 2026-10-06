# @rightkit/media-ui

Brand-neutral media components shared by the Right Suite apps.

- **`MediaPlayer`** — audio/video player for native WebViews: seek/volume/speed
  controls, subtitle tracks + CC toggle, keyboard controls
  (`space`/`k`/`j`/`l`/arrows/`m`/`f`/`r`), single-active-player exclusivity,
  DOM fullscreen with a native-window fallback, retry/proxy/open-original error
  actions, and a one-shot **compatibility-copy fallback** through the
  `preparePlayback` adapter (backed by the `rightkit-playback` crate's
  source-preserving FFmpeg remux).
- **`ImageViewer` / `ImageEditor`** — zoom/pan/fit/rotate/flip viewer with
  same-folder sibling paging plus crop, adjustments, text overlay, resize and
  canvas export; the edit recipe + undo history live in a path-keyed store that
  survives unmount.

Provenance: `MediaPlayer` merges ScrapeRight's `src/components/MediaPlayer.tsx`
and GenRight's player; `ImageViewer`/`ImageEditor` port ViewRight's
`src/documents/image/` stack (crop/rotate/text/fabric overlay, undo store).

## Adapters

No app imports: everything app-specific arrives through `MediaAdapters`, set
once via `<MediaAdaptersProvider adapters={…}>` or per-component `adapters`.
All fields optional — the components degrade cleanly without them:

| Adapter | Enables |
|---|---|
| `resolveSrc(src)` | path → playable URL (e.g. `convertFileSrc`) |
| `preparePlayback({id, requestId})` | compat-copy fallback (call `prepare_media_playback`) |
| `cancelPlayback(requestId)` | cancel in-flight prep on unmount |
| `setFullscreen(on)` | native window fullscreen when DOM fullscreen is unavailable |
| `listImageSiblings(path)` | folder paging chevrons + wheel/arrow paging |
| `listSystemFonts()` | system font list in the text panel |
| `bundledFonts` | app-shipped fonts for text overlay + export |
| `saveImage` / `printImage` | persistence + printing of exported bytes |
| `acquireResource` / `notify` / `logError` | host resource accounting + UX |

## Theming

`import "@rightkit/media-ui/styles.css"` once. Defaults are neutral slate;
rebrand via `--rk-media-*` and `--rk-iv-*` custom properties, which fall back
to common host variables (`--accent`, `--panel`, `--ink`, …) when present.

## Status

Internal during the shared-media gate — apps pin the published version only
after it appears in `rightkit-versions.json`. Outcome coverage: `pnpm test`
(pure contracts) and `pnpm test:browser` (headless-Chrome real playback,
remux fallback, and export pixel round-trip).
