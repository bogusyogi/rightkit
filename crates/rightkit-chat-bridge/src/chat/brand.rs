//! What the host app calls itself and which names it already put on disk and on the wire.
//!
//! The bridge is product-neutral: every display name, command name, install path and wire
//! identifier it needs comes from one [`Brand`]. The app installs its brand once at start with
//! [`set_brand`]; until then [`brand`] answers `Default::default`, whose display names are neutral
//! (`RightKit`, `rightkit`) and whose wire and install identifiers are the ones this code shipped
//! with, so data written by an existing app (envelope signatures, receipts, skill folders,
//! reply sockets) keeps working.
//!
//! ```
//! use rightkit_chat_bridge::chat::brand::{brand, set_brand, Brand};
//!
//! let mut mine = Brand::default();
//! mine.product = "MyApp".to_string();
//! mine.cli = "myapp".to_string();
//! set_brand(mine);
//! assert_eq!(brand().product, "MyApp");
//! ```

use std::sync::OnceLock;

/// Names, paths and identifiers the host app chooses.
#[derive(Clone, Debug)]
pub struct Brand {
    /// Display name used in messages and in the text agents read (default `RightKit`).
    pub product: String,
    /// The command agents run, without a platform suffix (default `rightkit`).
    pub cli: String,
    /// Where the app's command is installed on a Mac, for a link that names no path.
    pub cli_mac_path: String,
    /// Where the app's command is installed on Windows (a PowerShell path expression).
    pub cli_windows_path: String,
    /// The plugin id the app's Claude Code and Muse plugins carry.
    pub plugin_id: String,
    /// The Windows hook launcher program beside the app's command.
    pub launcher: String,
    /// The chat skill the bridge installs for Claude and Codex: its folder name.
    pub skill_name: String,
    /// The folder name the skill had before it was renamed; install retires it.
    pub legacy_skill_name: String,
    /// The record file kept beside the installed skill.
    pub manifest_name: String,
    /// The record file of the retired skill folder.
    pub legacy_manifest_name: String,
    /// The text of the chat skill. The host app supplies it; the bridge installs it unchanged.
    /// Empty means the host has none and install refuses.
    pub skill_markdown: String,
    /// The suffix of the one-time copy made of Codex's `hooks.json` (`hooks.json.<suffix>`).
    pub hooks_backup_suffix: String,
    /// What a chat envelope signature is over (a wire identifier).
    pub envelope_domain: String,
    /// What a receipt update signature is over (a wire identifier).
    pub receipt_domain: String,
    /// The schema name of the chat policy store.
    pub policy_schema: String,
    /// The macOS distributed notification the CLI posts to wake the hub.
    pub control_notification: String,
    /// The name the hub registers under in Claude's peer registry (default `RightKit`).
    pub hub_name: String,
    /// The `entrypoint` the hub registers with.
    pub hub_entrypoint: String,
    /// The `entrypoint` prefix by which the bridge recognises its own registry files.
    pub entrypoint_prefix: String,
    /// The prefix of session ids and reply-route seeds the hub makes (`<prefix>-hub-...`).
    pub session_prefix: String,
    /// The folder name of the reply sockets under the temporary directory.
    pub reply_dir_name: String,
    /// The `clientType` the bridge announces to Codex's app server.
    pub client_type: String,
    /// The version written into install records and the hub's registration.
    pub app_version: String,
    /// Cross-user bridge (Linux): the root-owned config folder.
    pub bridge_config_dir: String,
    /// Cross-user bridge (Linux): the socket the engine user listens on.
    pub bridge_socket: String,
    /// Cross-user bridge (Linux): the command the chat user runs.
    pub bridge_cli: String,
}

impl Default for Brand {
    fn default() -> Self {
        Brand {
            product: "RightKit".to_string(),
            cli: "rightkit".to_string(),
            cli_mac_path: "/Applications/RightKit.app/Contents/Helpers/rightkit".to_string(),
            cli_windows_path: "$env:LOCALAPPDATA\\Programs\\RightKit\\Helpers\\rightkit.exe"
                .to_string(),
            plugin_id: "pulse".to_string(),
            launcher: "run-pulse.exe".to_string(),
            skill_name: "pulse-chat".to_string(),
            legacy_skill_name: "pulse-bridge".to_string(),
            manifest_name: ".pulse-chat.json".to_string(),
            legacy_manifest_name: ".pulse-bridge.json".to_string(),
            skill_markdown: String::new(),
            hooks_backup_suffix: "pulse-bak".to_string(),
            envelope_domain: "pulse-chat/envelope/v1".to_string(),
            receipt_domain: "pulse-chat-receipt-v1".to_string(),
            policy_schema: "pulse-chat-policy".to_string(),
            control_notification: "dev.orthic.pulse.chat.control".to_string(),
            hub_name: "RightKit".to_string(),
            hub_entrypoint: "pulse-hub".to_string(),
            entrypoint_prefix: "pulse".to_string(),
            session_prefix: "pulse".to_string(),
            reply_dir_name: "pulse-chat".to_string(),
            client_type: "pulse".to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            bridge_config_dir: "/etc/pulse".to_string(),
            bridge_socket: "/run/pulse-chat/bridge.sock".to_string(),
            bridge_cli: "/usr/local/lib/pulse/pulse".to_string(),
        }
    }
}

impl Brand {
    /// `<product> Chat`, the label on the text a chat receives.
    pub fn chat_label(&self) -> String {
        format!("{} Chat", self.product)
    }

    /// `<product> chat` in lower case, the marker a delivered message is found by in a transcript.
    pub fn chat_marker(&self) -> String {
        format!("{} chat", self.product.to_lowercase())
    }
}

static BRAND: OnceLock<Brand> = OnceLock::new();

/// Installs the app's brand. Only the first call takes effect; returns `false` otherwise (also when
/// the bridge already read the brand, which fixes the default).
pub fn set_brand(brand: Brand) -> bool {
    BRAND.set(brand).is_ok()
}

/// The installed brand, or `Default::default`.
pub fn brand() -> &'static Brand {
    BRAND.get_or_init(Brand::default)
}

/// `text` with `@cli@` and `@product@` replaced by the brand's command and display name. The
/// markers are not format arguments, so they can sit inside a `format!` string.
pub fn branded(text: &str) -> String {
    let brand = brand();
    text.replace("@cli@", &brand.cli)
        .replace("@product@", &brand.product)
}

/// The product name for messages: [`Brand::product`].
pub fn product() -> &'static str {
    &brand().product
}
