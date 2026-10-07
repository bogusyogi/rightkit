# @rightkit/app-shell

Brand-neutral React frame for Right Suite desktop apps. Everything is optional and composable:

- **Default fused layout** (`AppShell`): sidebar + titlebar on one chrome surface, antialiased SVG concave join into the page plane, grouped nav with roving arrow keys, palette-trigger search, status footer, wordmark + settings + appearance, collapsible/narrow sidebar. Every region has a slot or override; `sidebar={false}` / `titlebar={false}` drop regions.
- **Custom layouts** (MailRight-style): skip `AppShell`; compose `ShellProvider` + `AppFrame` + `Titlebar` + `Plane` + `NavList` or your own markup. Palette, shortcuts, toasts, dialogs and menus work unchanged.
- **Command palette** (`CommandPalette`, `useCommands`), **shortcut registry + help** (`createShortcutRegistry`, `useShortcuts`, `ShortcutHelp`), **toasts** (`toast()`, `useToast`, `ToastViewport`), **modals** (`ModalShell`, `Dialog`, `ConfirmDialog`, `PromptDialog`, `shell.confirm()`/`prompt()`), **context menu** for app-owned menus (`ContextMenu`, `useContextMenu`), **ui kit** (`Button`, `IconButton`, `Input`, `Toggle`, `SegmentedControl`, `Card`, `Badge`, `EmptyState`, `Field`, `Pill`, `StatusRow`, `PageShell`, `ErrorBoundary`), **theme hooks** (`createThemeStore`, `useTheme`).
- **Native bridge contract** (`AppShellBridge`): structural, no Tauri import; `@rightkit/shell` implements it.

```tsx
import "@rightkit/app-shell/shell.css";
import { AppShell, PageShell } from "@rightkit/app-shell/react";
```

Brand colour is CSS only: `:root { --rk-brand: #7040c4; --rk-brand-dark: #9567f5; --rk-wordmark-font: "Tanker", sans-serif; }`.
All tokens are `--rk-*` (`--rk-ink`, `--rk-accent`, `--rk-plane` ...) so the stylesheet never overwrites an app's own variables. Apps that want the old unprefixed names can opt in with `import "@rightkit/app-shell/suite-aliases.css"`.
See `API.md` in the lane notes for the full reference and per-app migration. Tests: `pnpm test` runs real headless Chrome (muted) end to end.
