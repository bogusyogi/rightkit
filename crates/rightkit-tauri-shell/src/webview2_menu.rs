//! WebView2 context menu: one handler per webview, composed in one place (Orthic-Labs/rightkit#386).
//!
//! Three layers want a say in the same `ContextMenuRequested` event: the DevTools policy (the
//! "Inspect" item), the hardening layer (no stray engine menu) and the app (its own items on the
//! frames it shows). WebView2 runs every registered handler, so three separate handlers would make
//! the result depend on registration order. This module is the single owner: each layer records a
//! stage for the webview (`chain`), and ONE native handler runs the stages in a fixed order:
//!
//! 1. **Inspect filter** (DevTools policy): a denied webview loses its "Inspect" items
//!    (`chain::InspectRule::Strip`); a content webview keeps only "Inspect"
//!    (`chain::InspectRule::OnlyInspect`); a developer webview is left alone.
//! 2. **App graft** ([`install_context_menu`]): on frames the app's classifier claims, drop the named
//!    engine items and empty submenus, then append the app's items.
//! 3. **Resolution** (`chain::resolve_handled`): decides `Handled` once. An app item that was
//!    added always wins (the menu must show); otherwise a content webview with nothing left shows no
//!    menu, and hardening hides the menu unless the target is an editable field it allows.
//!
//! The order does not depend on which layer installed first or on when the app calls
//! [`install_context_menu`]. Stage 1 and the resolution are the behaviour the DevTools and hardening
//! handlers had separately; stage 2 is new and does nothing without an app graft.
//!
//! The app owns everything product-specific: which frames are claimed ([`FrameKind`] via the
//! classifier), the item labels, which items show where, and what a pick does (a callback or an
//! event name, [`CustomItem::new`] / [`CustomItem::emit`]).
//!
//! Compiled on every platform (the planning is pure); the WebView2 calls are `#[cfg(windows)]` and
//! use the `windows` / `webview2-com` generation the `wv2-0_38` / `wv2-0_39` features select. Not run
//! against a live WebView2 by this change: right-click a link in a claimed frame, in an editable
//! field, and on an app frame to see the three paths.

use std::sync::Arc;

use serde::Serialize;
use tauri::{Runtime, WebviewWindow};

use crate::ShellError;

/// What kind of frame a context-menu request came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum FrameKind {
    /// The app's own editable preview of a local document (a custom-scheme origin or `about:srcdoc`).
    Editable,
    /// A served web page (http or https) shown by the app.
    Web,
    /// Anything else, including the app's own UI: the menu is left exactly as the shell made it.
    Other,
}

/// Engine item names that open a detached engine window; apps usually drop them from preview menus.
pub const LINK_NEW_WINDOW_ITEM_NAMES: [&str; 3] = [
    "openLinkInNewWindow",
    "openLinkInNewTab",
    "openLinkInNewBackgroundTab",
];

/// The frame URI WebView2 reports for an `srcdoc` iframe.
pub const SRCDOC_FRAME: &str = "about:srcdoc";

/// True for an `http://` or `https://` URL (after trimming).
pub fn is_http_url(url: &str) -> bool {
    let url = url.trim();
    url.starts_with("http://") || url.starts_with("https://")
}

/// Classify `frame_uri`. `editable_prefixes` are the URI prefixes of the app's own preview origins
/// (on WebView2 a custom scheme is an `http://<scheme>.localhost/` URL; elsewhere `<scheme>://`).
/// `about:srcdoc` and every prefix match are [`FrameKind::Editable`]. The editable test runs BEFORE
/// the http(s) test, because the WebView2 form of a preview origin is an `http://` URL that would
/// otherwise pass for a live web page.
pub fn classify_frame(frame_uri: &str, editable_prefixes: &[&str]) -> FrameKind {
    if frame_uri == SRCDOC_FRAME || editable_prefixes.iter().any(|p| frame_uri.starts_with(p)) {
        FrameKind::Editable
    } else if frame_uri.starts_with("http://") || frame_uri.starts_with("https://") {
        FrameKind::Web
    } else {
        FrameKind::Other
    }
}

/// What the menu was opened on. Also the payload of an event-routed item
/// (`{"frameUri", "linkUri", "kind"}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct MenuContext {
    /// URI of the frame that was right-clicked (empty when WebView2 gave none).
    pub frame_uri: String,
    /// URI of the link under the pointer (empty when the target is not a link). For `srcdoc` this
    /// resolves a relative href against `about:srcdoc` and is unreliable: use it only as a yes/no.
    pub link_uri: String,
    /// The frame's classification.
    pub kind: FrameKind,
}

