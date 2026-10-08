# @rightkit/app-shell

Brand-neutral React frame for Right Suite desktop apps: fused sidebar and titlebar layout, command palette, shortcut registry and help, toasts, dialogs, context menus, UI primitives and theme hooks. Every region is optional and can be replaced by a slot.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Rust pair | None directly. The native side is reached through `@rightkit/shell`, which implements this package's bridge contract and pairs with `rightkit-shell`. |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/app-shell` | Navigation, theme, match, geometry, toast, shortcut, bridge and command modules (`dist/index.js`) |
| `@rightkit/app-shell/react` | `ShellProvider`, `AppShell`, `AppFrame`, `Titlebar`, `Plane`, `NavList`, `PageShell`, palette, toasts, overlays, context menu, hooks (`useShell`, `useCommands`, `useShortcuts`, `useToast`, `useTheme`) and UI primitives |
| `@rightkit/app-shell/shell.css` | Stylesheet. Tokens are `--rk-*`. |
| `@rightkit/app-shell/suite-aliases.css` | Optional unprefixed token aliases for apps that need the old names |

## Peer dependencies

| Peer | Range |
|---|---|
| `react` | `>=18 <20` |
| `@rightkit/platform-ui` | `>=0.1.1 <1` |

## Example

Illustrative. Import the stylesheet once and mount the provider near the root.

```tsx
import "@rightkit/app-shell/shell.css";
import { ShellProvider, AppShell } from "@rightkit/app-shell/react";

export function Root() {
  return (
    <ShellProvider>
      <AppShell />
    </ShellProvider>
  );
}
```

`ShellProvider` takes optional `bridge` (an `AppShellBridge`, for example from `@rightkit/shell`), `platform`, `paletteChord` and `helpChord`.

## Known issues

- Peer React is `<20` here, while `doc-render`, `i18n`, `media-ui` and `pdf-ui` accept `<21` (npm audit 2026-10-08, cross-package finding 4).
