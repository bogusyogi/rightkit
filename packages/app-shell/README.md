# @rightkit/app-shell

Brand-neutral desktop shell for Right Suite apps: fused sidebar + titlebar on one chrome surface, full-width active nav row with 12px antialiased SVG concave joins into the content plane, grouped nav, Jump-to search (Cmd/Ctrl+K), bottom-left wordmark with settings and appearance (system/light/dark), status-footer slot, macOS overlay traffic-light space, and Windows/Linux caption buttons driven by a `@rightkit/platform-ui` `WindowControlAdapter`.

```tsx
import "@rightkit/app-shell/shell.css";
import { AppShell, createThemeStore } from "@rightkit/app-shell/react";
```

Brand colour comes from CSS: `:root { --rk-brand: #7040c4; --rk-brand-dark: #9567f5; --rk-wordmark: "Tanker", sans-serif; }`.
Flat suite tokens (`--chrome --plane --ink --accent --fill --line ...`) are defined by `shell.css` and shared with app views.

The app owns routing, view bodies, settings content, and Tauri window calls (adapter). The package owns layout, chrome, theme persistence, and nav filtering.