impl MenuContext {
    pub fn new(frame_uri: &str, link_uri: &str, kind: FrameKind) -> Self {
        Self {
            frame_uri: frame_uri.into(),
            link_uri: link_uri.into(),
            kind,
        }
    }

    pub fn on_link(&self) -> bool {
        !self.link_uri.trim().is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemWhen {
    Always,
    OnLink,
}

type Classifier = Arc<dyn Fn(&str) -> FrameKind + Send + Sync>;
type Action = Arc<dyn Fn(&MenuContext) + Send + Sync>;

/// What a picked item does.
#[derive(Clone)]
enum Route {
    /// Run this closure. Only the WebView2 pick handler (`#[cfg(windows)]`) invokes it; on other
    /// platforms the planning code builds and classifies the route without ever reading it.
    #[cfg_attr(not(windows), allow(dead_code))]
    Callback(Action),
    /// Emit this Tauri event with the [`MenuContext`] as payload.
    Event(String),
}

/// One app item appended to the engine menu.
#[derive(Clone)]
pub struct CustomItem {
    label: String,
    when: ItemWhen,
    frames: Vec<FrameKind>,
    route: Route,
}

impl CustomItem {
    /// An item shown on every claimed frame ([`FrameKind::Editable`] and [`FrameKind::Web`]).
    /// `action` runs when the user picks it, on the WebView2 thread; keep it short.
    pub fn new(label: &str, action: impl Fn(&MenuContext) + Send + Sync + 'static) -> Self {
        Self::routed(label, Route::Callback(Arc::new(action)))
    }

    /// Like [`CustomItem::new`], but a pick emits the Tauri event `event` (payload
    /// [`MenuContext`]) to the renderer instead of calling Rust.
    pub fn emit(label: &str, event: &str) -> Self {
        Self::routed(label, Route::Event(event.to_owned()))
    }

    fn routed(label: &str, route: Route) -> Self {
        Self {
            label: label.to_owned(),
            when: ItemWhen::Always,
            frames: vec![FrameKind::Editable, FrameKind::Web],
            route,
        }
    }

    /// Offer the item only when the target is a link.
    pub fn on_link(mut self) -> Self {
        self.when = ItemWhen::OnLink;
        self
    }

    /// Offer the item only on these frame kinds. [`FrameKind::Other`] is never touched, whatever is
    /// listed here.
    pub fn frames(mut self, frames: &[FrameKind]) -> Self {
        self.frames = frames.to_vec();
        self
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// The event this item emits, or `None` for a callback item.
    pub fn event_name(&self) -> Option<&str> {
        match &self.route {
            Route::Event(name) => Some(name),
            Route::Callback(_) => None,
        }
    }
}

impl std::fmt::Debug for CustomItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustomItem")
            .field("label", &self.label)
            .field("when", &self.when)
            .field("frames", &self.frames)
            .field("event", &self.event_name())
            .finish_non_exhaustive()
    }
}

/// How the menu of one webview is grafted.
#[derive(Clone)]
pub struct ContextMenuConfig {
    classifier: Classifier,
    remove_names: Vec<String>,
    remove_empty_submenus: bool,
    items: Vec<CustomItem>,
}

/// The decision for one context-menu request.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GraftPlan {
    /// False: leave the menu alone (the frame is [`FrameKind::Other`]).
    pub touch: bool,
    /// Engine items (by WebView2 item name) to remove.
    pub remove_names: Vec<String>,
    /// Remove submenus that have no children (for example an empty "More tools").
    pub remove_empty_submenus: bool,
    /// Indices into the config's items to append, in order.
    pub items: Vec<usize>,
}

impl ContextMenuConfig {
    /// `classifier` maps a frame URI to its kind; [`classify_frame`] is the usual body.
    pub fn new(classifier: impl Fn(&str) -> FrameKind + Send + Sync + 'static) -> Self {
        Self {
            classifier: Arc::new(classifier),
            remove_names: Vec::new(),
            remove_empty_submenus: true,
            items: Vec::new(),
        }
    }

    /// Engine items to drop, by WebView2 item name (see [`LINK_NEW_WINDOW_ITEM_NAMES`]).
    pub fn remove_items(mut self, names: &[&str]) -> Self {
        self.remove_names = names.iter().map(|n| (*n).to_owned()).collect();
        self
    }

    /// Keep empty submenus (they are removed by default).
    pub fn keep_empty_submenus(mut self) -> Self {
        self.remove_empty_submenus = false;
        self
    }

    /// Append an app item (items appear in the order they were added).
    pub fn item(mut self, item: CustomItem) -> Self {
        self.items.push(item);
        self
    }

