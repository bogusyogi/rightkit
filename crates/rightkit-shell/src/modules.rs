//! Modular install: opt individual shell modules out, or install only the state the broker, stream,
//! recovery, frameless and clipboard commands need.
//!
//! ```ignore
//! let plugin = rightkit_shell::ShellPlugin::builder()
//!     .shell(rightkit_shell::Shell::builder("viewright").app_name("ViewRight").build())
//!     .without(Module::Presence)
//!     .without(Module::Watchdog)
//!     .build();
//!
//! // Or: broker, stream, recovery, frameless and clipboard only, with no hardening, navigation
//! // policy, window restore, show-on-ready, watchdog or presence.
//! let plugin = rightkit_shell::ShellPlugin::builder()
//!     .shell(rightkit_shell::Shell::builder("viewright").build())
//!     .install_state_only();
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

use crate::Shell;

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
    pub const ALL: [Module; 11] = [
        Module::JsHardening,
        Module::NavigationPolicy,
        Module::WindowState,
        Module::ShowOnReady,
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

    /// Stable snake_case name, used in diagnostics (`rk_shell_status`).
    pub fn name(self) -> &'static str {
        match self {
            Module::JsHardening => "js_hardening",
            Module::NavigationPolicy => "navigation_policy",
            Module::WindowState => "window_state",
            Module::ShowOnReady => "show_on_ready",
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

    /// Only the state-only set (`Module::STATE`).
    pub fn state_only() -> Self {
        Module::STATE.iter().fold(Self(0), |set, m| set.with(*m))
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
    pub fn lifecycle_only() -> Self {
        Module::LIFECYCLE_OFF
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
        }
    }
}

/// Builder returned by [`ShellPlugin::builder`].
#[must_use]
pub struct ShellPluginBuilder {
    shell: Option<Shell>,
    modules: ModuleSet,
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

    /// Install the plugin with the modules that remain enabled.
    ///
    /// # Panics
    /// Panics when [`ShellPluginBuilder::shell`] was never called, which is a programmer error at
    /// startup, the same as an invalid app id.
    pub fn build(self) -> TauriPlugin<Wry> {
        let shell = self.shell.expect(
            "rightkit-shell: ShellPlugin::builder() needs .shell(Shell::builder(..).build())",
        );
        let mut cfg = shell.cfg;
        cfg.modules = self.modules;
        crate::install(cfg)
    }

    /// Install the lifecycle without the webview policy: [`ModuleSet::lifecycle_only`].
    pub fn lifecycle_only(mut self) -> TauriPlugin<Wry> {
        self.modules = ModuleSet::lifecycle_only();
        self.build()
    }

    /// Install only the state the broker, stream, recovery, frameless and clipboard commands use.
    /// This replaces any earlier `without` calls: hardening, navigation policy, window restore,
    /// show-on-ready, watchdog and presence are all off.
    pub fn install_state_only(mut self) -> TauriPlugin<Wry> {
        self.modules = ModuleSet::state_only();
        self.build()
    }
}
