//! The names the product app gives this server.
//!
//! Every product name the server writes (the CLI name in messages, the state folder, the
//! `~/.config` folder, wire schema ids, HTTP header names, hash domains, the licence app id, the
//! credential namespace, the Github App client id) comes from one [`HostIdentity`] the app passes
//! to [`crate::run`]. The library holds no product's values; a second app passes its own.
//!
//! Everything derives from the app name unless set: for `HostIdentity::new("myapp")` the state
//! folder is `.myapp`, the config folder `~/.config/myapp`, schema ids `myapp.<name>`, headers
//! `x-myapp-<name>`, the licence app and the credential namespace `myapp`. An app that keeps
//! existing data passes its old name, and gets the same paths, ids and digests as before.
//!
//! In this crate's docs and help text, `{app}` stands for the app name.

use std::sync::OnceLock;

/// A product's watch-list for the harness-radar feed (`/runtime/harness-radar`): the source
/// table (TOML) and the current and previous project snapshots (YAML). Without one the feed is
/// empty.
#[derive(Debug, Clone, Default)]
pub struct HarnessRadarData {
    /// `sources.toml`: the watched sources.
    pub sources_toml: String,
    /// The current project snapshot.
    pub snapshot_yaml: String,
    /// The previous snapshot, the baseline for change detection.
    pub previous_yaml: String,
}

/// The product names this server uses. Built by the app, installed once by [`crate::run`].
#[derive(Debug, Clone)]
pub struct HostIdentity {
    app: String,
    home_dir: String,
    config_dir: String,
    license_app: String,
    credential_namespace: String,
    github_client_id: Option<String>,
    homepage: Option<String>,
    harness_radar: Option<HarnessRadarData>,
}

impl HostIdentity {
    /// An identity for the app called `app`: lowercase ASCII letters, digits and `-`, starting
    /// with a letter (it becomes a command name, a folder name and a header segment).
    ///
    /// # Panics
    ///
    /// When `app` is not such a name.
    pub fn new(app: impl Into<String>) -> Self {
        let app = app.into();
        assert!(
            valid_app_name(&app),
            "host app name must be lowercase ASCII letters, digits and '-', starting with a letter: {app:?}"
        );
        Self {
            home_dir: format!(".{app}"),
            config_dir: app.clone(),
            license_app: app.clone(),
            credential_namespace: app.clone(),
            github_client_id: None,
            homepage: None,
            harness_radar: None,
            app,
        }
    }

    /// The state folder name under the workspace and the home directory (default `.{app}`).
    pub fn with_home_dir(mut self, name: impl Into<String>) -> Self {
        self.home_dir = name.into();
        self
    }

    /// The folder name under `~/.config` (default `{app}`).
    pub fn with_config_dir(mut self, name: impl Into<String>) -> Self {
        self.config_dir = name.into();
        self
    }

    /// The app id the suite licence tokens are signed for (default `{app}`).
    pub fn with_license_app(mut self, name: impl Into<String>) -> Self {
        self.license_app = name.into();
        self
    }

    /// The OS credential-store namespace for the Github sign-in (default `{app}`).
    pub fn with_credential_namespace(mut self, name: impl Into<String>) -> Self {
        self.credential_namespace = name.into();
        self
    }

    /// The public client id of the app's Github App. Without one, Github sign-in reports that
    /// it is not configured.
    pub fn with_github_client_id(mut self, id: impl Into<String>) -> Self {
        self.github_client_id = Some(id.into());
        self
    }

    /// The app's public homepage, sent as `HTTP-Referer` to providers that attribute traffic
    /// (OpenRouter). Without one no referer is sent.
    pub fn with_homepage(mut self, url: impl Into<String>) -> Self {
        self.homepage = Some(url.into());
        self
    }

    /// The app's harness-radar watch-list.
    pub fn with_harness_radar(mut self, data: HarnessRadarData) -> Self {
        self.harness_radar = Some(data);
        self
    }

    /// The app name.
    pub fn app(&self) -> &str {
        &self.app
    }

    /// The state folder name.
    pub fn home_dir(&self) -> &str {
        &self.home_dir
    }

    /// The `~/.config` folder name.
    pub fn config_dir(&self) -> &str {
        &self.config_dir
    }

    /// The licence app id.
    pub fn license_app(&self) -> &str {
        &self.license_app
    }

    /// The credential-store namespace.
    pub fn credential_namespace(&self) -> &str {
        &self.credential_namespace
    }

    /// The Github App client id, if the app has one.
    pub fn github_client_id(&self) -> Option<&str> {
        self.github_client_id.as_deref()
    }

    /// The app's homepage, if it has one.
    pub fn homepage(&self) -> Option<&str> {
        self.homepage.as_deref()
    }

    /// The harness-radar watch-list, if the app has one.
    pub fn harness_radar(&self) -> Option<&HarnessRadarData> {
        self.harness_radar.as_ref()
    }
}

fn valid_app_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The identity used before (or without) [`install`]: a neutral name.
const FALLBACK_APP: &str = "agent-server";

static IDENTITY: OnceLock<HostIdentity> = OnceLock::new();

/// Installs the app's identity. Returns `false` when one was already installed (the first wins).
pub fn install(identity: HostIdentity) -> bool {
    IDENTITY.set(identity).is_ok()
}

/// The installed identity, or the neutral fallback.
pub(crate) fn host() -> &'static HostIdentity {
    IDENTITY.get_or_init(|| HostIdentity::new(FALLBACK_APP))
}

/// The app name: the CLI name in messages, log tags, the MCP server name.
pub(crate) fn app() -> &'static str {
    host().app()
}

/// The state folder name (`.{app}` by default).
pub(crate) fn home_dir() -> &'static str {
    host().home_dir()
}

/// The `~/.config` folder name.
pub(crate) fn config_dir() -> &'static str {
    host().config_dir()
}

/// A wire schema id or a label in the app's namespace: `{app}.{name}`.
pub(crate) fn schema(name: &str) -> String {
    format!("{}.{name}", app())
}

/// An HTTP header name in the app's namespace: `x-{app}-{name}`.
pub(crate) fn header(name: &str) -> String {
    format!("x-{}-{name}", app())
}

/// A name with the app as prefix: `{app}-{name}` (thread names, temp folders, ids).
pub(crate) fn prefixed(name: &str) -> String {
    format!("{}-{name}", app())
}

/// A command line in messages: `{app} {args}`.
pub(crate) fn command(args: &str) -> String {
    format!("{} {args}", app())
}

/// A `&'static str` for a name built from the identity, for places that keep names in static
/// tables. Each distinct name is allocated once; the set is small and fixed by the code.
pub(crate) fn interned(name: String) -> &'static str {
    static NAMES: OnceLock<std::sync::Mutex<std::collections::HashSet<&'static str>>> =
        OnceLock::new();
    let mut names = NAMES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(existing) = names.get(name.as_str()) {
        return existing;
    }
    let leaked: &'static str = Box::leak(name.into_boxed_str());
    names.insert(leaked);
    leaked
}

/// A hash domain separator: `{app}.{name}` followed by a NUL byte.
pub(crate) fn domain(name: &str) -> Vec<u8> {
    let mut bytes = schema(name).into_bytes();
    bytes.push(0);
    bytes
}