    /// Classify `frame_uri` with the configured classifier.
    pub fn classify(&self, frame_uri: &str) -> FrameKind {
        (self.classifier)(frame_uri)
    }

    /// The decision for a request. Pure.
    pub fn plan(&self, context: &MenuContext) -> GraftPlan {
        if context.kind == FrameKind::Other {
            return GraftPlan {
                touch: false,
                remove_names: Vec::new(),
                remove_empty_submenus: false,
                items: Vec::new(),
            };
        }
        let items = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item.frames.contains(&context.kind)
                    && (item.when == ItemWhen::Always || context.on_link())
            })
            .map(|(index, _)| index)
            .collect();
        GraftPlan {
            touch: true,
            remove_names: self.remove_names.clone(),
            remove_empty_submenus: self.remove_empty_submenus,
            items,
        }
    }
}

/// Wrap a URL callback as a router: an http(s) URL is handed to `on_url` and reported handled; any
/// other URL is left alone (and so to the shell's navigation policy). For an app that answers its
/// own `NewWindowRequested`; the shell's `windows.rs` policy stays the default.
pub fn http_router(
    on_url: impl Fn(&str) + Send + Sync + 'static,
) -> impl Fn(&str) -> bool + Send + Sync + 'static {
    move |url| {
        if is_http_url(url) {
            on_url(url.trim());
            true
        } else {
            false
        }
    }
}

/// Graft the app's items into `window`'s webview context menu (Windows; a no-op elsewhere, where
/// the platform's own menu applies). The shell composes it with the DevTools and hardening stages
/// in the fixed order described in the module docs, whenever it is called. Calling it again
/// replaces the previous config. `Err` only when the webview cannot be reached; a runtime without
/// `ICoreWebView2_11` leaves the engine menu as it is.
pub fn install_context_menu<R: Runtime>(
    window: &WebviewWindow<R>,
    config: ContextMenuConfig,
) -> Result<(), ShellError> {
    #[cfg(windows)]
    {
        windows_impl::install(window, Some(config))
    }
    #[cfg(not(windows))]
    {
        let _ = (window, config);
        Ok(())
    }
}

/// Remove the graft installed by [`install_context_menu`] (Windows; a no-op elsewhere). The
/// DevTools and hardening stages stay.
pub fn remove_context_menu<R: Runtime>(window: &WebviewWindow<R>) -> Result<(), ShellError> {
    #[cfg(windows)]
    {
        windows_impl::install(window, None)
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        Ok(())
    }
}

