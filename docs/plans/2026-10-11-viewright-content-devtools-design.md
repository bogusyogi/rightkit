# ViewRight content DevTools: migration design

Status: design for the ViewRight chat. Nothing here was compiled or run. Every Tauri/wry fact is cited to the source in the local cargo registry (tauri 2.11.5, wry 0.55.1, tauri-runtime-wry 2.11.4); anything from memory is marked **unverified**.

## The rule

Owner's fixed design, never to be changed: every Right app has absolutely NO access to DevTools: blocked at each and every level that is possible, for the app, its shell and its own UI. The single exception is ViewRight: it opens HTML documents, and DevTools must be available INSIDE that opened HTML content only (the content frame), never for ViewRight's own UI or shell.

## Why ViewRight does not meet it today

All three HTML preview kinds are iframes inside the main webview: interactive (`HtmlDocumentView.tsx:47-54`, `sandbox="allow-scripts"` + `srcDoc`), safe/sanitized (`HtmlViewer.tsx:760-764`, `sandbox="allow-same-origin"`), live URL (`LivePreviewPane.tsx:64-68`). `open_devtools` (`src-tauri/src/commands/external.rs:203-223`) opens the inspector of the whole main webview, so the shell DOM, Console and Sources are reachable. On macOS `developerExtrasEnabled` is toggled true while an iframe is hovered or focused (`external.rs:188-195`), which re-arms WebKit's Cmd+Opt+I for the whole webview; `isInspectable` is never set. On Windows `AreDevToolsEnabled` is never disabled and the additional-browser-arguments environment variable is not neutralised.

An inspector attaches to a webview, not to a frame. WKWebView inspection (`isInspectable`, the private `_inspector`) and WebView2 (`OpenDevToolsWindow` on a `CoreWebView2`) both target the whole page, and Chromium DevTools can switch the JavaScript context to the parent frame. So no setting makes "iframe only" inspection possible; the rule can only be met by moving the content into webviews of their own. The shell enforces exactly that: `rightkit-shell` `DevToolsPolicy::ContentOnly` never makes the main webview inspectable and has no iframe-level allowance.

## Target architecture

One child webview per open HTML preview, hosted in the main window, no IPC, positioned over a placeholder element of the React UI.

```
window "main"
 |- webview "main"                  app UI. Never inspectable. Capability: "webviews": ["main"]
 |- webview "html-content-<tabId>"  one per HTML tab. Inspectable (ContentOnly). No capability at all.
```

### What Tauri 2 offers (verified)

| Need | API | Status |
|---|---|---|
| Create a webview inside an existing window | `Window::add_child(WebviewBuilder, position, size)` (`tauri-2.11.5/src/window/mod.rs:1126-1129`) | **Unstable**: `#[cfg(any(test, all(desktop, feature = "unstable")))]`. Needs the `tauri` crate feature `unstable` (`Cargo.toml:130`, forwards `tauri-runtime-wry?/unstable`). Desktop only. |
| The builder | `Webview::builder(label, WebviewUrl)` returning `WebviewBuilder` (`src/webview/mod.rs:1368-1370`) | **Unstable** (`#[cfg(feature = "unstable")]`). |
| Move and size | `Webview::set_position`, `set_size`, `set_bounds` (`src/webview/mod.rs:1509-1527`) | Compiled; reaching a child webview needs `add_child`, so effectively unstable. |
| Run script in it | `Webview::eval` (`src/webview/mod.rs:1917`) | stable |
| Page-load hook, init script | `WebviewBuilder::on_page_load`, `initialization_script` (`:688`, `:868`) | stable on the builder |
| Per-webview devtools switch | `WebviewBuilder::devtools(bool)` (`:1118`) and `Webview::open_devtools` (needs `devtools` feature or a debug build) | stable; the shell sets the real value natively anyway |
| Per-webview capability | capability `webviews: [..]` (matched at `src/ipc/authority.rs:459`) | stable |
| Which webview made a custom-scheme request | `UriSchemeContext::webview_label()` (`src/app.rs:2481`) | stable |

Consequence: the real fix needs `tauri/unstable` in ViewRight. That is the only unstable surface. A stable-API alternative exists (each preview as a borderless `WebviewWindow` with `parent()` set, kept aligned with the placeholder by hand), but it loses clipping with the main window, Spaces/Mission Control behaviour and focus handling; **unverified** how tao positions such a child on both platforms. Recommendation: `unstable` + `add_child`, pinned to the Tauri version ViewRight already locks (2.11.5), and re-qualified at every Tauri bump.

### Is inspectability per webview? (confirmed)

