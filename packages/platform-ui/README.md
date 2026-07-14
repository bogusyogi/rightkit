# @rightkit/platform-ui

Brand-neutral cross-platform UI primitives for Right Suite desktop apps.

The package owns Tauri-backed platform detection, canonical shortcut formatting,
semantic keyboard-chord rendering, platform capability flags, stable accessible
window-control names, and a headless window-control adapter. Product title bars,
colors, spacing, navigation, and other branded UI stay in each app.

Production platform detection uses `@tauri-apps/plugin-os`. Browser QA must set
`globalThis.__RIGHTKIT_QA_PLATFORM__` explicitly; the package never infers the OS
from `navigator.platform` or user-agent strings.

```ts
import { createWindowControls, formatChord, getPlatform } from "@rightkit/platform-ui";
import { Kbd } from "@rightkit/platform-ui/react";

const platform = getPlatform();
const label = formatChord("mod+shift+space", platform);
const controls = createWindowControls(nativeWindowAdapter);
```

This package remains internal until each core app passes the per-OS
shortcut/window contract and retains its branded title-bar snapshots.
