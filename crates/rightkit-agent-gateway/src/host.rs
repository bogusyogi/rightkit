//! The seam between the gateway and the app that hosts it.
//!
//! The gateway owns the request gate, the pairing registry, the audit log and the approval flow,
//! and keeps their tables (`agents`, `agent_accounts`, `agent_audit`, the approval table) as SQL
//! over a [`rightkit_db::rusqlite::Connection`] the host hands it through
//! [`ConnAccess`](crate::access::ConnAccess). What it cannot own is the host's own mail store,
//! mutation guard, settings and file-scan policy: those are the methods of [`GatewayHost`], which
//! the host installs once at start with [`set_host`]. Until then every method fails closed or
//! answers neutrally.

use rightkit_db::rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub use rightkit_mail::search::SearchCursor;

/// How dangerous a guarded write is. Rising order of blast radius.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationClass {
    /// Trivially reversible: mark read or unread, star, label.
    SafeMutation,
    /// Reversible within a window: archive, trash, send with undo.
    ReversibleMutation,
}

/// Verdict of the host's file scanner for an exported file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileScan {
    /// Scanned and clean.
    Clean,
    /// Scanned and a threat was found.
    Threat,
    /// No verdict could be produced on this platform or right now.
    Unavailable,
}

/// An account as the `accounts` method lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountEntry {
    pub id: String,
    pub display_name: String,
}

/// A label of the label catalog with its counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelEntry {
    pub id: String,
    pub name: String,
    /// `system` or `user`.
    pub label_type: String,
    pub total: i64,
    pub unread: i64,
}

/// One thread of a search result page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadHit {
    pub account_id: String,
    pub thread_id: String,
    pub subject: String,
    pub from_name: String,
    pub snippet: String,
    pub matching_message_excerpt: Option<String>,
    pub participants: Vec<String>,
    pub attachment_names: Vec<String>,
    pub labels: Vec<String>,
    pub unread: bool,
    pub starred: bool,
    pub has_attachments: bool,
    pub last_message_at: i64,
    pub message_count: i64,
    pub match_count: i64,
}

/// One message of a thread, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadMessage {
    pub message_id: String,
    pub from_name: String,
    pub from_addr: String,
    pub to_addrs: String,
    pub cc_addrs: String,
    pub reply_to: Option<String>,
    pub subject: String,
    pub internal_date: i64,
    pub unread: bool,
    pub has_attachments: bool,
    pub body: String,
    pub is_html: bool,
}

/// What the gateway needs from the hosting app. Every method that touches the mail store takes
/// the connection the gateway is already running on, so a call stays inside the gateway's own
/// transaction and lock.
pub trait GatewayHost: Send + Sync {
    /// The product's display name, used in the sentences an agent and the user read.
    fn product_name(&self) -> &str;

    /// Record and permit a guarded write of `class` (`action` names it; `account` is the
    /// affected account). Refuse by returning an error.
    fn guard(
        &self,
        conn: &Connection,
        class: MutationClass,
        action: &str,
        account: Option<&str>,
    ) -> Result<(), String>;

    /// Ids of the messages of one thread, empty when the thread is not in this account.
    fn thread_message_ids(
        &self,
        conn: &Connection,
        account: &str,
        thread: &str,
    ) -> Result<Vec<String>, String>;
    /// Add a label to a thread in the local mirror.
    fn local_add_label(
        &self,
        conn: &Connection,
        account: &str,
        thread: &str,
        label: &str,
    ) -> Result<(), String>;
    /// Remove a label from a thread in the local mirror.
    fn local_drop_label(
        &self,
        conn: &Connection,
        account: &str,
        thread: &str,
        label: &str,
    ) -> Result<(), String>;
    /// Mark every message of a thread unread in the local mirror.
    fn set_thread_unread(
        &self,
        conn: &Connection,
        account: &str,
        thread: &str,
    ) -> Result<(), String>;
    /// Mark every message of a thread read in the local mirror.
    fn set_thread_read(&self, conn: &Connection, account: &str, thread: &str)
        -> Result<(), String>;
    /// Set or clear the star on a thread in the local mirror.
    fn set_thread_starred(
        &self,
        conn: &Connection,
        account: &str,
        thread: &str,
        on: bool,
    ) -> Result<(), String>;

    /// Read a persisted app setting.
    fn get_setting(&self, conn: &Connection, key: &str) -> Result<Option<String>, String>;
    /// Persist an app setting.
    fn set_setting(&self, conn: &Connection, key: &str, value: &str) -> Result<(), String>;