/// The stages of each webview's one handler, and the rule that turns them into `Handled`.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) mod chain {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use super::{Action, ContextMenuConfig};

    /// How the DevTools policy filters the engine menu (stage 1).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum InspectRule {
        /// A denied webview: remove "Inspect" from every menu.
        Strip,
        /// A content webview: keep only "Inspect"; with none left, show no menu.
        OnlyInspect,
    }

    /// The hardening layer's rule: hide the engine menu, except on an editable target when allowed.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct SuppressRule {
        pub allow_editable: bool,
    }

    /// The shell-owned stages of one webview (the app graft is held apart).
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub(crate) struct ChainRules {
        pub inspect: Option<InspectRule>,
        pub suppress: Option<SuppressRule>,
    }

    /// The app graft with its resolved pick actions (index-aligned with the config's items).
    #[derive(Clone)]
    pub(crate) struct Graft {
        pub config: Arc<ContextMenuConfig>,
        pub actions: Vec<Action>,
    }

    #[derive(Default)]
    struct Chain {
        rules: ChainRules,
        graft: Option<Graft>,
        /// (COM identity of the webview, event token) of the native handler.
        registered: Option<(usize, i64)>,
    }

    static CHAINS: Mutex<Option<HashMap<String, Chain>>> = Mutex::new(None);

    fn with_chain<T>(label: &str, f: impl FnOnce(&mut Chain) -> T) -> T {
        let mut chains = CHAINS.lock().unwrap_or_else(|poison| poison.into_inner());
        f(chains
            .get_or_insert_with(HashMap::new)
            .entry(label.to_owned())
            .or_default())
    }

    pub(crate) fn set_inspect(label: &str, rule: Option<InspectRule>) {
        with_chain(label, |c| c.rules.inspect = rule);
    }

    pub(crate) fn set_suppress(label: &str, rule: Option<SuppressRule>) {
        with_chain(label, |c| c.rules.suppress = rule);
    }

    pub(crate) fn set_graft(label: &str, graft: Option<Graft>) {
        with_chain(label, |c| c.graft = graft);
    }

    /// What the handler reads when the event fires, so a stage set after registration still applies.
    pub(crate) fn snapshot(label: &str) -> (ChainRules, Option<Graft>) {
        with_chain(label, |c| (c.rules, c.graft.clone()))
    }

    fn same_webview(registered: Option<(usize, i64)>, identity: usize) -> Option<i64> {
        registered.and_then(|(id, token)| (identity != 0 && id == identity).then_some(token))
    }

    /// Whether the webview with COM `identity` needs the native handler registered now: some stage
    /// or graft exists and no handler is registered for this very webview.
    pub(crate) fn needs_handler(label: &str, identity: usize) -> bool {
        with_chain(label, |c| {
            (c.rules != ChainRules::default() || c.graft.is_some())
                && same_webview(c.registered, identity).is_none()
        })
    }

    pub(crate) fn set_registered(label: &str, registered: Option<(usize, i64)>) {
        with_chain(label, |c| c.registered = registered);
    }

    /// A webview of this label became ready: forget its shell stages and its registration. Returns
    /// the event token to remove when the registration belongs to this very webview (applied twice);
    /// a registration of an earlier webview of the same label is dropped without a removal. The app
    /// graft stays, so it applies to the new webview.
    pub(crate) fn reset(label: &str, identity: usize) -> Option<i64> {
        with_chain(label, |c| {
            let token = same_webview(c.registered, identity);
            c.registered = None;
            c.rules = ChainRules::default();
            token
        })
    }

    /// Whether the event is `Handled` (no engine menu), given what the stages left.
    ///
    /// * `grafted`: app items appended in stage 2. Any means the menu must show: `false`.
    /// * Otherwise a content webview ([`InspectRule::OnlyInspect`]) with `items_left == 0` shows
    ///   nothing: `true`.
    /// * Otherwise hardening hides the menu, except on an editable target when it allows that.
    pub(crate) fn resolve_handled(
        rules: ChainRules,
        target_editable: bool,
        items_left: usize,
        grafted: usize,
    ) -> bool {
        if grafted > 0 {
            return false;
        }
        if rules.inspect == Some(InspectRule::OnlyInspect) && items_left == 0 {
            return true;
        }
        match rules.suppress {
            Some(rule) => !(rule.allow_editable && target_editable),
            None => false,
        }
    }
}

#[cfg(windows)]
pub(crate) use chain::{set_inspect, set_suppress, InspectRule, SuppressRule};
#[cfg(windows)]
pub(crate) use windows_impl::{reset, sync};

