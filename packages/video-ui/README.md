# @rightkit/video-ui

Video editing components: the finished-edit workspace (`TimelineEditorView`), the timeline,
inspector, colour and caption inspectors, transcript and changes panels, and the native preview
player, over a video engine and preview session the host app supplies. The root entry imports no
Tauri module and holds no app pages.

CutRight's application UI lives here too, on its own entry (`@rightkit/video-ui/app`): the desktop
workspace and its pages (Projects, Media, Auto-edit, Shorts, Phrases, Retention, Assets, Exports,
batch delivery, bundle export, recipe library, prompt panel, progress, Settings), the onboarding and
legal gate, agent demo mode and the app's services, over the same host. Its Tauri host is
`@rightkit/video-ui/tauri`.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Version | 0.2.1 |
| License | MIT OR Apache-2.0 (vendored hyperframes pieces: Apache-2.0, see `src/editor/NOTICES.md`) |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/video-ui` | The components, the host contract, the typed video engine client and the hooks (a deliberate list in `src/index.ts`). No Tauri import. |
| `@rightkit/video-ui/app` | Everything the root exports, plus the app: `DesktopWorkspace`, the pages (`Home`, `SourcesView`, `AutoEditView`, `ShortsView`, `PhrasesView`, `RetentionView`, `AssetsView`, `ExportsView`, `BatchDelivery`, `BundleExport`, `RecipeLibrary`, `PromptPanel`, `ProgressView`, `SettingsView`, `TimelineView`), `OnboardingGate`, `configureLegal`, `setHostAppName`/`hostAppName`, demo mode (`useDemo`, `demo*` bindings, `DemoSettings`), the services (`logs`, `licenseClient`, `updates`, `themeStore`, `shellBridge`, `quitNow`, `mediaAdapters`, pickers, GenRight and model helpers) and `videoAppHost`. No Tauri import: it runs over the installed host's `app` part. |
| `@rightkit/video-ui/tauri` | `tauriVideoHost()`: the Tauri `VideoHost`, `app` part included, making exactly the native calls 0.1.x made (and `shell`, the app's `@rightkit/tauri-shell` instance). The one entry that imports Tauri. |
| `@rightkit/video-ui/styles.css` | Every stylesheet the components use; import once. |
| `@rightkit/video-ui/app.css` | The app's stylesheets plus `styles.css`; an app host imports this instead of `styles.css`. |

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
  app: { ... },                               // only for @rightkit/video-ui/app (below)
} satisfies VideoHost);
```

A Tauri host implements `engine` over `@rightkit/tauri-shell` (`shell.engine.callTagged`,
`shell.engine.onEvent`, `shell.engine.status`) and `playback` over its preview commands; the video
engine serves the methods of the RightKit video crates (`rightkit-video-project` with
`engine-commands`, `rightkit-autoedit`, `rightkit-shorts`, `rightkit-motion`). `RangePreview` plays
through `@rightkit/media-ui`'s `MediaPlayer`, so the host also provides its `MediaAdapters`.

The typed client (`engineCall`, the `lib/media` wrappers such as `projectShow`, `transcriptShow`,
`colourGet`, `thumbnails`, `playbackOpen`) runs over the installed host.

## The app entry

`@rightkit/video-ui/app` needs the host's `app` part (`VideoAppHost`): the native command bridge
(`invoke`, `listen`) the licence and logs clients, demo mode and media playback derivatives run over,
`onEngineRaw` (every engine frame, for the pages' progress readers), the window (`minimize`,
`toggleMaximize`, `close`, `setFullscreen`), `onDragDrop`, the pickers (`pickFolder`, `pickFile`,
`pickFiles`), `appVersion`, `checkUpdate`, `relaunch`, `quit` and `exit`. Without it,
`videoAppHost()` throws and names the fix.

A Tauri app (CutRight) installs the shipped host and keeps its startup as it was:

```ts
import { installVideoHost, setHostAppName, configureLegal, OnboardingGate, DesktopWorkspace } from "@rightkit/video-ui/app";
import { tauriVideoHost } from "@rightkit/video-ui/tauri";
import "@rightkit/video-ui/app.css";

installVideoHost(tauriVideoHost());
setHostAppName("CutRight");
configureLegal({ manifest, contract, texts });
```

`tauriVideoHost()` calls the commands 0.1.x called, so the app's `src-tauri` serves the same set: the
shell host (`rk_shell_*` through `@rightkit/tauri-shell`), `playback_open`, `playback_refresh_finishing`,
`playback_close`, `playback_status`, `playback_control`, `playback_frame`, `playback_set_gain`,
`media_scope_allow`, `authorised_media_source_id`, `reveal_path`, `prepare_registered_media_playback`,
`cancel_media_playback`, the `rightkit_license_*` and `rightkit_logs_*` commands and the `demo_*` commands
and events; plus the Tauri plugins `process`, `updater` and `dialog`.

The product name (`setHostAppName`) and the legal facts (`configureLegal({ manifest, contract, texts,
step? })`: the host's manifest, generated contract and bundled texts; `step` overrides the gate's copy and
look, which default to 0.1.x's) are host-supplied. Build switches stay typeof-guarded: `__RIGHTKIT_QA__`
(QA dialog answers) and `__RIGHTKIT_DEMO__` (the demo settings chunk), as the host's Vite `define` sets them.

## Styling

Colour and type come from `@rightkit/theme` tokens (`--rk-*`). Spacing and radius read the host's
`--s1`..`--s5` and `--r-sm`/`--r-xl` tokens and fall back to the suite's 4px grid. Tailwind hosts add
`node_modules/@rightkit/video-ui/dist/**/*.js` to their content globs.

## Peer dependencies

`react`