    /// Strip HTML to visible text.
    fn html_to_text(&self, html: &str) -> String;

    /// The app's data folder; the export folder lives under it.
    fn data_dir(&self) -> PathBuf;
    /// Scan an exported file. The default says nothing could be scanned.
    fn scan_file(&self, _path: &Path) -> FileScan {
        FileScan::Unavailable
    }

    /// Every account, for the `accounts` method.
    fn list_accounts(&self, conn: &Connection) -> Result<Vec<AccountEntry>, String>;
    /// One keyset page of a scoped search, newest first.
    fn search_threads(
        &self,
        conn: &Connection,
        query: &str,
        account: &str,
        before: Option<&SearchCursor>,
        limit: usize,
    ) -> Result<Vec<ThreadHit>, String>;
    /// Every message of a thread, oldest first, each with its best body.
    fn thread_messages(
        &self,
        conn: &Connection,
        account: &str,
        thread: &str,
    ) -> Result<Vec<ThreadMessage>, String>;
    /// The account's label catalog with counts.
    fn label_catalog(&self, conn: &Connection, account: &str) -> Result<Vec<LabelEntry>, String>;
}

/// The host used before [`set_host`]: nothing is stored, every call fails.
struct Unset;

const UNSET: &str = "the agent gateway host is not installed";

impl GatewayHost for Unset {
    fn product_name(&self) -> &str {
        "the app"
    }
    fn guard(
        &self,
        _: &Connection,
        _: MutationClass,
        _: &str,
        _: Option<&str>,
    ) -> Result<(), String> {
        Err(UNSET.into())
    }
    fn thread_message_ids(&self, _: &Connection, _: &str, _: &str) -> Result<Vec<String>, String> {
        Err(UNSET.into())
    }
    fn local_add_label(&self, _: &Connection, _: &str, _: &str, _: &str) -> Result<(), String> {
        Err(UNSET.into())
    }
    fn local_drop_label(&self, _: &Connection, _: &str, _: &str, _: &str) -> Result<(), String> {
        Err(UNSET.into())
    }
    fn set_thread_unread(&self, _: &Connection, _: &str, _: &str) -> Result<(), String> {
        Err(UNSET.into())
    }
    fn set_thread_read(&self, _: &Connection, _: &str, _: &str) -> Result<(), String> {
        Err(UNSET.into())
    }
    fn set_thread_starred(&self, _: &Connection, _: &str, _: &str, _: bool) -> Result<(), String> {
        Err(UNSET.into())
    }
    fn get_setting(&self, _: &Connection, _: &str) -> Result<Option<String>, String> {
        Err(UNSET.into())
    }
    fn set_setting(&self, _: &Connection, _: &str, _: &str) -> Result<(), String> {
        Err(UNSET.into())
    }
    fn html_to_text(&self, html: &str) -> String {
        html.to_string()
    }
    fn data_dir(&self) -> PathBuf {
        std::env::temp_dir().join("rightkit-agent-gateway-unset")
    }
    fn list_accounts(&self, _: &Connection) -> Result<Vec<AccountEntry>, String> {
        Err(UNSET.into())
    }
    fn search_threads(
        &self,
        _: &Connection,
        _: &str,
        _: &str,
        _: Option<&SearchCursor>,
        _: usize,
    ) -> Result<Vec<ThreadHit>, String> {
        Err(UNSET.into())
    }
    fn thread_messages(
        &self,
        _: &Connection,
        _: &str,
        _: &str,
    ) -> Result<Vec<ThreadMessage>, String> {
        Err(UNSET.into())
    }
    fn label_catalog(&self, _: &Connection, _: &str) -> Result<Vec<LabelEntry>, String> {
        Err(UNSET.into())
    }
}

static HOST: OnceLock<Box<dyn GatewayHost>> = OnceLock::new();
static UNSET_HOST: Unset = Unset;

/// Install the host. The first call wins; later calls are ignored and return `false`.
pub fn set_host(host: Box<dyn GatewayHost>) -> bool {
    HOST.set(host).is_ok()
}

/// The installed host, or the fail-closed placeholder before [`set_host`].
pub fn host() -> &'static dyn GatewayHost {
    match HOST.get() {
        Some(h) => h.as_ref(),
        None => &UNSET_HOST,
    }
}

/// The product's display name for user-facing sentences.
pub fn product() -> &'static str {
    host().product_name()
}