#[cfg(windows)]
mod windows_impl {
    use super::chain::{self, ChainRules, Graft, InspectRule};
    use super::*;
    use crate::winapi::webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2, ICoreWebView2ContextMenuItem, ICoreWebView2ContextMenuItemCollection,
        ICoreWebView2ContextMenuRequestedEventArgs, ICoreWebView2Environment9, ICoreWebView2_11,
        ICoreWebView2_2, COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND,
        COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_COMMAND, COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_SUBMENU,
    };
    use crate::winapi::webview2_com::{
        ContextMenuRequestedEventHandler, CustomItemSelectedEventHandler,
    };
    use crate::winapi::windows::core::{IUnknown, Interface, BOOL, HSTRING, PWSTR};
    use crate::winapi::windows::Win32::System::Com::IStream;
    use tauri::{Emitter, Manager};

    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoTaskMemFree(block: *const core::ffi::c_void);
    }

    type WinResult<T> = crate::winapi::windows::core::Result<T>;

    /// Copy a CoTaskMem string WebView2 returned and free it. A null pointer is the empty string.
    fn take_string(text: PWSTR) -> String {
        if text.is_null() {
            return String::new();
        }
        // SAFETY: WebView2 allocates out-parameter strings with CoTaskMemAlloc; copied, then freed once.
        unsafe {
            let out = text.to_string().unwrap_or_default();
            CoTaskMemFree(text.0 as *const core::ffi::c_void);
            out
        }
    }

    /// The COM identity of a webview: the `IUnknown` pointer, stable across interface casts. 0 when
    /// it cannot be read (then no registration is ever taken for the same webview).
    fn identity_of(core: &ICoreWebView2) -> usize {
        core.cast::<IUnknown>()
            .map(|unknown| unknown.as_raw() as usize)
            .unwrap_or(0)
    }

    /// A webview of `label` became ready (the DevTools layer calls this first, for every webview):
    /// forget the shell stages and remove this webview's earlier handler, if it is applied twice.
    ///
    /// # Safety
    /// Call on the thread WebView2 gave `core` to us on (inside `with_webview`).
    pub(crate) unsafe fn reset(label: &str, core: &ICoreWebView2) {
        if let Some(token) = chain::reset(label, identity_of(core)) {
            if let Ok(wv11) = core.cast::<ICoreWebView2_11>() {
                let _ = wv11.remove_ContextMenuRequested(token);
            }
        }
    }

    /// Register the webview's one `ContextMenuRequested` handler when it has a stage or a graft and
    /// none is registered for this webview yet. Every layer calls this after setting its stage.
    ///
    /// # Safety
    /// As [`reset`].
    pub(crate) unsafe fn sync(label: &str, core: &ICoreWebView2) {
        let Ok(wv11) = core.cast::<ICoreWebView2_11>() else {
            return;
        };
        let identity = identity_of(core);
        if !chain::needs_handler(label, identity) {
            return;
        }
        let owner = label.to_owned();
        let handler = ContextMenuRequestedEventHandler::create(Box::new(move |sender, args| {
            // SAFETY: WebView2 invokes the handler on the thread that owns the live objects.
            unsafe { on_requested(&owner, sender, args) }
        }));
        let mut token: i64 = 0;
        if wv11.add_ContextMenuRequested(&handler, &mut token).is_ok() {
            chain::set_registered(label, Some((identity, token)));
        }
    }

    /// Set (or clear) the app graft of `window`'s webview and make sure the handler is registered.
    pub(crate) fn install<R: Runtime>(
        window: &WebviewWindow<R>,
        config: Option<ContextMenuConfig>,
    ) -> Result<(), ShellError> {
        let label = window.label().to_owned();
        let graft = config.map(|config| {
            // Event-routed items emit through this app handle; callbacks are used as given.
            let actions = config
                .items
                .iter()
                .map(|item| match &item.route {
                    Route::Callback(action) => action.clone(),
                    Route::Event(name) => {
                        let app = window.app_handle().clone();
                        let name = name.clone();
                        Arc::new(move |context: &MenuContext| {
                            let _ = app.emit(&name, context);
                        }) as Action
                    }
                })
                .collect();
            Graft {
                config: Arc::new(config),
                actions,
            }
        });
        window
            .with_webview(move |webview| {
                chain::set_graft(&label, graft);
                // SAFETY: Tauri hands over the live controller; used only inside this call.
                unsafe {
                    if let Ok(core) = webview.controller().CoreWebView2() {
                        sync(&label, &core);
                    }
                }
            })
            .map_err(|e| ShellError::Other(format!("webview unavailable: {e}")))
    }

    /// Remove the items for which `remove` is true; returns how many are left. An entry that cannot
    /// be read is kept and a failed removal is ignored: tidying is best effort.
    pub(crate) unsafe fn retain_items(
        items: &ICoreWebView2ContextMenuItemCollection,
        remove: impl Fn(&ICoreWebView2ContextMenuItem) -> bool,
    ) -> usize {
        let mut count: u32 = 0;
        if items.Count(&mut count).is_err() {
            return 0;
        }
        let mut i = count;
        while i > 0 {
            i -= 1;
            if let Ok(item) = items.GetValueAtIndex(i) {
                if remove(&item) {
                    let _ = items.RemoveValueAtIndex(i);
                }
            }
        }
        item_count(items)
    }

    unsafe fn item_count(items: &ICoreWebView2ContextMenuItemCollection) -> usize {
        let mut left: u32 = 0;
        let _ = items.Count(&mut left);
        left as usize
    }

    /// The "Inspect" items of the default menu (WebView2 names it with "inspect" in it).
    pub(crate) fn is_inspect_item(item: &ICoreWebView2ContextMenuItem) -> bool {
        let mut name = PWSTR::null();
        // SAFETY: Name writes a CoTaskMem string, which `take_string` copies and frees.
        let named = unsafe { item.Name(&mut name) }.is_ok();
        named && take_string(name).to_ascii_lowercase().contains("inspect")
    }

    fn is_empty_submenu(item: &ICoreWebView2ContextMenuItem) -> bool {
        let mut kind = COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND(0);
        // SAFETY: a live menu item, on the thread that delivered the event.
        unsafe {
            if item.Kind(&mut kind).is_err() || kind != COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_SUBMENU
            {
                return false;
            }
            let Ok(children) = item.Children() else {
                return false;
            };
            let mut count: u32 = 0;
            children.Count(&mut count).is_ok() && count == 0
        }
    }

    fn has_name(item: &ICoreWebView2ContextMenuItem, names: &[String]) -> bool {
        let mut name = PWSTR::null();
        // SAFETY: as `is_inspect_item`.
        if unsafe { item.Name(&mut name) }.is_err() {
            return false;
        }
        let name = take_string(name);
        names.contains(&name)
    }

    /// The single handler body: the three stages of the module docs, in order.
    unsafe fn on_requested(
        label: &str,
        sender: Option<ICoreWebView2>,
        args: Option<ICoreWebView2ContextMenuRequestedEventArgs>,
    ) -> WinResult<()> {
        let Some(args) = args else { return Ok(()) };
        let (rules, graft) = chain::snapshot(label);
        if rules == ChainRules::default() && graft.is_none() {
            return Ok(());
        }
        let items = args.MenuItems()?;

        // 1. Inspect filter.
        match rules.inspect {
            Some(InspectRule::Strip) => {
                retain_items(&items, is_inspect_item);
            }
            Some(InspectRule::OnlyInspect) => {
                retain_items(&items, |item| !is_inspect_item(item));
            }
            None => {}
        }

        // 2. App graft.
        let grafted = match (&graft, &sender) {
            (Some(graft), Some(core)) => apply_graft(graft, core, &args, &items),
            _ => 0,
        };

        // 3. Resolution.
        let mut editable = BOOL(0);
        if let Ok(target) = args.ContextMenuTarget() {
            let _ = target.IsEditable(&mut editable);
        }
        let handled =
            chain::resolve_handled(rules, editable.as_bool(), item_count(&items), grafted);
        // Never touch `Handled` when nothing here decided it; an added app item overrides it.
        if handled || grafted > 0 {
            args.SetHandled(handled)?;
        }
        Ok(())
    }

    /// Stage 2. Best effort: any failure leaves the menu as the earlier stage made it. Returns how
    /// many app items were appended.
    unsafe fn apply_graft(
        graft: &Graft,
        core: &ICoreWebView2,
        args: &ICoreWebView2ContextMenuRequestedEventArgs,
        items: &ICoreWebView2ContextMenuItemCollection,
    ) -> usize {
        let Ok(target) = args.ContextMenuTarget() else {
            return 0;
        };
        let mut frame = PWSTR::null();
        if target.FrameUri(&mut frame).is_err() {
            return 0;
        }
        let frame_uri = take_string(frame);
        let mut link = PWSTR::null();
        let _ = target.LinkUri(&mut link);
        let link_uri = take_string(link);
        let context = MenuContext::new(&frame_uri, &link_uri, graft.config.classify(&frame_uri));
        let plan = graft.config.plan(&context);
        if !plan.touch {
            return 0;
        }
        if plan.remove_empty_submenus {
            retain_items(items, is_empty_submenu);
        }
        if !plan.remove_names.is_empty() {
            retain_items(items, |item| has_name(item, &plan.remove_names));
        }
        if plan.items.is_empty() {
            return 0;
        }
        let Ok(env) = core
            .cast::<ICoreWebView2_2>()
            .and_then(|core| core.Environment())
            .and_then(|env| env.cast::<ICoreWebView2Environment9>())
        else {
            return 0;
        };
        let mut added = 0;
        for index in plan.items {
            let (Some(item), Some(action)) =
                (graft.config.items.get(index), graft.actions.get(index))
            else {
                continue;
            };
            let action = action.clone();
            let context = context.clone();
            let appended = append_command(&env, items, &item.label, move |_item, _args| {
                action(&context);
                Ok(())
            });
            if appended.is_ok() {
                added += 1;
            }
        }
        added
    }

    unsafe fn append_command(
        env: &ICoreWebView2Environment9,
        items: &ICoreWebView2ContextMenuItemCollection,
        label: &str,
        on_select: impl FnMut(Option<ICoreWebView2ContextMenuItem>, Option<IUnknown>) -> WinResult<()>
            + 'static,
    ) -> WinResult<()> {
        let label = HSTRING::from(label);
        let item = env.CreateContextMenuItem(
            &label,
            None::<&IStream>,
            COREWEBVIEW2_CONTEXT_MENU_ITEM_KIND_COMMAND,
        )?;
        let mut token: i64 = 0;
        item.add_CustomItemSelected(
            &CustomItemSelectedEventHandler::create(Box::new(on_select)),
            &mut token,
        )?;
        let mut count: u32 = 0;
        items.Count(&mut count)?;
        items.InsertValueAtIndex(count, &item)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::chain::{self, ChainRules, InspectRule, SuppressRule};
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const PREVIEW: [&str; 2] = ["http://app-preview.localhost/", "app-preview://"];

    fn config() -> ContextMenuConfig {
        ContextMenuConfig::new(|frame| classify_frame(frame, &PREVIEW))
            .remove_items(&LINK_NEW_WINDOW_ITEM_NAMES)
            .item(CustomItem::new("Open link in tab", |_| {}).on_link())
            .item(
                CustomItem::emit("View source", "app://view-source").frames(&[FrameKind::Editable]),
            )
    }

    fn context(config: &ContextMenuConfig, frame: &str, link: &str) -> MenuContext {
        MenuContext::new(frame, link, config.classify(frame))
    }

    #[test]
    fn frames_classify_with_editable_origins_before_http() {
        assert_eq!(
            classify_frame("about:srcdoc", &PREVIEW),
            FrameKind::Editable
        );
        assert_eq!(
            classify_frame("http://app-preview.localhost/abc", &PREVIEW),
            FrameKind::Editable
        );
        assert_eq!(
            classify_frame("app-preview://localhost/abc", &PREVIEW),
            FrameKind::Editable
        );
        assert_eq!(
            classify_frame("https://example.com/", &PREVIEW),
            FrameKind::Web
        );
        assert_eq!(
            classify_frame("http://tauri.localhost/index.html", &PREVIEW),
            FrameKind::Web
        );
        assert_eq!(classify_frame("", &PREVIEW), FrameKind::Other);
        assert_eq!(
            classify_frame("data:text/html,x", &PREVIEW),
            FrameKind::Other
        );
        assert_eq!(classify_frame("about:blank", &PREVIEW), FrameKind::Other);
    }

    #[test]
    fn app_frames_are_never_touched() {
        let config = config();
        let plan = config.plan(&context(&config, "", ""));
        assert!(!plan.touch);
        assert!(plan.items.is_empty() && plan.remove_names.is_empty());
        let plan = config.plan(&context(&config, "tauri://localhost/", "https://a"));
        assert!(!plan.touch && plan.items.is_empty());
    }

    #[test]
    fn items_follow_frame_kind_and_link_target() {
        let config = config();
        // Editable preview, no link: only "View source".
        let plan = config.plan(&context(&config, "about:srcdoc", ""));
        assert!(plan.touch);
        assert_eq!(plan.items, vec![1]);
        // Editable preview on a link: both, in registration order.
        let plan = config.plan(&context(&config, "about:srcdoc", "about:srcdoc#x"));
        assert_eq!(plan.items, vec![0, 1]);
        // Web page on a link: the link item only, never "View source".
        let plan = config.plan(&context(&config, "https://example.com/", "https://a/"));
        assert_eq!(plan.items, vec![0]);
        // Web page, no link: nothing appended, but the menu is still tidied.
        let plan = config.plan(&context(&config, "https://example.com/", "  "));
        assert!(plan.touch && plan.items.is_empty());
        assert_eq!(plan.remove_names, LINK_NEW_WINDOW_ITEM_NAMES);
        assert!(plan.remove_empty_submenus);
        let kept = ContextMenuConfig::new(|_| FrameKind::Web).keep_empty_submenus();
        assert!(
            !kept
                .plan(&context(&kept, "https://x/", ""))
                .remove_empty_submenus
        );
    }

    #[test]
    fn other_is_never_listed_as_an_item_frame() {
        let config = ContextMenuConfig::new(|_| FrameKind::Other)
            .item(CustomItem::new("x", |_| {}).frames(&[FrameKind::Other]));
        let plan = config.plan(&context(&config, "tauri://localhost/", ""));
        assert!(!plan.touch && plan.items.is_empty());
    }

    #[test]
    fn items_route_to_a_callback_or_an_event() {
        let seen = Arc::new(std::sync::Mutex::new(String::new()));
        let sink = seen.clone();
        let callback = CustomItem::new("Open", move |ctx| {
            *sink.lock().unwrap() = ctx.link_uri.clone();
        });
        assert_eq!(callback.event_name(), None);
        assert_eq!(callback.label(), "Open");
        let ctx = MenuContext::new("about:srcdoc", "https://a/", FrameKind::Editable);
        if let Route::Callback(action) = &callback.route {
            action(&ctx);
        }
        assert_eq!(*seen.lock().unwrap(), "https://a/");
        let event = CustomItem::emit("View source", "app://view-source");
        assert_eq!(event.event_name(), Some("app://view-source"));
        let json = serde_json::to_value(&ctx).unwrap();
        assert_eq!(json["frameUri"], "about:srcdoc");
        assert_eq!(json["linkUri"], "https://a/");
        assert_eq!(json["kind"], "editable");
    }

    #[test]
    fn http_router_takes_only_http_urls() {
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = seen.clone();
        let router = http_router(move |url| {
            assert!(url.starts_with("http"));
            counter.fetch_add(1, Ordering::SeqCst);
        });
        assert!(router("https://example.com/a"));
        assert!(router("  http://example.com "));
        assert!(!router("mailto:a@b.c"));
        assert!(!router("file:///etc/passwd"));
        assert!(!router("javascript:alert(1)"));
        assert!(!router(""));
        assert_eq!(seen.load(Ordering::SeqCst), 2);
    }

    fn rules(inspect: Option<InspectRule>, suppress: Option<SuppressRule>) -> ChainRules {
        ChainRules { inspect, suppress }
    }

    const HIDE: Option<SuppressRule> = Some(SuppressRule {
        allow_editable: false,
    });
    const HIDE_BUT_EDITABLE: Option<SuppressRule> = Some(SuppressRule {
        allow_editable: true,
    });

    #[test]
    fn resolution_reproduces_the_separate_devtools_and_hardening_handlers() {
        let strip = Some(InspectRule::Strip);
        let only = Some(InspectRule::OnlyInspect);
        // Developer webview, no hardening: the engine menu is untouched.
        assert!(!chain::resolve_handled(rules(None, None), false, 9, 0));
        // Denied webview, context menu not blocked: Inspect is stripped, the menu shows.
        assert!(!chain::resolve_handled(rules(strip, None), false, 5, 0));
        // Denied webview with the context menu blocked: hidden except on an allowed editable.
        assert!(chain::resolve_handled(rules(strip, HIDE), true, 5, 0));
        let editable_ok = rules(strip, HIDE_BUT_EDITABLE);
        assert!(chain::resolve_handled(editable_ok, false, 5, 0));
        assert!(!chain::resolve_handled(editable_ok, true, 5, 0));
        // Content webview: only Inspect stays; none left means no menu.
        assert!(!chain::resolve_handled(rules(only, None), false, 1, 0));
        assert!(chain::resolve_handled(rules(only, None), false, 0, 0));
    }

    #[test]
    fn an_app_item_always_wins_over_every_hiding_stage() {
        let only = Some(InspectRule::OnlyInspect);
        assert!(!chain::resolve_handled(rules(only, None), false, 0, 1));
        assert!(!chain::resolve_handled(rules(None, HIDE), false, 1, 2));
        assert!(!chain::resolve_handled(
            rules(Some(InspectRule::Strip), HIDE_BUT_EDITABLE),
            false,
            3,
            1
        ));
    }

    #[test]
    fn stages_are_independent_of_the_order_they_are_set_in() {
        let label = "wv2menu-order-test";
        chain::set_suppress(label, HIDE_BUT_EDITABLE);
        chain::set_inspect(label, Some(InspectRule::Strip));
        let (first, _) = chain::snapshot(label);
        let other = "wv2menu-order-test-2";
        chain::set_inspect(other, Some(InspectRule::Strip));
        chain::set_suppress(other, HIDE_BUT_EDITABLE);
        let (second, _) = chain::snapshot(other);
        assert_eq!(first, second);
        assert_eq!(first, rules(Some(InspectRule::Strip), HIDE_BUT_EDITABLE));
    }

    #[test]
    fn one_handler_per_webview_and_a_reset_keeps_the_graft() {
        let label = "wv2menu-register-test";
        // Nothing to compose yet: no handler is registered.
        assert!(!chain::needs_handler(label, 10));
        chain::set_inspect(label, Some(InspectRule::OnlyInspect));
        assert!(chain::needs_handler(label, 10));
        chain::set_registered(label, Some((10, 7)));
        // Another layer syncing the same webview adds no second handler.
        assert!(!chain::needs_handler(label, 10));
        // A different webview under the same label does need its own.
        assert!(chain::needs_handler(label, 11));
        // An unreadable identity never counts as the same webview.
        assert!(chain::needs_handler(label, 0));

        let graft = chain::Graft {
            config: Arc::new(ContextMenuConfig::new(|_| FrameKind::Web)),
            actions: Vec::new(),
        };
        chain::set_graft(label, Some(graft));
        // The same webview applied twice: its token is handed back for removal, stages cleared.
        assert_eq!(chain::reset(label, 10), Some(7));
        let (rules_after, graft_after) = chain::snapshot(label);
        assert_eq!(rules_after, ChainRules::default());
        assert!(graft_after.is_some(), "the app graft survives a reset");
        // With only the graft left a handler is still needed, and registration was forgotten.
        assert!(chain::needs_handler(label, 10));

        chain::set_registered(label, Some((10, 8)));
        // A new webview under the label: the old registration is dropped, no token to remove.
        assert_eq!(chain::reset(label, 12), None);
        assert!(chain::needs_handler(label, 12));
        chain::set_graft(label, None);
        assert!(!chain::needs_handler(label, 12));
    }
}
