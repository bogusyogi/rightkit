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

## Entry points and optional `fabric`

The root entry (`@rightkit/media-ui`) holds the player, adapters and every pure
helper (edit recipe, export math, crop math); it never imports `fabric`.
`ImageViewer` / `ImageEditor` live in **`@rightkit/media-ui/image`** because the
text overlay uses `fabric`, which is an *optional peer dependency*.

- **Player-only apps** install nothing extra: no `fabric`, no native build approval.
- **Image apps** add `fabric` (`^7.4`) themselves and, because `fabric` lists the
  Node-only native addon `canvas` as an optional dependency (unused in the
  browser), set pnpm `allowBuilds: { canvas: false }` in the workspace config.

```ts
import { MediaPlayer } from "@rightkit/media-ui";
import { ImageViewer, ImageEditor } from "@rightkit/media-ui/image";
```

## Host seams

All optional; defaults preserve prior behaviour.

- `ImageViewer`: `toolbarContainer` portals `.rk-iv-toolbar` into a host element.
- Adapters: `defaultFontId` (font for new text layers); `acquireResource` may
  return a release function **or** `{ release, update }`; `update({ dirty,
  stateExternalized })` is called when the edit state changes, instead of re-acquiring.
- `MediaPlayer`: `showArtwork`, `showOpenOriginal`, `icons` (map by `IconName`),
  `speeds` + `speedControl` (`"cycle"` | `"menu"`), `labels` (every string; see
  `DEFAULT_MEDIA_PLAYER_LABELS`) and `renderError({ message, retry, openOriginal })`.

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
rebrand via `--rk-media-*` and `--rk-iv-*` custom properties set on `:root` or any
ancestor (active toolbar buttons: `--rk-iv-active-bg` / `--rk-iv-active-fg`). The
only shared fallback is the suite accent `--rk-accent`; other app variables are never read.

## Status

Listed in `rightkit-versions.json`; pin the exact version, and adopt only after your
app's own e2e passes with it. Outcome coverage: `pnpm test`
(pure contracts) and `pnpm test:browser` (headless-Chrome real playback,
remux fallback, and export pixel round-trip).