- macOS: `isInspectable` is a property of one `WKWebView` (wry sets it per webview, `wry-0.55.1/src/wkwebview/mod.rs:537-545`, together with the per-webview `WKPreferences` key `developerExtrasEnabled`, `:546-549`). Each child webview is its own `WKWebView` with its own configuration, so the shell webview can stay `isInspectable = false` while a content webview's inspector is open. (`isInspectable` exists from macOS 13.3; wry guards with `respondsToSelector`.) **Unverified**: that a `WKWebViewConfiguration`'s `preferences` object is not shared between sibling webviews; wry creates one configuration per webview, so it should not be.
- Windows: DevTools are a setting of the `ICoreWebView2Settings` object of one `CoreWebView2` (`SetAreDevToolsEnabled`, wry `src/webview2/mod.rs:573`, called for each created webview). Child webviews are separate controllers/`CoreWebView2`s in one shared environment and user-data folder, so the shell webview keeps `AreDevToolsEnabled = false` while a content webview has it true. The environment-wide parts (browser arguments, remote-debugging port) are per browser process and therefore not scopable per webview: they are blocked for everything by `guard_process_start` (see the rightkit-shell README).

## IPC isolation (the part that is easy to get wrong)

A content webview must not be able to call the app. Two verified Tauri behaviours decide how:

1. A capability's `windows` field matches the window that hosts the webview, not the webview (`authority.rs:459-460`: `webviews.matches(webview) || windows.matches(window)`). ViewRight's `capabilities/default.json` has `"windows": ["main"]`; every child webview added to window `main` would inherit all of it. **Change it to `"webviews": ["main"]`** (and the same for any other capability that must not reach content).
2. App-defined commands are ACL-checked only when the app declares an ACL manifest, or the plugin command, or the origin is remote (`src/webview/mod.rs:1819-1823`). Without a manifest a local-origin page (a custom scheme such as `viewright-content://`) can invoke every app command. **Add an `AppManifest` listing ViewRight's commands in `build.rs`** (`tauri_build::Attributes::new().app_manifest(AppManifest::new().commands(&[...]))`) and grant them only to webview `main` through the capability. Plugin commands (`core:*`, `dialog`, ...) are always ACL-checked, so a content webview with no capability gets none of them.

`rightkit-shell` enforces this at startup: `ContentWebview::new("html-content-*", "main").capability(<each capability file>).app_acl_manifest(true)` fails with `ContentHasIpc` if any supplied capability could apply to the content webview (by webview-pattern overlap or by parent window label), and `right-release` refuses a build whose `build.rs` has no `AppManifest ... .commands(`. The shell cannot see capabilities at run time, so the app must pass every capability document it ships (`include_str!`).

## The three preview kinds

| Today | New webview | URL and origin |
|---|---|---|
| Interactive: `<iframe sandbox="allow-scripts" srcdoc>` | `html-content-<tab>` | `WebviewUrl::CustomProtocol("viewright-content://<token>/index.html")`, served by a Rust scheme handler from bytes the path broker granted. The handler sets `Content-Security-Policy: sandbox allow-scripts` so the document gets the iframe's opaque origin (**unverified** in WKWebView and WebView2 for a top-level document) and a per-document token host, so documents never share an origin. Bytes come from the in-memory editor buffer, as `srcdoc` does today. |
| Safe/sanitized: `sandbox="allow-same-origin"` | `html-content-<tab>` | Same scheme; sanitised HTML; CSP `sandbox allow-same-origin; script-src 'none'`. |
| Live URL: `sandbox="allow-scripts allow-same-origin allow-forms allow-popups"` | `html-content-<tab>` | `WebviewUrl::External(url)`. A remote origin is always ACL-checked (`mod.rs:1823`), and no capability matches it, so it has no IPC. New-window requests keep going to ViewRight's existing link policy (`route_new_windows_to_tabs`) through the shell `NavPolicy`. |

Site documents (`viewright-site://`, `site_root.rs`) keep their scheme; the content webview loads it like the iframe did.

## Replacing the postMessage bridge

An iframe talks to its parent with `postMessage`. Separate webviews have no such link, and the content webview must not get IPC. The mediated channel uses no Tauri command at all:

