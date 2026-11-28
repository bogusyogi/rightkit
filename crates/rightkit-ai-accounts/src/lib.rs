//! AI assistant account management for Claude on the desktop.
//!
//! - [`desktop`]: Claude Desktop login slots (save, switch, restore), per-account instances started
//!   with their own data folder, chat moves between accounts, and the account watch.
//! - [`sync`]: chat sync and mirroring between Claude Desktop accounts, with backups and a registry.
//! - [`cli_accounts`]: one `CLAUDE_CONFIG_DIR` folder per command-line account.
//! - [`book`]: the account book, each account's last usage reading and the hub's Accounts list.
//! - [`switcher`]: the model of a Desktop account switcher card (slots, phases, list parsing).
//! - [`names`]: the names the owner gives each account, merged across devices.
//! - [`desktop_roots`]: every Claude Desktop data folder on this computer.
//! - [`desktop_usage_cache`]: the usage responses Claude Desktop cached on Windows (Chromium
//!   block-file cache), dated and decoded.
//!
//! State lives under the host app's state folder ([`rightkit_app_host::state_root`]); without one
//! the account features answer "unsupported". The app installs its host with
//! [`rightkit_app_host::install`].

pub mod book;
pub mod cli_accounts;
pub mod desktop;
pub mod desktop_roots;
pub mod desktop_usage_cache;
pub mod names;
pub mod switcher;
pub mod sync;
