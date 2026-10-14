# @rightkit/react-shell

Brand-neutral React frame for Right Suite desktop apps: fused sidebar and titlebar layout, command palette, shortcut registry and help, toasts, dialogs, context menus, UI primitives and theme hooks. Every region is optional and can be replaced by a slot.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Rust pair | None directly. The native side is reached through `@rightkit/tauri-shell`, which implements this package's bridge contract and pairs with `rightkit-tauri-shell`. |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/react-shell` | Navigation, theme, match, geometry, toast, shortcut, bridge and command modules (`dist/index.js`) |
| `@rightkit/react-shell/headless` | `useToggle` and `useSegmentedControl`: switch and segmented-control behaviour and ARIA as props for your own markup. Imports only `react`. See [Headless toggle and segmented control](#headless-toggle-and-segmented-control). |
| `@rightkit/react-shell/react` | `ShellProvider`, `AppShell`, `AppFrame`, `Titlebar`, `Plane`, `NavList`, `PageShell`, palette, toasts, overlays, context menu, hooks (`useShell`, `useCommands`, `useShortcuts`, `useToast`, `useTheme`) and UI primitives |
| `@rightkit/react-shell/shell.css` | Stylesheet, unlayered (the 0.5.x behaviour). Tokens are `--rk-*`. |
| `@rightkit/react-shell/shell.layered.css` | The same rules inside the cascade layer `rk`, so host utilities and unlayered host CSS override them. See [Adopting with a locked UI](#adopting-with-a-locked-ui). |
| `@rightkit/react-shell/shell.structure.css` | Only the rules the frame, overlays, menu and toasts need to function: position, z-index, layout, overflow, pointer-events. No colour, type, radius or decoration. |
| `@rightkit/react-shell/shell.visual.css` | Colour, type, radius, spacing, shadows and motion. Pairs with the structure file. |
| `@rightkit/react-shell/suite-aliases.css` | Optional unprefixed token aliases for apps that need the old names |

## Peer dependencies

| Peer | Range |
|---|---|
| `react` | `>=18 <21` |
| `react-dom` | `>=18 <21` |
| `@rightkit/platform-ui` | `>=0.2.8 <1` (0.6.4 raised the minimum from `>=0.2.2`: the `ctrlormeta` chord is formatted by 0.2.8) |
| `@tauri-apps/plugin-os` | `>=2 <3`, optional. Native platform detection loads it lazily; without it the browser's platform report is used. |

## Example

Illustrative. Import the stylesheet once and mount the provider near the root.

```tsx
import "@rightkit/react-shell/shell.css";
import { ShellProvider, AppShell } from "@rightkit/react-shell/react";

export function Root() {
  return (
    <ShellProvider>
      <AppShell />
    </ShellProvider>
  );
}
```

`ShellProvider` takes optional `bridge` (an `AppShellBridge`, for example from `@rightkit/tauri-shell`), `platform`, `paletteChord` and `helpChord`.

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

### Shortcut registry chords and per-shortcut options

`createShortcutRegistry` takes chords as strings: `mod` (Cmd on macOS, Ctrl elsewhere), `ctrl`, `meta` (`cmd`), `alt` (`option`), `shift`, and `ctrlormeta` (exactly one of Ctrl or Meta, on every platform; `CtrlOrMeta` is the same token). `mod` is the platform primary, so `ctrlormeta` is the chord for "Ctrl or Cmd/Win" when a shortcut must not depend on the OS. Each `ShortcutDef` accepts three opt-ins, all defaulting to the previous behaviour:

| Option | Default | Effect |
|---|---|---|
| `allowComposing` | `false` | Fire during IME composition. Without it, composing events never fire this shortcut. |
| `allowDefaultPrevented` | `false` | Fire when another handler already called `preventDefault()`. |
| `ignoreExtraModifiers` | `false` | Match when modifiers the chord does not name are also held (AltGr reports Ctrl+Alt; `esc` with any modifier). Named modifiers must still be held. |

Chord display for `ctrlormeta` comes from `@rightkit/platform-ui` `formatChord` 0.2.8 or later.

### Class names and slots

Visual components take `className` for their root and `slots` for named regions. Each slot is a class string merged after the `rk-*` classes, so an app can restyle a region without forking markup.

| Component | `slots` regions |
|---|---|
| `ModalShell` | `root` (surface), `overlay` (backdrop) |
| `ConfirmDialog`, `PromptDialog`, `ActionDialog` | `root` (surface), `header`, `body`, `footer`, `overlay` |
| `Dialog` | `root`, `header`, `body` (wrapper rendered only when set), `footer`, `overlay` |
| `ContextMenu` | `root`, `item`, `separator`, `label`, `lead`, `scrim` |
| `CommandPalette` | `root`, `field`, `input`, `list`, `group`, `option`, `empty`, `icon` |
| `ToastViewport` | `root`, `toast`, `message`, `action`, `dismiss`, `glyph` |
| `Titlebar` | `root`, `side`, `lights`, `title`, `drag`, `end` (wraps the end content), `caption` |
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
| `--rk-chrome` | theme | Sidebar and frame background |
| `--rk-top-bg` | `var(--rk-chrome)` | Titlebar background only |
| `--rk-top-border` | `none` | Titlebar bottom border (any `border` value). Counted inside `--rk-titlebar-height`. |
| `--rk-top-end-gap` | `var(--rk-titlebar-gap)` | Gap between the end cluster's children, separate from the bar gap |
| `--rk-caption-margin-start` | `6px` | Left margin of the caption group |
| `--rk-caption-margin-end` | `calc(-1 * var(--rk-titlebar-pad-end))` | Right margin of the caption group (bleeds into the bar padding) |
| `--rk-caption-hover-bg` | `var(--rk-fill-2)` | Caption button hover background (close keeps `--rk-caption-close-bg`) |

Derived defaults (`--rk-top-bg`, `--rk-caption-hover-bg`, the caption margins and `--rk-top-end-gap`) are not set on `:root`. Their fallbacks are in the rules, so a dark theme still follows `--rk-chrome` and `--rk-fill-2`. Set them on `:root` or `.rk-shell` to override.

For a fully custom titlebar, pass `titlebar={<YourBar />}` (or `titlebar={false}`) and keep `AppFrame`, `Plane` and `NavList`. Use `Titlebar` with `slots` when only classes need to change. `titlebarStart` and `titlebarEnd` take the app's own controls, and `wordmark` takes the brand lockup.

#### Render your existing titlebar

Use this to match an app's titlebar pixel for pixel. `AppShell` builds its own titlebar, so pass a `Titlebar` through its `titlebar` prop.

```tsx
import "@rightkit/react-shell/shell.css";
import { AppFrame, Plane, Titlebar, NavList } from "@rightkit/react-shell/react";

// 1. Copy the measurements from the old titlebar onto :root (or .rk-shell).
//    Keep the old values for anything you do not change.
//    :root { --rk-titlebar-height: 44px; --rk-top-bg: #1b1b1f; --rk-top-border: 1px solid #2a2a2f;
//            --rk-caption-hover-bg: #2a2a2f; --rk-caption-margin-start: 0px; --rk-top-end-gap: 6px; }

// 2. Pass the old icons as caption glyphs, and tell Titlebar when the window is maximized.
<AppFrame>
  <Titlebar
    platform="windows"
    windowControls={windowAdapter}
    maximized={isMaximized}
    captionGlyphs={{ minimize: <MinIcon />, maximize: <MaxIcon />, restore: <RestoreIcon />, close: <CloseIcon /> }}
    start={<SidebarToggle />}
    title="Library"
    end={<StatusPill />}
  />
  <Plane>{children}</Plane>
</AppFrame>
```

Check the result against the old app in light and dark. Each value that differs from the default should appear in the token table above.

### Known gaps for the recipe

- `AppShell`'s built-in titlebar does not take `captionGlyphs` or `maximized`. Use the `titlebar` prop with a `Titlebar` for those.
- Tokens size the bar. They do not move the titlebar's title, the sidebar's nav, or the content plane. Use `slots` or a custom titlebar for those.

### Headless toggle and segmented control

`@rightkit/react-shell/headless` gives the behaviour of `Toggle` and `SegmentedControl` for a host whose markup, classes and styles are fixed. The hooks render nothing and return props for an element you render.

```tsx
import { useToggle, useSegmentedControl } from "@rightkit/react-shell/headless";

function Wifi({ on, setOn }: { on: boolean; setOn: (v: boolean) => void }) {
  const p = useToggle({ checked: on, onChange: setOn, label: "Wi-Fi" });
  return <span className="my-switch" {...p} />; // role="switch", aria-checked, aria-disabled, tabIndex, onClick, onKeyDown
}

function Mode({ value, setValue }: { value: "a" | "b" | "c"; setValue: (v: "a" | "b" | "c") => void }) {
  const { groupProps, getOptionProps } = useSegmentedControl({ value, onChange: setValue, label: "Mode", options: [{ value: "a" }, { value: "b" }, { value: "c", disabled: true }] });
  return (
    <div className="my-seg" {...groupProps}>
      <button type="button" className="my-seg__item" {...getOptionProps("a")}>A</button>
      <button type="button" className="my-seg__item" {...getOptionProps("b")}>B</button>
      <button type="button" className="my-seg__item" {...getOptionProps("c")}>C</button>
    </div>
  );
}
```

- `useToggle({ checked, onChange, label, disabled })`: Space and Enter flip the value and prevent the default. Disabled means no change, `tabIndex` -1 and `aria-disabled`. On a native `<button>` the keys are handled by the hook, so do not add a second key handler.
- `useSegmentedControl({ value, options, onChange, label?, disabled?, mode? })`. `mode: "radio"` (the default) gives `role="radiogroup"` and a roving tab stop on the selected option; Arrow keys, Home and End select and move focus, wrapping and skipping disabled options. Spread the `ref` from `getOptionProps` so focus can move; without it keys still select but focus stays put. `mode: "toggle"` gives a group of buttons with `aria-pressed`, each enabled button a tab stop; put it in a `<fieldset>` with a legend.
- Per-option `disabled` works in both modes. The styled `Toggle` and `SegmentedControl` stay as they are for hosts that take the shell's look.

### Sizing and class hooks on Toggle and SegmentedControl

Both accept `className`, added after the built-in class. Their sizes are custom properties, each defaulting to today's value:

| Property | Default | Applies to |
|---|---|---|
| `--rk-toggle-width` | `36px` | track width |
| `--rk-toggle-height` | `22px` | track height |
| `--rk-toggle-knob` | `18px` | knob width and height |
| `--rk-toggle-travel` | `14px` | knob offset when on |
| `--rk-seg-gap` | `2px` | gap between segments |

### Reduced motion

The shell stops the animations and transitions on `.rk-modal`, `.rk-palette`, `.rk-menu`, `.rk-toast` and `.rk-overlay` while the operating system asks for `prefers-reduced-motion: reduce`. That is the default and it does not change.

A host that decides, deliberately, to keep full motion sets `data-rk-motion="full"` on the root element (or on any ancestor of the shell, or on a single element) instead of overriding the selectors:

```html
<html data-rk-motion="full">
```

Opting out removes an accessibility accommodation for people who asked their system for less motion. It is the host's decision to make, not a package default, and only the exact value `full` opts out. The selectors use `:where()`, so their specificity is what it was before the attribute existed. `@rightkit/platform-ui` ships no stylesheet and no animation, so this attribute is the only motion switch in these two packages. Pinned by `e2e/motion.e2e.test.ts` (real Chrome with the media feature emulated) and `src/css.test.ts`.

### Palette and toast tokens

`--rk-palette-width` (640px), `--rk-toast-offset` (16px) and `--rk-plane-radius` (20px) size the palette, the toast stack and the content corner. `--rk-toast-width` (default `26rem`) sets the toast stack width; it is still capped at `100vw - 2rem`. `--rk-toast-translate` (default `-50% 0`) is the stack's horizontal centring, applied with the individual `translate` property (not `transform`), so a host `-translate-x-1/2` utility does not stack with it; set `--rk-toast-translate: 0 0` or `none` to take over the positioning (0.6.1). `--rk-menu-exit-ms` is set by `ContextMenu` itself while `exitMs` is above 0.

## Adopting with a locked UI

0.6.0 lets an app that already has a finished design (its own menu, modal, palette and toast look) use the shell's behaviour, focus handling and accessibility without inheriting its look. Every option below is optional and its default reproduces 0.5.1 exactly (DOM, attributes, classes and timing), pinned by snapshot tests.

The options below are for apps that take the shell's components with small changes. An app whose markup is fixed should not keep adding flags here: keep your own markup and take only the behaviour from `@rightkit/platform-ui/headless` (`useFocusTrap`, `useDismiss`, `useExitTransition`, `useListNavigation`, `fitToViewport`, `rankCommands`), documented with worked menu, modal and palette examples in that package's README.

### 1. Put the stylesheet in a cascade layer

`shell.css` is unlayered. Unlayered rules beat every layered rule, so Tailwind utilities (which sit in `@layer utilities`) cannot override a shell rule that targets the same element, and `:where(.rk-overlay, .rk-menu, .rk-toasts) button { font: inherit; color: inherit }` overrides a host's button typography. Import `shell.layered.css` instead and declare the layer order first. With Tailwind 4:

```css
@layer theme, base, rk, components, utilities;
@import "tailwindcss/theme.css" layer(theme);
@import "tailwindcss/preflight.css" layer(base);
@import "@rightkit/react-shell/shell.layered.css";
@import "tailwindcss/utilities.css" layer(utilities);
```

Or, with the all-in-one entry, put the order line first and keep the rest:

```css
@layer theme, base, rk, components, utilities;
@import "tailwindcss";
@import "@rightkit/react-shell/shell.layered.css";
```

Rules:

- The `@layer ... rk ...` order line must come before the first import of any of those layers. Without it `rk` is created where it is first seen: after `utilities` if the shell is imported after Tailwind (so the shell beats utilities again), or before `base` if imported first (so preflight's button reset beats the shell).
- Unlayered host CSS always beats `rk`. A host reset like `button { background: none }` outside any layer now overrides shell buttons; keep resets in `base`.
- Define brand tokens (`--rk-brand`, `--rk-brand-dark`, `--rk-wordmark-font`) in unlayered CSS or in a layer after `rk` (for example `components`). A `:root` rule inside `base` loses to the layered defaults.
- Apps that keep importing `shell.css` change nothing.

To take structure only, import `shell.structure.css` (optionally `layer(rk)`), skip the visual file and style the `rk-*` classes yourself. Structure holds position, z-index, display/flex/grid, overflow, pointer-events, the sidebar/titlebar box sizes and the layout variables. Since 0.6.1 it carries only rules the frame needs to function: `.rk-modal { overflow: auto }`, `.rk-palette { overflow: hidden }`, `.rk-btn { display: inline-flex; align-items: center; justify-content: center }` and `.rk-btn:disabled { pointer-events: none }` restyle a surface or control the host usually owns and sit in `shell.visual.css`. `shell.css` renders the same computed styles as 0.6.0 except the toast stack's centring (`transform: translateX(-50%)` became `translate: -50% 0`; same position). Colour variables, radii, type, spacing, shadows and animation are in `shell.visual.css`. `shell.css` is generated from the two (`pnpm run build:css`; `pnpm run check:css` fails when it is stale).

### 2. Options

| Component | Option | Default | Effect |
|---|---|---|---|
| `ContextMenu` | `itemAs` | `"div"` | `"button"` renders `button[type=button][role=menuitem\|menuitemcheckbox]` with a native `disabled` (no `aria-disabled`). Rows stay out of the Tab order; the menu keeps `aria-activedescendant`. |
| | `renderItem(item, {index, active})` | none | Replaces row content (lead, label, shortcut). Role, id and state classes stay. |
| | `showLead` | `true` | `false` drops the lead column (`span.rk-menu__lead`). |
| | `scrimClassName`, `slots.scrim` | none | Class on the click-away scrim. |
| | `autoFocus` | `true` | `false` leaves focus and the document selection where they are. Arrows, Home, End, Enter, Space, Escape and Tab are read from the window while the menu is open; chords with Ctrl, Cmd or Alt pass through. |
| | `keyboard` | `"menu"` | `"escape-only"`: the component handles only Escape (close, focus restored, IME-guarded), sets no highlight and captures no other key; Tab, arrows, Space and Enter stay native. It relies on native button semantics, so it requires `itemAs="button"` (dev-mode warning otherwise); item buttons stay in the Tab order. |
| | `initialHighlight` | `"first"` | `"none"` opens with no row highlighted; ArrowDown goes to the first row, ArrowUp to the last. |
| | `exitMs`, `exitClassName`, `onExitStart` | `0`, `"is-closing"`, none | With `exitMs > 0` closing adds the class to the menu and scrim, sets `--rk-menu-exit-ms`, calls `onExitStart`, and calls `onClose` after `exitMs`. The chosen item's `run` fires immediately. Input is ignored while exiting. Passing a new `x`, `y` or `items` array while exiting counts as a new open and cancels the exit: memoise `items`, or use `useContextMenu`, which remounts the menu per open. |
| `useContextMenu` | third argument | none | Any of the above options for the menu it renders. |
| `ModalShell`, `Dialog` | `overlayClassName`, `overlayStyle`, `slots.overlay` | none | Class and inline style on the backdrop. |
| | `dismissOn` | `"mousedown"` | `"click"` closes on click and only when both the press and the release landed on the backdrop itself; a drag that crosses between the dialog and the backdrop in either direction never closes. |
| | `ariaLabel` | none | Sets `aria-label` and removes `aria-labelledby`, even when a title is rendered. |
| `ActionDialog` (new) | `actions: DialogAction[]` | required | Any number of footer buttons: `label`, `onClick`, `variant`, `className`, `autoFocus`, `disabled`, `id`. |
| | `role` | `"dialog"` | `"alertdialog"` for interruptions. |
| `ConfirmDialog` | `role` | `alertdialog` if `danger`, else `dialog` | Explicit role. Now built on `ActionDialog`; output unchanged. |
| `CommandPalette` | `labels.inputAriaLabel` | same string as `labels.placeholder` | Accessible name of the input, independent of the placeholder. |
| | `enterDelayMs` | `90` | Delay between choosing a command and running it. |
| | `rank(commands, query, limit)` | `rankCommands` | Replaces ranking and filtering. Results are still grouped by `command.group`, first-seen order. `rankCommands` is exported. |
| | `searchIcon` | magnifier | Node replacing the magnifier; `null` for none. |
| | `renderIcon(command, {active})` | `command.icon` | Content of the row icon box; `null` for no box. `slots.icon` styles it. |
| `ToastViewport` | `renderGlyph(toast)` | built-in glyph span | Replaces the glyph element (`null` for none). |
| | `renderRow({toast, leaving, dismiss, role, className})` | none | Replaces the whole row. Put `role` and `className` on your element. |
| | `rowClassName` | none | String, or `(toast, {leaving}) => string`. |
| | `exitClassName`, `exitMs` | none, `150` | Class added next to `is-leaving`; exit length before removal. |
| | `roles` | error `alert`, others `status` | Per kind; `null` omits the role. |
| | `--rk-toast-width` | `26rem` | CSS variable for the stack width. |

**Page-wide key capture with `autoFocus: false` (default `keyboard`).** While such a menu is open, plain Space, Enter, ArrowUp/Down, Home, End and Tab, and Shift+arrows, are captured from the window and prevented even when a page input or button has focus (Ctrl/Cmd/Alt chords and IME composition pass through). Use `keyboard="escape-only"` with `itemAs="button"` when the page must keep those keys.

Keyboard and focus: every option combination keeps Escape, the focus trap and focus return (modals), arrow/Home/End/Enter navigation (menu, palette) and `aria-activedescendant`. `e2e/options.e2e.test.ts` drives these with real key events.

### 3. Checking a layered adoption

Compare the app before and after in light and dark. Typical differences after moving to `shell.layered.css`: any host rule outside a layer that targets `.rk-*` elements now wins (it used to win or lose by specificity and order), and any `base`-layer reset now loses to the shell (it always lost to unlayered `shell.css`).

## Known issues

- The default titlebar draws its own nav and caption layout. An app with a very different titlebar should use `titlebar={...}` rather than tuning tokens past their useful range.

- Peer React is `<21`, the same range as `doc-render`, `i18n`, `media-ui` and `pdf-ui` (`package.json:46`).
