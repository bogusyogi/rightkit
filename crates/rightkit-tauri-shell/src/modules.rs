//! Modular install: opt individual shell modules out, or install only the state the broker, stream,
//! recovery, frameless and clipboard commands need.
//!
//! ```text
//! let plugin = rightkit_tauri_shell::ShellPlugin::builder()
//!     .shell(rightkit_tauri_shell::Shell::builder("viewright").app_name("ViewRight").build())
//!     .without(Module::Presence)
//!     .without(Module::Watchdog)
//!     .build();
//!
//! // Or: broker, stream, recovery, frameless and clipboard only, with no hardening, navigation
//! // policy, window restore, show-on-ready, watchdog or presence.
//! let plugin = rightkit_tauri_shell::ShellPlugin::builder()
//!     .shell(rightkit_tauri_shell::Shell::builder("viewright").build())
//!     .install_state_only();
//!
//! // Or: the app owns its windows and keeps the webview policy (hardening and navigation policy).
//! let plugin = rightkit_tauri_shell::ShellPlugin::builder()
//!     .shell(rightkit_tauri_shell::Shell::builder("viewright").build())
//!     .host_windows()
//!     .build();
//! ```
//!
//! An opted-out command family answers `unsupported` ("module ... is disabled for this app") rather
//! than doing nothing, so a renderer never mistakes a disabled module for a silent success.
//!
//! The path broker needs no plugin at all: [`crate::broker::PathBroker`] is a plain `Default`
//! library type. Create one in your own state, grant files with it, and call its `require_*`,
//! `grant_stream`, `grant_site_root` and recovery helpers from your own commands. Only the plugin's
//! command wiring is optional.

use tauri::plugin::TauriPlugin;
use tauri::Wry;

use crate::{Shell, ShellBuilder};

/// A shell module that can be opted out. The comments say what each one controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Module {
    /// Injected DOM hardening script and native webview hardening (`Hardening`).
    JsHardening,
    /// Navigation and popup policy (`NavPolicy`, the external-link handler).
    NavigationPolicy,
    /// Window geometry restore and persistence.
    WindowState,
    /// Show the main window once its webview is ready.
    ShowOnReady,
    /// Reveal the main window on a macOS Dock reopen (`RunEvent::Reopen`) and when the OS hands the
    /// app a URL or file (`RunEvent::Opened`). Off, neither shows, unminimises or focuses anything
    /// (the URL is still delivered). On in every set except where a host removes it.
    RevealOnReopen,
    /// The hang watchdog and its `rightkit-shell://stall` event.
    Watchdog,
    /// The presence loop (fullscreen and meeting detection).
    Presence,
    /// Path-broker commands (`rk_shell_path_*`).
    Broker,
    /// Ranged custom schemes: the stream scheme and the site-root scheme, with their grant commands.
    Stream,
    /// Crash-safe setup recovery commands.
    Recovery,
    /// Frameless chrome for windows opened through the shell, and traffic-light offsets.
    Frameless,
    /// Clipboard commands, including the typed `rk_shell_clipboard_item`.
    Clipboard,
}

impl Module {
    /// Every module, in declaration order.
    pub const ALL: [Module; 12] = [
        Module::JsHardening,
        Module::NavigationPolicy,
        Module::WindowState,
        Module::ShowOnReady,
        Module::RevealOnReopen,
        Module::Watchdog,
        Module::Presence,
        Module::Broker,
        Module::Stream,
        Module::Recovery,
        Module::Frameless,
        Module::Clipboard,
    ];

    /// The modules `install_state_only` keeps.
    pub const STATE: [Module; 5] = [
        Module::Broker,
        Module::Stream,
        Module::Recovery,
        Module::Frameless,
        Module::Clipboard,
    ];

    /// The modules `lifecycle_only` turns off.
    pub const LIFECYCLE_OFF: [Module; 5] = [
        Module::JsHardening,
        Module::NavigationPolicy,
        Module::Presence,
        Module::Watchdog,
        Module::WindowState,
    ];

    /// Modules `lifecycle_only` leaves off by default because a lifecycle-only host owns its own
    /// windows: the host turns one back on explicitly (`ShellBuilder::show_on_ready(true)`).
    pub const LIFECYCLE_HOST_OPT_IN: [Module; 1] = [Module::ShowOnReady];

    /// The modules `host_windows` turns off: the app owns its windows, so the shell restores and
    /// saves no window state, shows nothing on ready, and runs no presence loop or watchdog.
    pub const HOST_WINDOWS_OFF: [Module; 4] = [
        Module::WindowState,
        Module::ShowOnReady,
        Module::Presence,
        Module::Watchdog,
    ];

