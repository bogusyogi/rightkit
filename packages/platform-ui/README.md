# @rightkit/platform-ui

Brand-neutral platform primitives for Right Suite desktop apps: Tauri-backed platform detection, display-only shortcut formatting, keyboard-chord rendering, platform capability flags, window-control labels and headless window-control adapters. Title bars, colours and navigation stay in each app.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React and plain TypeScript) |
| Rust pair | None |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/platform-ui` | `getPlatform`, `normalizePlatform`, `formatChord`, `platformCapabilities`, `createWindowControls`, `createFullscreenControls`, `createWindowControlLabels`, `DEFAULT_WINDOW_CONTROL_LABELS`, `WINDOW_CONTROL_LABELS`, `bootstrapQaPlatform`, `resolveQaPlatform`; types `Platform`, `WindowControlAdapter`, `FullscreenControlAdapter`, `WindowControlLabels`, `QaPlatformBootstrapInput` |
| `@rightkit/platform-ui/react` | `usePlatform`, `Kbd`, `KbdProps` |
| `@rightkit/platform-ui/headless` | No native detection and no `@tauri-apps/plugin-os` import, so it works in any app. `formatChord` (explicit platform, default `unknown`), `normalizePlatform`, `floatingPanelStyle`, `fitToViewport`, `createToastBus`, `defaultToastBus`, `toast`, `useFocusTrap`, `useModalDismiss`, `useLatest`, `useDismiss`, `useExitTransition`, `useListNavigation`, `rankCommands`, `rankItems`, `matchScore`, and the framework-free cores `attachFocusTrap`, `attachDismiss`, `createExitTransition`. See [Headless behaviour hooks](#headless-behaviour-hooks). |

## Peer dependencies

| Peer | Range |
|---|---|
| `@tauri-apps/plugin-os` | `>=2 <3`, optional (`peerDependenciesMeta`). Loaded lazily; the browser's platform report is used when it is absent. |
| `react` | `>=18 <21` |

## Example

Illustrative. `nativeWindowAdapter` is the app's own Tauri window adapter.

```ts
import { createWindowControls, formatChord, getPlatform } from "@rightkit/platform-ui";
import { Kbd, usePlatform } from "@rightkit/platform-ui/react";

