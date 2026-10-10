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
| `react` | `>=18 <21` |
| `@rightkit/platform-ui` | `>=0.2.1 <1` |
| `@tauri-apps/plugin-os` | `>=2 <3`, optional. Native platform detection loads it lazily; without it the browser's platform report is used. |

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

### Keybindings

Built-in chords can be moved or disabled through `keybindings` (the same object works on `ShellProvider` and on `AppShell`'s `shell` prop). Set a chord to `false` to turn that binding off.

```tsx
<ShellProvider keybindings={{ palette: false, help: "mod+/", sidebar: "mod+b" }}>
```

| Key | Default | Binding |
|---|---|---|
| `palette` | `mod+k` | Command palette |
| `help` | `?` | Shortcut help |
| `sidebar` | `mod+\` | Sidebar toggle |

`keybindings` wins over the legacy `paletteChord` and `helpChord` props. An app whose editor owns `mod+k` (for example an "add link" action) sets `palette: false` and opens the palette from a button with `PaletteTrigger` or `shell.palette.show()`. `paletteWidth` (default 640) sets the palette width in px.

### Class names and slots

Visual components take `className` for their root and `slots` for named regions. Each slot is a class string merged after the `rk-*` classes, so an app can restyle a region without forking markup.

| Component | `slots` regions |
|---|---|
| `ModalShell`, `ConfirmDialog`, `PromptDialog` | `root` (surface) |
| `Dialog` | `root`, `header`, `body` (wrapper rendered only when set), `footer` |
| `ContextMenu` | `root`, `item`, `separator`, `label` |
| `CommandPalette` | `root`, `field`, `input`, `list`, `group`, `option`, `empty` |
| `ToastViewport` | `root`, `toast`, `message`, `action`, `dismiss` |
| `Titlebar` | `root`, `side`, `lights`, `title`, `drag`, `end`, `caption` |
| `AppShell` | `root`, `titlebar`, `sidebar`, `sidebarFoot`, `body`, `plane` |

### Theme store

`createThemeStore` (and the provider's store) takes:

- `defaultChoice` (default `"system"`): used when nothing is stored.
- `alwaysPaintDataTheme` (default `true`): `system` resolves to `light` or `dark` and `data-theme` is always set to that value. Pass `false` to keep the old behaviour, where `system` removes `data-theme` and CSS alone decides.
- `systemMedia`: the OS colour-scheme query. The store listens to it itself, so `data-theme` follows the OS even with no React mounted. `store.resolved()` returns the current light or dark value, and `store.dispose()` detaches the listener.

### Titlebar recipe (matching an existing titlebar)

The default titlebar is sized by tokens, so an app can reproduce its own titlebar without forking the markup. Set these on `:root` (or on `.rk-shell`):

| Token | Default | Controls |
|---|---|---|
| `--rk-titlebar-height` | `52px` | Bar height |
| `--rk-sidebar-width` | `220px` | Start-cluster width over the sidebar column |
| `--rk-titlebar-start-pad` | `16px` | Left padding of the start cluster |
| `--rk-titlebar-side-pad-end` | `14px` | Right padding of the start cluster |
| `--rk-titlebar-pad-end` | `14px` | Right padding of the bar (caption buttons bleed into it) |
| `--rk-titlebar-gap` | `10px` | Gap between titlebar children |
| `--rk-lights-width` | `72px` | Space reserved for the macOS traffic lights |
| `--rk-caption-width` | `46px` | Width of each Windows caption button |
| `--rk-caption-close-bg` / `--rk-caption-close-ink` | `#c42b1c` / `#fff` | Close button hover |
| `--rk-chrome` | theme | Titlebar, sidebar and frame background |

For a fully custom titlebar, pass `titlebar={<YourBar />}` (or `titlebar={false}`) and keep `AppFrame`, `Plane` and `NavList`. Use `Titlebar` with `slots` when only classes need to change. `titlebarStart` and `titlebarEnd` take the app's own controls, and `wordmark` takes the brand lockup.

### Palette and toast tokens

`--rk-palette-width` (640px), `--rk-toast-offset` (16px) and `--rk-plane-radius` (20px) size the palette, the toast stack and the content corner.

## Known issues

- The default titlebar draws its own nav and caption layout. An app with a very different titlebar should use `titlebar={...}` rather than tuning tokens past their useful range.

- Peer React is `<21`, the same range as `doc-render`, `i18n`, `media-ui` and `pdf-ui` (`package.json:36`).
