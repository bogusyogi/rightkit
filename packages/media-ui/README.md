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

- `ImageViewer`: `toolbarContainer` portals only the capsule row (`.rk-iv-bar`) into a
  host element; the crop/adjust/text/resize panels stay in the viewer above the stage.
  `panelsContainer` optionally portals the panels elsewhere. `toolbarLabel` names the
  capsule (`role="toolbar"`, default "Image tools"); it has arrow-key roving focus
  (Left/Right/Home/End, one tab stop). Buttons act on mousedown without taking focus
  and on keyboard/AT click only.
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

### Styling contract (stable)

These class names are public. Renames or removals happen only in a major version.

`rk-iv`, `rk-iv-empty`, `rk-iv-toolbar`, `rk-iv-panels`, `rk-iv-bar`, `rk-iv-capsule`,
`rk-iv-btn`, `rk-iv-btn-round`, `rk-iv-sep`, `rk-iv-dim`, `rk-iv-input`, `rk-iv-num`,
`rk-iv-num-sm`, `rk-iv-text`, `rk-iv-font`, `rk-iv-zoom`, `rk-iv-panel`, `rk-iv-panel-wrap`,
`rk-iv-field`, `rk-iv-color`, `rk-iv-swatch`, `rk-iv-slider`, `rk-iv-slider-label`,
`rk-iv-slider-value`, `rk-iv-range`, `rk-iv-stage`, `rk-iv-canvas-pos`, `rk-iv-canvas-box`,
`rk-iv-canvas-inner`, `rk-iv-img`, `rk-iv-overlay`, `rk-iv-crop`, `rk-iv-badge`,
`rk-iv-badge-left`, `rk-iv-badge-center`, `rk-iv-sibling`, `rk-iv-sibling-left`,
`rk-iv-sibling-right`. State hook: `data-active` on `.rk-iv-btn`.

Public tokens (custom properties; defaults equal 0.2.0 rendering):
`--rk-iv-capsule-radius` (999px), `--rk-iv-capsule-border` (`1px solid <line>`),
`--rk-iv-capsule-shadow` (none), `--rk-iv-capsule-bg` (panel);
`--rk-iv-btn-size` (28px, height), `--rk-iv-btn-radius` (6px; 999px on round buttons),
`--rk-iv-btn-border` (0), `--rk-iv-btn-font-size` (11px);
`--rk-iv-input-height` (28px), `--rk-iv-input-bg`, `--rk-iv-input-focus` (accent; also the
keyboard focus ring on buttons);
`--rk-iv-badge-font-family`, `--rk-iv-badge-bg`, `--rk-iv-badge-shadow`;
`--rk-iv-sibling-size` (40px), `--rk-iv-sibling-bg`. Plus the existing `--rk-iv-accent`,
`-bg`, `-panel`, `-line`, `-fg`, `-fg-dim`, `-active-bg`, `-active-fg`.

## Status

Listed in `rightkit-versions.json`; pin the exact version, and adopt only after your
app's own e2e passes with it. Outcome coverage: `pnpm test`
(pure contracts) and `pnpm test:browser` (headless-Chrome real playback,
remux fallback, and export pixel round-trip).
