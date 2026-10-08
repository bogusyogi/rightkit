# @rightkit/media-ui

Brand-neutral media player and image viewer/editor for Right Suite apps. The root entry holds the player, the adapter provider and every pure image helper. Image viewer and editor live in a separate entry because they need the optional `fabric` peer.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Rust pair | None declared. Compatibility-copy playback is wired by the app's `preparePlayback` adapter, which the app backs with `rightkit-playback`. |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/media-ui` | `MediaPlayer`, `MediaAdaptersProvider`, `useMediaAdapters`, label defaults, pure image-edit and export helpers, crop helpers, image save helpers, image-viewer state helpers, icons. Never imports `fabric`. |
| `@rightkit/media-ui/image` | `ImageViewer`, `ImageEditor`, `ImageViewerProps`, `ImageTool` (needs `fabric`) |
| `@rightkit/media-ui/styles.css` | Stylesheet. Import once. |

## Peer dependencies

| Peer | Range | Required |
|---|---|---|
| `react`, `react-dom` | `>=18 <21` | Yes |
| `fabric` | `^7.4.0` | Optional. Only for `@rightkit/media-ui/image`. |

## Example

Illustrative. Player-only apps need no `fabric`.

```tsx
import { MediaAdaptersProvider, MediaPlayer } from "@rightkit/media-ui";
import "@rightkit/media-ui/styles.css";

// Wrap the app (or a subtree) once with the host's adapters:
// <MediaAdaptersProvider adapters={{ resolveSrc, preparePlayback, cancelPlayback }}>
//   <MediaPlayer ... />
// </MediaAdaptersProvider>
```

Image apps add `fabric` and import `ImageViewer` or `ImageEditor` from `@rightkit/media-ui/image`. Because `fabric` lists the native `canvas` addon as an optional dependency, a pnpm workspace may need `allowBuilds: { canvas: false }`.

Adapters are all optional: `resolveSrc`, `preparePlayback`, `cancelPlayback`, `setFullscreen`, `listImageSiblings`, `listSystemFonts`, `bundledFonts`, `saveImage`, `printImage`, `acquireResource`, `notify`, `logError`.

Theme with `--rk-media-*` and `--rk-iv-*` custom properties on `:root` or an ancestor. The shared fallback is `--rk-accent`.

## Known issues

- `react-image-crop` (`^11.1.2`) is a regular dependency, so every consumer installs it, even consumers that only use the root entry. It is imported only under `./image` (`src/image/ImageCropTool.tsx`, `src/image/ImageViewerStage.tsx`). Moving it to an optional peer is pending (npm audit 2026-10-08, media-ui finding 3).