- **Content to app.** An initialization script (`WebviewBuilder::initialization_script`, injected before page scripts) defines `window.__vrBridge.post(kind, payload)` which does a `fetch`/`sendBeacon` POST to a second custom scheme, `viewright-bridge://<token>/m`. The handler is registered with `register_asynchronous_uri_scheme_protocol`; it reads `ctx.webview_label()` (`app.rs:2481`) to learn which webview sent it and ignores the claimed identity in the body. It accepts only a fixed list of message kinds (key relay for app shortcuts, link click, source-jump target, find results, title/size), caps body size and rate per webview, and forwards a validated value to the main webview with `app.emit_to("main", "viewright://content", {label, kind, payload})`. All content input is untrusted: a hostile page can call the bridge too, so nothing it sends may be an instruction. **Unverified**: cross-origin requests from `viewright-content://` (a `http://*.localhost` origin on Windows) to the bridge scheme need CORS response headers; if that proves fragile, serve the bridge from the same scheme under a reserved path.
- **App to content.** `Webview::eval("window.__vrHost.receive(<json>)")` from Rust, called by a main-webview command that validates the label is a content webview. No event-system access (`core:event` would be IPC) is needed.
- **Features that used `contentDocument`** from the parent must move into the content side: find-in-page (driven by `eval`, results back over the bridge), print (`Webview::print` on the content webview from Rust; the current `core:webview:allow-print` stays on `main` only), source-jump (script captures the click target, posts it), focus and keyboard relay (Cmd+F, Cmd+W, tab switching).

## Layout and lifecycle costs

- The placeholder `<div>` reports its rectangle (ResizeObserver + scroll/transform changes) to a Rust command, which calls `Webview::set_bounds` in logical pixels. Native child webviews sit above DOM content, so React overlays (context menus, dialogs, toasts, drag ghosts, the command palette) are covered by it: while any overlay is open the content webview must be hidden or shrunk. **Unverified**: that `Webview::hide/show` is available for child webviews in 2.11.5; shrinking to zero size is the fallback.
- Per-tab webviews cost memory. Keep at most the active tab's content webview alive and recreate others on activation, or use a small pool; this replaces the `visibility:hidden` trick in `DocumentRouterTable.tsx` (iframes were kept alive that way to avoid reload).
- Drag and drop, text selection hand-off, and macOS traffic-light/vibrancy composition around child webviews need a visual check on both platforms.
- Windows: all webviews share one environment and one browser process; a content page that crashes the renderer takes only its own renderer, but `ProcessFailed` handling for child webviews should be added to the hang watchdog.

## Removing ViewRight's current DevTools code

Delete, do not adapt: `open_devtools`, `set_devtools_shortcut_scope`, `disable_automatic_devtools_shortcuts`, the macOS `_inspector` guard and `configure_macos_inspector` (`commands/external.rs`), the `dev-tools` feature (`Cargo.toml:36`; the owner's rule has no dev-build exception, and the release gate refuses the feature name), `install_view_source_context_item`'s dependence on the native Inspect item (the shell now removes Inspect from every Denied menu and keeps only Inspect in content menus), and the frontend F12 / Cmd+Opt+I handlers (`useGlobalShortcuts.ts:146-150`). Replace with one command that calls `rightkit_shell::devtools::open_content_devtools(&app, &label)`; the shell refuses anything but a registered content webview.

Cargo and config changes:

```toml
tauri = { version = "2.11.5", features = ["unstable"] }
rightkit-shell = { version = "...", features = ["content-devtools"] }
```

```rust
Shell::builder("viewright")
    .devtools_policy(DevToolsPolicy::ContentOnly)
    .content_webview(
        ContentWebview::new("html-content-*", "main")
            .capability(include_str!("../capabilities/default.json"))
            .capability(include_str!("../capabilities/print-preview.json"))
            .app_acl_manifest(true),
    )
```

and `devtools: "content-only"` in ViewRight's right-release config. The `tauri` crate does not need the `devtools` feature in ViewRight's manifest; `content-devtools` switches it on.

## What the content inspector can and cannot reach

Can: the content document, its scripts, console, network for its own requests, storage for its origin. Cannot: the app UI (a different webview with the inspector disabled), app IPC (no capability, ACL manifest), other documents (per-document origin). Residual, **unverified**: a user who can open DevTools for one WebView2 webview shares a browser process and user-data folder with the shell webview; without a remote-debugging port (blocked by the guard) there is no target list to reach it.

## Order of work for the ViewRight chat

1. Capabilities to `webviews`, `AppManifest`, `tauri/unstable` (behind a spike: one interactive preview in a child webview, both platforms).
2. Content scheme handlers, bridge scheme, init script; port find/print/source-jump/keyboard relay.
3. Placeholder layout and overlay handling; per-tab lifecycle.
4. Remove the old DevTools code; adopt `ContentOnly`; run the rightkit-shell tests and a real-app check that F12, Ctrl+Shift+I/J/C, Cmd+Opt+I/J/C/U and the context menu give no inspector in the app UI and an inspector only in HTML content.
5. `right-release` config `devtools: "content-only"`; the release scan then accepts the devtools code only with the shell's marker.