const platform = getPlatform();
const keys = formatChord("mod+shift+p", platform); // display tokens only
const controls = createWindowControls(nativeWindowAdapter); // your Tauri window adapter
```

`formatChord` only returns display tokens. The app owns the stored shortcut string. Fullscreen is a separate adapter, so an app can implement only minimize, maximize and close.

## Native platform loading

`getPlatform()` is synchronous. It returns the QA override if set, then the native platform once it has loaded, and before that the browser's report. Every supported desktop webview reports the right OS there, so the first render is correct. `loadPlatform()` loads `@tauri-apps/plugin-os` with a dynamic import and resolves the native value, which is cached. `usePlatform()` calls it for you.

Bundling note: the root and `./react` entries contain a dynamic `import("@tauri-apps/plugin-os")`. Apps that bundle those entries must install the plugin, which every Tauri app already does. An app without the plugin should import `./headless` only.

## Headless behaviour hooks

For an app whose markup and look are fixed (ViewRight): keep your JSX and classes, delete your local focus, dismiss, exit and list logic, and take that behaviour from `@rightkit/platform-ui/headless`. The hooks return state and props; they render nothing, import no styles, create no portals and impose no element types or ids. Each default reproduces the previous behaviour, so adopting a hook with no options changes nothing that 0.2.1 did.

| Hook / function | Options (default) | Returns |
|---|---|---|
| `useFocusTrap(ref, active = true, opts)` | `autoFocus` (true), `trap` (true), `restore` (true), `restoreTiming` (`"microtask"` \| `"sync"` \| ms), `restoreWhen` (`"inactive"` \| `"always"`), `captureOpener` (`"render"` \| `"effect"`), `fromOutside` (`"both"` \| `"backward"` \| `"none"`), `skipHidden` (true), `emptyList` (`"prevent"` \| `"pass"`), `focusFallback` (`"container"` \| `"none"`) | nothing |
| `useModalDismiss(onClose, opts)` | `escape` (false), `exitMs` (80), `preventDefault` (true), `stopPropagation` (true) | `{ closing, close }` |
| `useDismiss(onDismiss, opts)` | `enabled` (true), `escape` (true), `escapeTarget` (`"window"` capture, or a ref: bubble on that element), `preventDefault` (true), `stopPropagation` (true), `imeGuard` (true), `outside` (`"mousedown"` \| `"click"` \| `"click-target"` \| `false`; default `"mousedown"`), `container`, `backdrop`, `ignore`, `outsideContextMenu` (false), `onWindowBlur` (false), `onResize` (false), `onScroll` (false) | nothing; calls `onDismiss(reason, event)`, reason is `escape` \| `outside` \| `blur` \| `resize` \| `scroll` |
| `useExitTransition(opts)` | `exitMs` (80), `closeAfter` (`"immediately"` \| `"settled"`), `onActionWhileClosing` (`"drop"` \| `"run"`), `initialOpen` (true), `onExitStart`, `onClosed` | `{ phase: "open" \| "closing" \| "closed", closing, isOpen, pending, open(), close(action?) }` |
| `useListNavigation(opts)` | `count`, `initialIndex` (0), `wrap` (true), `homeEnd` (true), `keepIndexOnItemsChange` (false), `resetKey`, `hoverSelects` (`"always"` \| `"after-pointer-move"`), `typeahead` (off), `isDisabled`, `onActivate`, `activateKeys` (`["Enter"]`), `idPrefix`, `listId`, `optionId`, `role` (`"listbox"` \| `"menu"`), `focusOwner` (`"input"` \| `"list"`), `scrollIntoView` (false), `scrollOn` (`"active"` \| `"active-or-length"`), `imeGuard` (true), `emptyIndex` (`"clamp"` \| `"preserve"`) | `{ activeIndex, rawIndex, activeId, setActiveIndex, getInputProps(), getListProps(), getOptionProps(i) }` |
| `fitToViewport({ x, y, size \| estimatedSize, viewport, margin = 8 })` | pure; `size` (measured) wins over `estimatedSize` | `{ left, top, width, height, maxWidth, maxHeight }` |
| `rankCommands(items, query, limit = 50)` | pure; also `rankItems`, `matchScore` | ranked copy; empty query keeps input order |

Notes that matter when you configure them:

- **Focus trap.** `captureOpener: "render"` reads the opener during the first render, before React's own `autoFocus` attribute moves focus at commit; `"effect"` reads it when the trap activates. By default (`restoreWhen: "inactive"`) a cleanup restores only when the container is gone or `active` is false, whatever the timing, so StrictMode's simulated cleanup leaves focus alone; `"always"` restores on every cleanup (use it with `restoreTiming: "sync"`; with a microtask the restore would land after StrictMode's re-run and take focus from the first control). With `captureOpener: "effect"` the opener is recorded once per open: never from a node inside the container, never over a value already recorded, and cleared when the trap really closes (0.2.3; before, StrictMode's second effect run recorded the trap's own first control). A React `autoFocus` attribute inside the container has already moved focus when effects run, so use `"render"` with it.
- **`useDismiss` outside press.** It needs `container` (the panel) or `backdrop`. With `backdrop`, only a target that is the backdrop element counts as outside, like `e.target === e.currentTarget`; otherwise outside means not inside `container` and not inside an `ignore` element. `"click"` needs both the press and the release outside (a drag that starts or ends inside never dismisses); `"click-target"` dismisses on any click whose target is outside, which for a panel nested in its backdrop includes a drag from the panel to the backdrop, and for a sibling backdrop does not (the click lands on the common ancestor), exactly as a plain `onClick` on the backdrop would. Listeners are removed when `enabled` turns false and on unmount; pass `enabled: exit.isOpen` so a closing layer stops listening.
- **`useExitTransition`.** `close(action)` with `closeAfter: "immediately"` starts the exit, then calls `action` synchronously. With `"settled"` it calls `action` first, keeps `phase: "open"` with `pending: true` until the returned value settles (fulfilled or rejected), then starts the exit. `open()` while closing starts a fresh instance; the old timer and any unsettled action cannot close it. A `close(action)` while closing or pending drops its action (`onActionWhileClosing: "drop"`, default). `"run"` still runs it, so a double Enter or click executes the action twice; it exists only for parity with wrappers that always call their action. `close` returns a promise that rejects with the action's error, after the close has still happened. Timers are cleared on unmount.
- **`useListNavigation`.** The index is clamped on read, so a shrinking list never leaves a dangling `aria-activedescendant`; an empty list gives `-1`. `keepIndexOnItemsChange: true` keeps the stored index when `count` changes (it comes back if the list grows again); `resetKey` (the query) resets it regardless. Keys during an IME composition are ignored unless `imeGuard: false`. With `emptyIndex: "preserve"` the stored -1 that ArrowDown leaves on an empty list survives (`rawIndex`), so results arriving later select nothing until a key moves; the default clamps it to the first item. `scrollOn: "active-or-length"` also scrolls to the active option when the list length changes. `hoverSelects: "after-pointer-move"` never lets a resting pointer take the selection back from the keyboard.
- **`fitToViewport`** is the same clamp as `floatingPanelStyle` and equals it for `{ width, height: estimatedHeight }`. Measuring (`getBoundingClientRect` in a layout effect, then calling it again with `size`) stays in your component.

### Menu (replaces a local scrim + Escape listener + exit timer)

ViewRight's `DocumentContextMenu`: a full-screen scrim closes on mousedown and on a context-menu gesture, a window-capture Escape listener calls only `onClose`, `floatingPanelStyle` with a fixed 176 x 260 estimate. The sidebar and explorer menus close with `click`, run each item then close after it settles.

```tsx
const panel = useRef<HTMLDivElement>(null);
const scrim = useRef<HTMLDivElement>(null);
const exit = useExitTransition({ exitMs: 80, closeAfter: "settled", onActionWhileClosing: "run", onClosed: onClose }); // document menu: exitMs 0 (it unmounts at once)
useDismiss(() => void exit.close(), {
  enabled: exit.isOpen, container: panel, backdrop: scrim,
  outside: "mousedown",            // sidebar menus: "click-target"
  outsideContextMenu: true,        // scrim onContextMenu -> preventDefault + close
  preventDefault: false, stopPropagation: false, imeGuard: false, // what the local Escape listener did
// and on the transition: onActionWhileClosing: "run" (ViewRight's wrapper runs the action on every click, even during the exit)
});
const pos = fitToViewport({ x, y, estimatedSize: { width: 176, height: 260 }, viewport: { width: innerWidth, height: innerHeight } });
// JSX unchanged: scrim div ref={scrim}, menu div ref={panel} style={pos} className={exit.closing ? "vr-pop-exit" : ""}
// and onMouseDown={(e) => e.preventDefault()} stays yours (it keeps the document selection alive).
// run = (fn) => () => void exit.close(fn).catch(() => {})    // replaces Promise.resolve(fn()).finally(close)
```

Deletes: the `useEffect` Escape listener, the scrim `onMouseDown` / `onContextMenu` handlers, the `useModalDismiss` call and `run` wrapper in the sidebar menus. Stays: portal choice, `tabIndex`, wrappers, `vr-pop*` classes, the mousedown `preventDefault`, the measured-height re-fit if you want one (call `fitToViewport` again with `size`).

### Modal (replaces local `useFocusTrap` + `useModalDismiss`)

ViewRight's `ModalShell`: Tab trap that restores to the element that was focused when the effect ran, synchronously; no wrap for a forward Tab from outside; a click on the overlay (not on the panel) closes; Escape on the window capture calls `stopPropagation` only.

```tsx
const overlay = useRef<HTMLDivElement>(null);
const panel = useRef<HTMLDivElement>(null);
const exit = useExitTransition({ exitMs: 80, onClosed: onClose });
useFocusTrap(panel, true, {
  autoFocus,                       // the shell's own prop
  captureOpener: "effect", restoreTiming: "sync", restoreWhen: "always", fromOutside: "backward",
  skipHidden: false, emptyList: "pass", focusFallback: "none",
});
useDismiss(() => void exit.close(), {
  enabled: exit.isOpen, container: panel, backdrop: overlay,
  outside: "click-target", preventDefault: false, imeGuard: false,
});
// JSX unchanged; className uses exit.closing in place of the local `closing`.
```

For `alertdialog` surfaces that must not dismiss on the backdrop, pass `outside: false`. `autoFocus: false` and `trap: false` cover dialogs that set their own initial focus or stay Tab-native.

### Command palette (replaces the local list state, clamping and exit)

ViewRight's `CommandPalette`: ArrowDown/Up clamp, no Home/End, a stored index that is only clamped on read, every mouse move selects, option ids `vr-palette-option-<i>`, the list id `vr-palette-results`, Enter runs the item and starts the exit in the same tick, Escape closes from anywhere inside the dialog.

```tsx
const exit = useExitTransition({ exitMs: 80, onActionWhileClosing: "run", onClosed: onClose });
const items = rankCommands(allItems, q, 50);          // or your own scorer
const nav = useListNavigation({
  count: items.length, wrap: false, homeEnd: false, keepIndexOnItemsChange: true,
  hoverSelects: "always", scrollIntoView: true, scrollOn: "active-or-length",
  imeGuard: false, emptyIndex: "preserve",
  idPrefix: "vr-palette", listId: "vr-palette-results", optionId: (i) => `vr-palette-option-${i}`,
  onActivate: (i) => void exit.close(items[i].run),
});
// ViewRight has no focus trap or restore on the palette, so there is no useFocusTrap call; an app that wants one adds
// useFocusTrap(dialog, true, { autoFocus: false }) and focuses its input itself.
useDismiss(() => void exit.close(), { enabled: exit.isOpen, escapeTarget: dialog, stopPropagation: false, container: dialog, backdrop: overlay, outside: "click-target" });
// <input value={q} onChange={...} aria-label="Search files and commands" {...nav.getInputProps()} />
// <div {...nav.getListProps()} aria-label="Results"> ... <button {...nav.getOptionProps(i)} onClick={() => exit.close(it.run)} /></div>
```

`rankCommands` scores substring, keyword, hint and subsequence matches (higher is better, stable, empty query keeps input order). It is not ViewRight's scorer (subsequence only, lower first): adopting it changes the order results appear in. Pass `onActivate` the `run` you want; grouping into sections stays in your JSX. An app that wants the shared look and resets on typing sets `wrap: true`, `homeEnd: true`, `resetKey: q`, `hoverSelects: "after-pointer-move"`.

## Known issues

- `bootstrapQaPlatform` and `resolveQaPlatform` are QA helpers exported from the runtime entry. Call them only in QA builds. Browser overrides need `qa=1` and `qaPlatform`, or `RIGHTKIT_QA=1` and `RIGHTKIT_QA_PLATFORM`. Production detection uses `@tauri-apps/plugin-os`.
- Peer React is `<21`, the same range as `doc-render`, `i18n`, `media-ui` and `pdf-ui` (`package.json:33`).