    /// Stable snake_case name, used in diagnostics (`rk_shell_status`).
    pub fn name(self) -> &'static str {
        match self {
            Module::JsHardening => "js_hardening",
            Module::NavigationPolicy => "navigation_policy",
            Module::WindowState => "window_state",
            Module::ShowOnReady => "show_on_ready",
            Module::RevealOnReopen => "reveal_on_reopen",
            Module::Watchdog => "watchdog",
            Module::Presence => "presence",
            Module::Broker => "broker",
            Module::Stream => "stream",
            Module::Recovery => "recovery",
            Module::Frameless => "frameless",
            Module::Clipboard => "clipboard",
        }
    }
}

/// A set of enabled modules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModuleSet(u16);

impl ModuleSet {
    /// Every module enabled (what [`Shell::plugin`] installs).
    pub fn all() -> Self {
        Module::ALL.iter().fold(Self(0), |set, m| set.with(*m))
    }

    /// Only the state-only set (`Module::STATE`), plus `RevealOnReopen`: reopen handling is
    /// lifecycle, not state, and a state-only install has always revealed on reopen.
    pub fn state_only() -> Self {
        Module::STATE
            .iter()
            .fold(Self(0), |set, m| set.with(*m))
            .with(Module::RevealOnReopen)
    }

    fn bit(module: Module) -> u16 {
        1 << (module as u16)
    }

    /// True when `module` is enabled.
    pub fn contains(self, module: Module) -> bool {
        self.0 & Self::bit(module) != 0
    }

    /// This set with `module` enabled.
    pub fn with(self, module: Module) -> Self {
        Self(self.0 | Self::bit(module))
    }

    /// This set with `module` disabled.
    pub fn without(self, module: Module) -> Self {
        Self(self.0 & !Self::bit(module))
    }

    /// Everything except the five webview-policy modules: no hardening script or native hardening,
    /// no navigation policy, no presence loop, no watchdog and no window-state restore or save.
    /// `ShowOnReady` is also off (`Module::LIFECYCLE_HOST_OPT_IN`): the host owns its windows and
    /// turns it on with `ShellBuilder::show_on_ready(true)` if it wants the shell to show the main
    /// window. `RevealOnReopen` stays on; a host removes it with `ShellBuilder::reveal_on_reopen(false)`.
    /// Prefer [`ModuleSet::host_windows`]: lifecycle_only also removes JS hardening and the
    /// navigation policy.
    pub fn lifecycle_only() -> Self {
        Module::LIFECYCLE_OFF
            .iter()
            .chain(Module::LIFECYCLE_HOST_OPT_IN.iter())
            .fold(Self::all(), |set, m| set.without(*m))
    }

    /// Everything except `Module::HOST_WINDOWS_OFF`: the app owns its windows, so there is no
    /// window-state restore or save, no show-on-ready, no presence loop and no watchdog. The webview
    /// policy stays on: JS hardening, the navigation policy and reveal on reopen.
    pub fn host_windows() -> Self {
        Module::HOST_WINDOWS_OFF
            .iter()
            .fold(Self::all(), |set, m| set.without(*m))
    }

    /// Names of the enabled modules, in declaration order.
    pub fn names(self) -> Vec<&'static str> {
        Module::ALL
            .iter()
            .filter(|m| self.contains(**m))
            .map(|m| m.name())
            .collect()
    }
}

/// Entry point for a modular install. See the module docs for examples.
pub struct ShellPlugin;

impl ShellPlugin {
    /// Start a modular install. Give it the app's configuration with [`ShellPluginBuilder::shell`].
    pub fn builder() -> ShellPluginBuilder {
        ShellPluginBuilder {
            shell: None,
            modules: ModuleSet::all(),
            host_owns_windows: false,
        }
    }
}

/// Builder returned by [`ShellPlugin::builder`].
#[must_use]
pub struct ShellPluginBuilder {
    shell: Option<Shell>,
    modules: ModuleSet,
    /// Set by [`ShellPluginBuilder::host_windows`]. `build` then sets the shell's `lifecycle_only`
    /// switch, which keeps ShowOnReady and the cache repair off unless the host turns them on.
    host_owns_windows: bool,
}

impl ShellPluginBuilder {
    /// The app's configuration, built with `Shell::builder(app_id)...build()`.
    pub fn shell(mut self, shell: Shell) -> Self {
        self.shell = Some(shell);
        self
    }

    /// Opt one module out.
    pub fn without(mut self, module: Module) -> Self {
        self.modules = self.modules.without(module);
        self
    }

    /// Re-enable one module. Additive: it adjusts the set a profile or `without` left.
    pub fn with(mut self, module: Module) -> Self {
        self.modules = self.modules.with(module);
        self
    }

    /// The modules [`ShellPluginBuilder::build`] would install.
    pub fn modules(&self) -> ModuleSet {
        self.modules
    }

    /// Install the plugin with the modules that remain enabled.
    ///
    /// # Panics
    /// Panics when [`ShellPluginBuilder::shell`] was never called, which is a programmer error at
    /// startup, the same as an invalid app id.
    pub fn build(self) -> TauriPlugin<Wry> {
        crate::install(self.into_config())
    }

