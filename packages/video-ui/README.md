# @rightkit/video-ui

Video editing components: the finished-edit workspace (`TimelineEditorView`), the timeline,
inspector, colour and caption inspectors, transcript and changes panels, and the native preview
player, over a video engine and preview session the host app supplies. The package imports no
Tauri module and holds no app pages, settings, legal, onboarding, demo or product name: those stay
in the app.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.2.0 |
| License | MIT OR Apache-2.0 (vendored hyperframes pieces: Apache-2.0, see `src/editor/NOTICES.md`) |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/video-ui` | The components, the host contract, the typed video engine client and the hooks (a deliberate list in `src/index.ts`). |
| `@rightkit/video-ui/styles.css` | Every stylesheet the components use; import once. |

## Host contract

Install the host once at startup, before rendering:

```ts
import { installVideoHost, type VideoHost } from "@rightkit/video-ui";

installVideoHost({
  engine: { call, onEvent, status },          // the app's video engine (sidecar call, events, lifecycle)
  playback: { open, refreshFinishing, close, status, control, frame, setGain }, // native preview session
  assetUrl: (path) => convertFileSrc(path),   // a URL the webview can load for a local file
  allowMediaScope: (paths, recursive) => ..., // grant the webview the project folders
  mediaSourceId: (path) => ...,               // registered id for @rightkit/media-ui playback
  revealPath: (path) => ...,                  // show in the file manager
} satisfies VideoHost);
```

A Tauri host implements `engine` over `@rightkit/tauri-shell` (`shell.engine.callTagged`,
`shell.engine.onEvent`, `shell.engine.status`) and `playback` over its preview commands; the video
engine serves the methods of the RightKit video crates (`rightkit-video-project` with
`engine-commands`, `rightkit-autoedit`, `rightkit-shorts`, `rightkit-motion`). `RangePreview` plays
through `@rightkit/media-ui`'s `MediaPlayer`, so the host also provides its `MediaAdapters`.

The typed client (`engineCall`, the `lib/media` wrappers such as `projectShow`, `transcriptShow`,
`colourGet`, `thumbnails`, `playbackOpen`) runs over the installed host.

## Styling

Colour and type come from `@rightkit/theme` tokens (`--rk-*`). Spacing and radius read the host's
`--s1`..`--s5` and `--r-sm`/`--r-xl` tokens and fall back to the suite's 4px grid. Tailwind hosts add
`node_modules/@rightkit/video-ui/dist/**/*.js` to their content globs.

## Peer dependencies

`react`
