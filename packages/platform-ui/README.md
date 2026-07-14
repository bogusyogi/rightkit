# @rightkit/platform-ui

Brand-neutral cross-platform UI primitives for Right Suite desktop apps.

The package owns Tauri-backed platform detection, display-only shortcut formatting,
semantic keyboard-chord rendering, platform capability flags, overridable accessible
window-control names, and headless window-control adapters. Product title bars,
colors, spacing, navigation, and other branded UI stay in each app.

Production platform detection uses `@tauri-apps/plugin-os`. Browser QA must call
`bootstrapQaPlatform` with explicit canonical input. Query overrides require both
`qa=1` and `qaPlatform`; environment overrides require `RIGHTKIT_QA=1` and
`RIGHTKIT_QA_PLATFORM`. The helper never reads globals, `navigator.platform`, or a
user-agent. `Win32` is accepted as the existing QA alias for `windows`. Android and
unrecognized values deliberately resolve to `unknown`, never silently to Linux.

```ts
import {
  bootstrapQaPlatform,
  createFullscreenControls,
  createWindowControlLabels,
  createWindowControls,
  formatChord,
  getPlatform,
} from "@rightkit/platform-ui";
import { Kbd, usePlatform } from "@rightkit/platform-ui/react";

bootstrapQaPlatform({
  query: window.location.search,
  env: {
    RIGHTKIT_QA: import.meta.env.VITE_APP_QA,
    RIGHTKIT_QA_PLATFORM: import.meta.env.VITE_APP_QA_PLATFORM,
  },
});

const platform = getPlatform();
const renderedKeys = formatChord(storedShortcut, platform);
const controls = createWindowControls(nativeWindowAdapter);
const fullscreen = createFullscreenControls(nativeFullscreenAdapter); // optional
const labels = createWindowControlLabels({ close: "Close inspector" });
```

`getPlatform()` is the plain static getter. React consumers import the real, stable
`usePlatform()` hook from `@rightkit/platform-ui/react`. `formatChord` only creates
display tokens; the app remains the owner of the stored shortcut string. Fullscreen
is intentionally separate so apps can implement only minimize/maximize/close.

This package remains internal until each core app passes the per-OS
shortcut/window contract and retains its branded title-bar snapshots.