    /// The configuration [`ShellPluginBuilder::build`] installs.
    fn into_config(self) -> ShellBuilder {
        let (modules, host_owns_windows) = (self.modules, self.host_owns_windows);
        let shell = self.shell.expect(
            "rightkit-tauri-shell: ShellPlugin::builder() needs .shell(Shell::builder(..).build())",
        );
        let mut cfg = shell.cfg;
        cfg.modules = modules;
        if host_owns_windows {
            cfg.lifecycle_only = true;
        }
        cfg
    }

    /// Replace the module set with a profile. `host_owns_windows` belongs to that profile alone, so
    /// an earlier profile's choice does not carry over.
    fn profile(mut self, modules: ModuleSet, host_owns_windows: bool) -> Self {
        self.modules = modules;
        self.host_owns_windows = host_owns_windows;
        self
    }

    /// Use the host-windows profile, [`ModuleSet::host_windows`], for an app that owns its windows.
    /// The shell keeps JS hardening, the navigation policy and reveal on reopen, and does not
    /// restore, save, show or watch windows. Unlike [`ShellPluginBuilder::lifecycle_only`], it keeps
    /// the webview policy. A profile replaces the module set, so earlier `with` and `without` calls
    /// are dropped; calls after it adjust it.
    pub fn host_windows(self) -> Self {
        self.profile(ModuleSet::host_windows(), true)
    }

    /// Install the lifecycle without the webview policy: [`ModuleSet::lifecycle_only`]. Besides the
    /// module set, a lifecycle-only install defaults `repair_cache` to off and shows the main window
    /// on ready only when the host called `show_on_ready(true)`; `repair_cache(true)` turns the
    /// repair back on. Prefer [`ShellPluginBuilder::host_windows`]: lifecycle_only also removes JS
    /// hardening and the navigation policy.
    pub fn lifecycle_only(mut self) -> TauriPlugin<Wry> {
        self.modules = ModuleSet::lifecycle_only();
        if let Some(shell) = self.shell.as_mut() {
            shell.cfg.lifecycle_only = true;
        }
        self.build()
    }

    /// Install only the state the broker, stream, recovery, frameless and clipboard commands use.
    /// This replaces any earlier `without` calls: hardening, navigation policy, window restore,
    /// show-on-ready, watchdog and presence are all off.
    pub fn install_state_only(self) -> TauriPlugin<Wry> {
        self.profile(ModuleSet::state_only(), false).build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builder() -> ShellPluginBuilder {
        ShellPlugin::builder().shell(Shell::builder("viewright").build())
    }

    #[test]
    fn host_windows_keeps_the_webview_policy_and_state_modules() {
        let set = ModuleSet::host_windows();
        for module in [
            Module::JsHardening,
            Module::NavigationPolicy,
            Module::RevealOnReopen,
            Module::Broker,
            Module::Stream,
            Module::Recovery,
            Module::Frameless,
            Module::Clipboard,
        ] {
            assert!(set.contains(module), "{module:?} should stay on");
        }
    }

    #[test]
    fn host_windows_turns_off_the_window_owning_modules() {
        let set = ModuleSet::host_windows();
        for module in [
            Module::WindowState,
            Module::ShowOnReady,
            Module::Presence,
            Module::Watchdog,
        ] {
            assert!(!set.contains(module), "{module:?} should be off");
        }
    }

    #[test]
    fn host_windows_names_are_the_expected_list_in_declaration_order() {
        assert_eq!(
            ModuleSet::host_windows().names(),
            vec![
                "js_hardening",
                "navigation_policy",
                "reveal_on_reopen",
                "broker",
                "stream",
                "recovery",
                "frameless",
                "clipboard",
            ]
        );
    }

    #[test]
    fn with_navigation_policy_after_lifecycle_only_re_enables_it() {
        let set = ModuleSet::lifecycle_only().with(Module::NavigationPolicy);
        assert!(set.contains(Module::NavigationPolicy));
    }

    #[test]
    fn with_after_host_windows_adds_one_module_and_keeps_the_profile() {
        let set = builder().host_windows().with(Module::WindowState).modules();
        assert!(set.contains(Module::WindowState));
        assert!(set.contains(Module::JsHardening));
    }

    #[test]
    fn host_windows_replaces_an_earlier_without() {
        let set = builder().without(Module::Broker).host_windows().modules();
        assert!(set.contains(Module::Broker));
    }

    #[test]
    fn host_windows_builds_a_lifecycle_host_and_other_profiles_do_not() {
        assert!(builder().host_windows().into_config().lifecycle_only);
        assert!(!builder().into_config().lifecycle_only);
    }

    #[test]
    fn install_state_only_clears_an_earlier_host_windows_choice() {
        let cfg = builder()
            .host_windows()
            .profile(ModuleSet::state_only(), false)
            .into_config();
        assert!(!cfg.lifecycle_only);
        assert_eq!(cfg.modules, ModuleSet::state_only());
    }
}
