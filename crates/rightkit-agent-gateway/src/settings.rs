//! The remote tools' settings. The owner app owns them (Preferences on the Mac, the pill settings on
//! Windows) and publishes them in `owner app-state.json` under `settings`; `pulse settings set`
//! changes the ones that are not owner-only like any other. This reads the last published values
//! for those. A missing file, an unreadable file or a missing key is the default, and the default
//! is off.
//!
//! The owner-only security settings (`PROTECTED_KEYS`) are different: `owner app-state.json` is a file
//! any process of the user can write, so the engine keeps its own copy of them
//! (`protected_store.rs`, in the OS credential store) and `load` takes them from there, never from
//! the file, which may still show them. A process that is not the engine asks the engine. When the
//! engine's copy cannot be read, every protected gate is off and an unlock is required.

use super::{Refusal, Result};
use serde_json::Value;

pub const KEY_TOOLS: &str = "remoteTools";
/// The ChatGPT gateway's switch, needed in addition to `remoteTools`.
pub const KEY_GATEWAY: &str = "remoteGateway";
pub const KEY_ROOTS: &str = "remoteRoots";
pub const KEY_CHAT_READ: &str = "remoteChatRead";
/// "All chats" mode for the owner's own paired phone (#579 D9): the phone's `chat.read` may read
/// every chat on this computer. On by default (a missing value is on, only a real `false` turns it
/// off); it never applies to a guest, the ChatGPT gateway or an agent tool, which read only the
/// chats `remoteChatRead` selects. Owner-only.
pub const KEY_PHONE_CHAT_READ_ALL: &str = "phoneChatReadAll";
pub const KEY_READ_MAX_BYTES: &str = "remoteReadMaxBytes";
pub const KEY_FIND_MAX_RESULTS: &str = "remoteFindMaxResults";
pub const KEY_CHAT_READ_MAX_TURNS: &str = "remoteChatReadMaxTurns";
pub const KEY_CHAT_READ_MAX_BYTES: &str = "remoteChatReadMaxBytes";
pub const KEY_IMAGE_MAX_BYTES: &str = "remoteImageMaxBytes";
/// The document tools' switch (`docs.read`), needed in addition to `remoteTools`.
pub const KEY_DOCS: &str = "remoteDocs";
pub const KEY_DOCS_MAX_BYTES: &str = "remoteDocsMaxBytes";
/// The file-changing tools' switch (`files.patch`, `files.manage`), needed in addition to
/// `remoteTools`. Off by default.
pub const KEY_FILES_WRITE: &str = "remoteFilesWrite";
/// The terminal and process tools' switch (`term.*`, `procs.*`), needed in addition to
/// `remoteTools`. Off by default.
pub const KEY_TERMINALS: &str = "remoteTerminals";
/// A standing policy for the terminal family: no per-session prompt. Off by default; the 30-minute
/// idle expiry of every session still applies.
pub const KEY_TERM_STANDING: &str = "remoteTermStanding";
/// The user's command policy for `term.exec` (`term.rs` `check_policy`): programs it may start. A
/// list of program names (or leading words such as `git status`); empty (the default) allows every
/// program. A command line whose every program is not listed is refused (`command_not_allowed`).
pub const KEY_TERM_ALLOW: &str = "remoteTermAllow";
/// Programs `term.exec` refuses (`command_denied`); empty by default.
pub const KEY_TERM_DENY: &str = "remoteTermDeny";
/// The clipboard read tool's switch (`clipboard.read`), needed in addition to `remoteTools` and the
/// screen disclosure grant. Off by default.
pub const KEY_CLIPBOARD: &str = "remoteClipboard";
/// The master read-only switch: while on, the ChatGPT gateway refuses every tool that changes this
/// computer, runs a command, sends a message, starts or stops a chat, or acts on the screen
/// (`read_only_tool`). Off by default.
pub const KEY_READ_ONLY: &str = "remoteReadOnly";
/// Membrane's ChatGPT and Codex history index: owner-only, it reads the user's chats.
pub const KEY_LEDGER_TRANSCRIPTS: &str = "ledgerTranscriptSearch";
/// The history-share grant: while on, this computer answers a paired computer's
/// `ledger.history` with redacted snippets of the chats `remoteChatRead` selects. Owner-only; off by
/// default.
pub const KEY_LEDGER_SHARE: &str = "ledgerTranscriptShare";
/// `screen.observe`'s switch, needed in addition to `remoteTools`. Off by default.
pub const KEY_SCREEN_OBSERVE: &str = "remoteScreenObserve";
/// `screen.act`'s own tier-3 switch, needed in addition to `remoteTools`. Off by default.
pub const KEY_SCREEN_ACT: &str = "remoteScreenAct";
/// The apps no remote tool may launch, observe or act on: a bundle id on macOS, an
/// AppUserModelID, an exe name or the full path of an exe on Windows. Empty by default. It replaces
/// the old `remoteLaunchApps` and `remoteScreenApps` allowlists (the owner, 2026-10-17: a block list,
/// not an allow list); the old keys are no longer read.
pub const KEY_BLOCKED_APPS: &str = "remoteBlockedApps";
/// The user's explicit disclosure grant for the screen tools: what an approved window shows
/// (text, values, a screenshot) goes to the caller. Off by default.
pub const KEY_SCREEN_DISCLOSURE: &str = "remoteScreenDisclosure";
/// `apps.launch`'s switch, needed in addition to `remoteTools`. Off by default.
pub const KEY_LAUNCH: &str = "remoteLaunch";
/// `media.control`'s switch, needed in addition to `remoteTools`. Off by default.
pub const KEY_MEDIA: &str = "remoteMedia";
/// Form fill through `screen.act`, needed in addition to its own switches. Off by default.
pub const KEY_FORM_FILL: &str = "remoteFormFill";
/// Apps (an app id, or `app::title fragment` for a site) where the ChatGPT gateway's non-sensitive
/// form fill needs no question. The local agent and the owner are never asked.
pub const KEY_FORM_FILL_STANDING: &str = "remoteFormFillStanding";
/// The user's own details for form fill, each `field=value` (`name`, `email`, `phone`,
/// `address`). Typed by the user in settings; never read from anywhere else.
pub const KEY_FORM_FILL_PROFILE: &str = "remoteFormFillProfile";
/// `login.fill`: the host app types a saved login into the user's browser after a
/// native confirmation; the agent never sees the secret. Off by default. Owner-only through the
/// `remoteFormFill` prefix.
pub const KEY_FORM_FILL_LOGINS: &str = "remoteFormFillLogins";
/// The password managers the user opted into, one `name=chord` line each (the key chord that
/// starts that manager's own autofill in the browser, for example `nordpass=cmd+shift+l`). Empty by
/// default: no manager is used until the user lists it. Owner-only through the prefix.
pub const KEY_FORM_FILL_LOGIN_MANAGERS: &str = "remoteFormFillLoginManagers";
/// `payment.fill`: the host app types an approved virtual card into a checkout.
/// Off by default. Owner-only through the prefix.
pub const KEY_FORM_FILL_PAYMENTS: &str = "remoteFormFillPayments";
/// The largest purchase `payment.fill` approves without asking, across all cards, in cents (the
/// owner's number, shown in Settings; default `DEFAULT_PAYMENTS_AUTO_CAP_CENTS`). It clamps every
/// card's own `autoCents`: a card item cannot raise it. Under the `remoteFormFill` prefix, so it is
/// owner-only like the other payment keys.
pub const KEY_PAYMENTS_AUTO_CAP: &str = "remoteFormFillPaymentsAutoCapCents";
/// The most agents may spend in a calendar month over all cards, in cents (default
/// `DEFAULT_PAYMENTS_MONTH_CAP_CENTS`). It clamps the month total whatever a card's `monthCents`
/// says. Owner-only through the prefix.
pub const KEY_PAYMENTS_MONTH_CAP: &str = "remoteFormFillPaymentsMonthCapCents";
/// Spot takeover (`screen.act` handover, an upstream issue): lets an agent hand the user one
/// region of the screen. Off by default. Owner-only through the `remoteScreen` prefix.
pub const KEY_SPOT_TAKEOVER: &str = "remoteScreenSpotTakeover";
/// Per-chat unlock (`unlock.rs`): an external chat must be unlocked once before the tools that
/// change this computer run for it. On by default; a missing value is on, only a real `false`
/// turns it off.
pub const KEY_CHAT_UNLOCK: &str = "chatUnlock";
/// How long an unlock lasts, in minutes; 0 (the default) is until the chat ends.
pub const KEY_CHAT_UNLOCK_MINUTES: &str = "chatUnlockMinutes";
/// The agent the engine starts may act (`agent.act`). Off by default.
pub const KEY_AGENT_ACTIONS: &str = "agentActions";
/// Agent browser logins: an agent may ask for a
/// browser session on the computer that holds the login store; the owner approves each use. Off by
/// default, owner-only.
pub const KEY_AGENT_BROWSER: &str = "agentBrowser";
/// The longest an approved agent browser session lasts, in minutes (default 60, at most 240).
pub const KEY_AGENT_BROWSER_MINUTES: &str = "agentBrowserSessionMinutes";
/// The `claude` executable the resident agent runs for `agent.request`. Empty (the default) means
/// the fixed install locations, then PATH. A path names a program the engine starts, so setting one
/// is a change that makes things less safe and needs the host app PIN.
pub const KEY_AGENT_CLI_PATH: &str = "agentCliPath";
/// The most `chatUnlockMinutes` accepted (one week).
pub const HARD_CHAT_UNLOCK_MINUTES: u64 = 10_080;
/// `agentBrowserSessionMinutes`: default and ceiling.
pub const DEFAULT_AGENT_BROWSER_MINUTES: u64 = 60;
pub const HARD_AGENT_BROWSER_MINUTES: u64 = 240;

/// Settings that gate what a remote chat or agent may do. They are owner-only: the CLI, the
/// owner app's and Windows' `hub-commands` files and any other same-user writer are refused for them
/// (`is_protected`). They change only through the owner-checked engine methods
/// `remote.settings.set` and `remote.settings.take` (`owner_settings.rs`): the hub asks, the engine
/// verifies the caller's executable and, for a change that makes a gate less safe, the host app PIN, and
/// owner app collects the change from the engine and applies it itself. docs/settings.md lists them.
///
/// This list is the source of truth. The owner app (`HubBridge.swift` `protectedKeys`), Windows
/// (`settings.rs` `PROTECTED_KEYS`) and the hub (`api.ts` `PROTECTED_KEYS`) keep copies, and
/// `scripts/check-protected-keys.mjs` fails when the four lists or their prefix rules differ.
pub const PROTECTED_KEYS: &[&str] = &[
    KEY_CHAT_UNLOCK,
    KEY_CHAT_UNLOCK_MINUTES,
    KEY_BLOCKED_APPS,
    KEY_AGENT_ACTIONS,
    KEY_AGENT_CLI_PATH,
    KEY_AGENT_BROWSER,
    KEY_AGENT_BROWSER_MINUTES,
    KEY_ROOTS,
    KEY_GATEWAY,
    KEY_CHAT_READ,
    KEY_PHONE_CHAT_READ_ALL,
    KEY_DOCS,
    KEY_READ_MAX_BYTES,
    KEY_FIND_MAX_RESULTS,
    KEY_CHAT_READ_MAX_TURNS,
    KEY_CHAT_READ_MAX_BYTES,
    KEY_IMAGE_MAX_BYTES,
    KEY_DOCS_MAX_BYTES,
    KEY_LAUNCH,
    KEY_MEDIA,
    KEY_FORM_FILL,
    KEY_FORM_FILL_STANDING,
    KEY_FORM_FILL_PROFILE,
    KEY_SCREEN_OBSERVE,
    KEY_SCREEN_ACT,
    KEY_SCREEN_DISCLOSURE,
    KEY_TERMINALS,
    KEY_TERM_STANDING,
    KEY_TERM_ALLOW,
    KEY_TERM_DENY,
    KEY_CLIPBOARD,
    KEY_READ_ONLY,
    KEY_LEDGER_TRANSCRIPTS,
    KEY_LEDGER_SHARE,
    KEY_FILES_WRITE,
    KEY_TOOLS,
];

/// Whether `key` is one of the owner-only security settings. Also true for any other
/// `remoteFormFill*` or `remoteScreen*` key, so a later addition is protected by default.
pub fn is_protected(key: &str) -> bool {
    PROTECTED_KEYS.contains(&key) || PROTECTED_PREFIXES.iter().any(|p| key.starts_with(p))
}

/// The prefixes that make any key under them owner-only (`is_protected`), so a later addition is
/// protected by default. Mirrored in `HubBridge.swift`, Windows `settings.rs` and `api.ts`.
pub const PROTECTED_PREFIXES: &[&str] = &["remoteFormFill", "remoteScreen"];

/// Defaults, as docs/settings.md states them.
pub const DEFAULT_READ_MAX_BYTES: u64 = 1024 * 1024;
pub const DEFAULT_FIND_MAX_RESULTS: u64 = 200;
pub const DEFAULT_CHAT_READ_MAX_TURNS: u64 = 50;
pub const DEFAULT_CHAT_READ_MAX_BYTES: u64 = 256 * 1024;
pub const DEFAULT_IMAGE_MAX_BYTES: u64 = 10 * 1024 * 1024;
pub const DEFAULT_DOCS_MAX_BYTES: u64 = 20 * 1024 * 1024;
/// The payment caps' defaults ($25 per purchase without asking, $100 per month, the owner
/// 2026-10-22 through the lead's placeholders). Both are the owner's to change in Settings.
pub const DEFAULT_PAYMENTS_AUTO_CAP_CENTS: u64 = 2_500;
pub const DEFAULT_PAYMENTS_MONTH_CAP_CENTS: u64 = 10_000;
/// What a payment cap may be raised to ($100,000), whatever it says.
pub const HARD_PAYMENTS_CAP_CENTS: u64 = 10_000_000;

/// What a caps setting may be raised to, whatever it says: a frame that carries a result must stay
/// under the engine contract's 16 MiB. Text grows about 6 times when JSON escapes every control
/// character and base64 a third, so a read is at most 2 MiB (12 MiB escaped).
pub const HARD_READ_MAX_BYTES: u64 = 2 * 1024 * 1024;
pub const HARD_FIND_MAX_RESULTS: u64 = 1000;
pub const HARD_CHAT_READ_MAX_TURNS: u64 = 500;
pub const HARD_CHAT_READ_MAX_BYTES: u64 = 2 * 1024 * 1024;
pub const HARD_IMAGE_MAX_BYTES: u64 = 25 * 1024 * 1024;
/// A document is read whole into memory, so its size is capped; the reply is capped separately
/// (`docs::HARD_MAX_CHARS`).
pub const HARD_DOCS_MAX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// `remoteTools`: the master switch. Off by default.
    pub enabled: bool,
    /// `remoteGateway`: lets `the gateway` answer ChatGPT at all. Off by default.
    pub gateway: bool,
    /// `remoteRoots`: the approved folders, as written. Empty by default.
    pub roots: Vec<String>,
    /// `remoteChatRead`: the chats `chat_read` may read, as `<id>` or `<kind>:<id>`. Empty by
    /// default.
    pub chat_read: Vec<String>,
    /// `phoneChatReadAll`: the owner's paired phone may read every chat. On by default; only an
    /// explicit `false` turns it off.
    pub phone_chat_read_all: bool,
    /// `ledgerTranscriptShare`: a paired computer may pull redacted history snippets of the chats
    /// `chat_read` selects (`chat::history`). Off by default; only a real `true` turns it on.
    pub ledger_share: bool,
    pub read_max_bytes: u64,
    pub find_max_results: u64,
    pub chat_read_max_turns: u64,
    pub chat_read_max_bytes: u64,
    pub image_max_bytes: u64,
    /// `remoteDocs`: lets `docs.read` open documents, needed in addition to `remoteTools`. Off by
    /// default.
    pub docs: bool,
    /// `remoteDocsMaxBytes`: the largest document file `docs.read` opens.
    pub docs_max_bytes: u64,
    /// `remoteFilesWrite`: lets `files.patch` and `files.manage` act, needed in addition to
    /// `remoteTools`. Off by default.
    pub files_write: bool,
    /// `remoteTerminals`: lets `term.*` and `procs.*` act, needed in addition to `remoteTools`.
    /// Off by default.
    pub terminals: bool,
    /// `remoteTermStanding`: the terminal family needs no per-session approval. Off by default.
    pub term_standing: bool,
    /// `remoteTermAllow`: programs `term.exec` may start; empty is no restriction.
    pub term_allow: Vec<String>,
    /// `remoteTermDeny`: programs `term.exec` refuses; empty by default.
    pub term_deny: Vec<String>,
    /// `remoteClipboard`: lets `clipboard.read` act. Off by default.
    pub clipboard: bool,
    /// `remoteReadOnly`: the gateway refuses every tool that changes things. Off by default.
    pub read_only: bool,
    /// `remoteScreenObserve`: lets `screen.observe` read approved windows, needed in addition to
    /// `remoteTools`. Off by default.
    pub screen_observe: bool,
    /// `remoteScreenAct`: lets `screen.act` act in approved windows after a local approval of
    /// each action. Off by default.
    pub screen_act: bool,
    /// `remoteBlockedApps`: the apps no remote tool may launch or drive, as written. Empty by
    /// default.
    pub blocked_apps: Vec<String>,
    /// `remoteScreenDisclosure`: the user's grant that approved windows' content leaves this
    /// computer. Off by default.
    pub screen_disclosure: bool,
    /// `remoteLaunch`: lets `apps.launch` open any app that is not blocked. Off by default.
    pub launch: bool,
    /// `remoteMedia`: lets `media.control` act. Off by default.
    pub media: bool,
    /// `remoteFormFill`: lets `screen.act` type the user's profile details. Off by default.
    pub form_fill: bool,
    /// `remoteFormFillStanding`: scope entries with a standing form-fill grant. Empty by default.
    pub form_fill_standing: Vec<String>,
    /// `remoteFormFillProfile`: `field=value` lines the user typed. Empty by default.
    pub form_fill_profile: Vec<String>,
    /// `remoteFormFillLogins`: lets `login.fill` type a saved login. Off by default.
    pub logins: bool,
    /// `remoteFormFillLoginManagers`: `name=chord` lines for the password managers the user opted
    /// into. Empty by default.
    pub login_managers: Vec<String>,
    /// `remoteFormFillPayments`: lets `payment.fill` type a saved virtual card. Off by default.
    pub payments: bool,
    /// `remoteFormFillPaymentsAutoCapCents`: no-question purchase ceiling over all cards, in cents.
    pub payments_auto_cap_cents: u64,
    /// `remoteFormFillPaymentsMonthCapCents`: month total over all cards, in cents.
    pub payments_month_cap_cents: u64,
    /// `remoteScreenSpotTakeover`: lets an agent hand the user a region of the screen. Off by default.
    pub spot_takeover: bool,
    /// `chatUnlock`: an external chat is unlocked once before effectful tools run. On by default.
    pub chat_unlock: bool,
    /// `chatUnlockMinutes`: how long an unlock lasts; 0 is until the chat ends.
    pub chat_unlock_minutes: u64,
    /// `agentActions`: the agent the engine starts may act. Off by default.
    pub agent_actions: bool,
    /// `agentBrowser`: agents may ask for an approved browser session on this computer. Off by
    /// default.
    pub agent_browser: bool,
    /// `agentBrowserSessionMinutes`: the longest such a session lasts.
    pub agent_browser_minutes: u64,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            enabled: false,
            gateway: false,
            roots: Vec::new(),
            chat_read: Vec::new(),
            phone_chat_read_all: true,
            ledger_share: false,
            read_max_bytes: DEFAULT_READ_MAX_BYTES,
            find_max_results: DEFAULT_FIND_MAX_RESULTS,
            chat_read_max_turns: DEFAULT_CHAT_READ_MAX_TURNS,
            chat_read_max_bytes: DEFAULT_CHAT_READ_MAX_BYTES,
            image_max_bytes: DEFAULT_IMAGE_MAX_BYTES,
            docs: false,
            docs_max_bytes: DEFAULT_DOCS_MAX_BYTES,
            files_write: false,
            terminals: false,
            term_standing: false,
            term_allow: Vec::new(),
            term_deny: Vec::new(),
            clipboard: false,
            read_only: false,
            screen_observe: false,
            screen_act: false,
            blocked_apps: Vec::new(),
            screen_disclosure: false,
            launch: false,
            media: false,
            form_fill: false,
            form_fill_standing: Vec::new(),
            form_fill_profile: Vec::new(),
            logins: false,
            login_managers: Vec::new(),
            payments: false,
            payments_auto_cap_cents: DEFAULT_PAYMENTS_AUTO_CAP_CENTS,
            payments_month_cap_cents: DEFAULT_PAYMENTS_MONTH_CAP_CENTS,
            spot_takeover: false,
            chat_unlock: true,
            chat_unlock_minutes: 0,
            agent_actions: false,
            agent_browser: false,
            agent_browser_minutes: DEFAULT_AGENT_BROWSER_MINUTES,
        }
    }
}

impl Settings {
    /// The effective settings: owner app's last published values for everything that is not
    /// owner-only, and the engine's own copy for the owner-only ones (`protected_store`). Nothing
    /// in `owner app-state.json` can change a protected value. The defaults (off) apply when the file
    /// is missing or unreadable; when the engine's copy cannot be read every protected gate is off
    /// and the unlock is required (fail closed), except `phoneChatReadAll`, which reads as on so the
    /// owner's own paired phone keeps reading every chat (#579 D9).
    pub fn load() -> Settings {
        let mut published = read_published();
        // Test builds only (`test-overrides`, never in a release build): the remote smoke
        // (scripts/qa/remote-smoke.mjs) switches the gates in its fixture `owner app-state.json`, with
        // no engine or PIN, so with RIGHTKIT_TEST_PROTECTED_FROM_FILE=1 the file's values stand.
        #[cfg(feature = "test-overrides")]
        if std::env::var_os("RIGHTKIT_TEST_PROTECTED_FROM_FILE").is_some_and(|v| v == "1") {
            return Settings::from_published(&published);
        }
        super::protected_store::apply(&mut published);
        Settings::from_published(&published)
    }

    /// The settings in a parsed `owner app-state.json`, as published (protected keys included).
    pub fn from_state(state: &Value) -> Settings {
        Settings::from_published(&state["settings"])
    }

    /// The settings in a `settings` object.
    pub fn from_published(published: &Value) -> Settings {
        Settings {
            agent_actions: published[KEY_AGENT_ACTIONS].as_bool() == Some(true),
            agent_browser: published[KEY_AGENT_BROWSER].as_bool() == Some(true),
            agent_browser_minutes: published[KEY_AGENT_BROWSER_MINUTES]
                .as_u64()
                .or_else(|| {
                    published[KEY_AGENT_BROWSER_MINUTES]
                        .as_f64()
                        .filter(|f| f.is_finite() && *f >= 1.0 && f.fract() == 0.0)
                        .map(|f| f as u64)
                })
                .map_or(DEFAULT_AGENT_BROWSER_MINUTES, |n| {
                    n.clamp(1, HARD_AGENT_BROWSER_MINUTES)
                }),
            // Only a real `true` turns it on.
            enabled: published[KEY_TOOLS].as_bool() == Some(true),
            gateway: published[KEY_GATEWAY].as_bool() == Some(true),
            roots: text_list(&published[KEY_ROOTS]),
            chat_read: text_list(&published[KEY_CHAT_READ]),
            // Only an explicit `false` turns it off: an untouched install reads as today.
            phone_chat_read_all: published[KEY_PHONE_CHAT_READ_ALL].as_bool() != Some(false),
            ledger_share: published[KEY_LEDGER_SHARE].as_bool() == Some(true),
            read_max_bytes: cap(
                &published[KEY_READ_MAX_BYTES],
                DEFAULT_READ_MAX_BYTES,
                HARD_READ_MAX_BYTES,
            ),
            find_max_results: cap(
                &published[KEY_FIND_MAX_RESULTS],
                DEFAULT_FIND_MAX_RESULTS,
                HARD_FIND_MAX_RESULTS,
            ),
            chat_read_max_turns: cap(
                &published[KEY_CHAT_READ_MAX_TURNS],
                DEFAULT_CHAT_READ_MAX_TURNS,
                HARD_CHAT_READ_MAX_TURNS,
            ),
            chat_read_max_bytes: cap(
                &published[KEY_CHAT_READ_MAX_BYTES],
                DEFAULT_CHAT_READ_MAX_BYTES,
                HARD_CHAT_READ_MAX_BYTES,
            ),
            image_max_bytes: cap(
                &published[KEY_IMAGE_MAX_BYTES],
                DEFAULT_IMAGE_MAX_BYTES,
                HARD_IMAGE_MAX_BYTES,
            ),
            docs: published[KEY_DOCS].as_bool() == Some(true),
            docs_max_bytes: cap(
                &published[KEY_DOCS_MAX_BYTES],
                DEFAULT_DOCS_MAX_BYTES,
                HARD_DOCS_MAX_BYTES,
            ),
            files_write: published[KEY_FILES_WRITE].as_bool() == Some(true),
            terminals: published[KEY_TERMINALS].as_bool() == Some(true),
            term_standing: published[KEY_TERM_STANDING].as_bool() == Some(true),
            term_allow: text_list(&published[KEY_TERM_ALLOW]),
            term_deny: text_list(&published[KEY_TERM_DENY]),
            clipboard: published[KEY_CLIPBOARD].as_bool() == Some(true),
            read_only: published[KEY_READ_ONLY].as_bool() == Some(true),
            screen_observe: published[KEY_SCREEN_OBSERVE].as_bool() == Some(true),
            screen_act: published[KEY_SCREEN_ACT].as_bool() == Some(true),
            blocked_apps: text_list(&published[KEY_BLOCKED_APPS]),
            screen_disclosure: published[KEY_SCREEN_DISCLOSURE].as_bool() == Some(true),
            launch: published[KEY_LAUNCH].as_bool() == Some(true),
            media: published[KEY_MEDIA].as_bool() == Some(true),
            form_fill: published[KEY_FORM_FILL].as_bool() == Some(true),
            form_fill_standing: text_list(&published[KEY_FORM_FILL_STANDING]),
            form_fill_profile: text_list(&published[KEY_FORM_FILL_PROFILE]),
            logins: published[KEY_FORM_FILL_LOGINS].as_bool() == Some(true),
            login_managers: text_list(&published[KEY_FORM_FILL_LOGIN_MANAGERS]),
            payments: published[KEY_FORM_FILL_PAYMENTS].as_bool() == Some(true),
            payments_auto_cap_cents: payment_cap(
                &published[KEY_PAYMENTS_AUTO_CAP],
                DEFAULT_PAYMENTS_AUTO_CAP_CENTS,
            ),
            payments_month_cap_cents: payment_cap(
                &published[KEY_PAYMENTS_MONTH_CAP],
                DEFAULT_PAYMENTS_MONTH_CAP_CENTS,
            ),
            spot_takeover: published[KEY_SPOT_TAKEOVER].as_bool() == Some(true),
            // Fails closed: only an explicit `false` turns the unlock off.
            chat_unlock: published[KEY_CHAT_UNLOCK].as_bool() != Some(false),
            chat_unlock_minutes: published[KEY_CHAT_UNLOCK_MINUTES]
                .as_u64()
                .or_else(|| {
                    published[KEY_CHAT_UNLOCK_MINUTES]
                        .as_f64()
                        .filter(|f| f.is_finite() && *f >= 0.0 && f.fract() == 0.0)
                        .map(|f| f as u64)
                })
                .map_or(0, |n| n.min(HARD_CHAT_UNLOCK_MINUTES)),
        }
    }
}

/// A payment cap in cents: a whole number (0 allowed, which means nothing is approved) up to
/// `HARD_PAYMENTS_CAP_CENTS`. A missing value is the default (the low one); a present but negative,
/// fractional or non-numeric value is 0, which refuses. Never unlimited.
fn payment_cap(value: &Value, default: u64) -> u64 {
    value
        .as_u64()
        .or_else(|| {
            value
                .as_f64()
                .filter(|f| f.is_finite() && *f >= 0.0 && f.fract() == 0.0)
                .map(|f| f as u64)
        })
        // Absent: the default. Present but not a whole non-negative number: 0, which refuses.
        .map_or(if value.is_null() { default } else { 0 }, |n| {
            n.min(HARD_PAYMENTS_CAP_CENTS)
        })
}

/// The `settings` object of owner app's published `owner app-state.json`; an empty object when the file
/// is missing, unreadable or not the host app's.
pub fn read_published() -> Value {
    let empty = || Value::Object(serde_json::Map::new());
    let settings = rightkit_app_host::settings();
    if settings.is_object() {
        settings
    } else {
        empty()
    }
}

impl Settings {
    /// What a call from the ChatGPT gateway needs before anything else: `remoteGateway` on, and
    /// `remoteTools` on as well. `gateway_off` and `remote_off` are the stable codes.
    pub fn require_gateway(&self) -> Result<()> {
        if !self.gateway {
            return Err(Refusal::new(
                "gateway_off",
                "the ChatGPT gateway is turned off in the app's settings (remoteGateway)",
            ));
        }
        if !self.enabled {
            return Err(Refusal::new(
                "remote_off",
                "remote tools are turned off in the app's settings (remoteTools)",
            ));
        }
        Ok(())
    }
}

impl Settings {
    /// What `docs.read` needs before anything else: `remoteTools` on, and `remoteDocs` on as well.
    /// `remote_off` and `docs_off` are the stable codes.
    pub fn require_docs(&self) -> Result<()> {
        if !self.enabled {
            return Err(Refusal::new(
                "remote_off",
                "remote tools are turned off in the app's settings (remoteTools)",
            ));
        }
        if !self.docs {
            return Err(Refusal::new(
                "docs_off",
                "document tools are turned off in the app's settings (remoteDocs)",
            ));
        }
        Ok(())
    }
}

impl Settings {
    /// What `files.patch` and `files.manage` need before anything else: `remoteTools` on, and
    /// `remoteFilesWrite` on as well. `remote_off` and `files_write_off` are the stable codes.
    pub fn require_files_write(&self) -> Result<()> {
        if !self.enabled {
            return Err(Refusal::new(
                "remote_off",
                "remote tools are turned off in the app's settings (remoteTools)",
            ));
        }
        if !self.files_write {
            return Err(Refusal::new(
                "files_write_off",
                "changing files is turned off in the app's settings (remoteFilesWrite)",
            ));
        }
        Ok(())
    }

    /// What `term.*` and `procs.*` need before anything else: `remoteTools` on, and
    /// `remoteTerminals` on as well. `remote_off` and `terminals_off` are the stable codes.
    pub fn require_terminals(&self) -> Result<()> {
        if !self.enabled {
            return Err(Refusal::new(
                "remote_off",
                "remote tools are turned off in the app's settings (remoteTools)",
            ));
        }
        if !self.terminals {
            return Err(Refusal::new(
                "terminals_off",
                "terminals are turned off in the app's settings (remoteTerminals)",
            ));
        }
        Ok(())
    }
}

impl Settings {
    /// What `apps.launch` needs before anything else: `remoteTools` and `remoteLaunch`. The codes
    /// are `remote_off` and `launch_off`; an app on `remoteBlockedApps` is `app_blocked`.
    pub fn require_launch(&self) -> Result<()> {
        if !self.enabled {
            return Err(Refusal::new(
                "remote_off",
                "remote tools are turned off in the app's settings (remoteTools)",
            ));
        }
        if !self.launch {
            return Err(Refusal::new(
                "launch_off",
                "opening apps is turned off in the app's settings (remoteLaunch)",
            ));
        }
        Ok(())
    }

    /// What `media.control` needs: `remoteTools` and `remoteMedia` (`remote_off`, `media_off`).
    pub fn require_media(&self) -> Result<()> {
        if !self.enabled {
            return Err(Refusal::new(
                "remote_off",
                "remote tools are turned off in the app's settings (remoteTools)",
            ));
        }
        if !self.media {
            return Err(Refusal::new(
                "media_off",
                "media control is turned off in the app's settings (remoteMedia)",
            ));
        }
        Ok(())
    }

    /// The profile value for a field (`name`, `email`, `phone`, `address`), from the lines the
    /// user typed as `field=value`; the first line for a field wins.
    pub fn profile_value(&self, field: &str) -> Option<&str> {
        self.form_fill_profile.iter().find_map(|line| {
            let (key, value) = line.split_once('=')?;
            let value = value.trim();
            (key.trim().eq_ignore_ascii_case(field) && !value.is_empty()).then_some(value)
        })
    }
}

impl Settings {
    /// What `login.fill` needs: `remoteTools` and `remoteFormFillLogins`. The stable codes are
    /// `remote_off` and `logins_off`.
    pub fn require_logins(&self) -> Result<()> {
        if !self.enabled {
            return Err(Refusal::new(
                "remote_off",
                "remote tools are turned off in the app's settings (remoteTools)",
            ));
        }
        if !self.logins {
            return Err(Refusal::new(
                "logins_off",
                "saved logins are turned off in the app's settings (remoteFormFillLogins)",
            ));
        }
        Ok(())
    }

    /// What `payment.fill` needs: `remoteTools` and `remoteFormFillPayments`. The stable codes are
    /// `remote_off` and `payments_off`.
    pub fn require_payments(&self) -> Result<()> {
        if !self.enabled {
            return Err(Refusal::new(
                "remote_off",
                "remote tools are turned off in the app's settings (remoteTools)",
            ));
        }
        if !self.payments {
            return Err(Refusal::new(
                "payments_off",
                "saved cards are turned off in the app's settings (remoteFormFillPayments)",
            ));
        }
        Ok(())
    }
}

impl Settings {
    /// What `screen.observe` needs before anything else: `remoteTools`, `remoteScreenObserve` and
    /// the disclosure grant (`remoteScreenDisclosure`). The stable codes are `remote_off`,
    /// `screen_observe_off` and `screen_disclosure_missing`. No window list is needed: any window
    /// of an app that is not blocked (`remoteBlockedApps`) is in reach.
    pub fn require_screen_observe(&self) -> Result<()> {
        self.require_screen(
            self.screen_observe,
            "screen_observe_off",
            KEY_SCREEN_OBSERVE,
        )
    }

    /// `clipboard.read`: `remoteTools`, `remoteClipboard` and the disclosure grant
    /// (`remote_off`, `clipboard_off`, `screen_disclosure_missing`).
    pub fn require_clipboard(&self) -> Result<()> {
        self.require_screen(self.clipboard, "clipboard_off", KEY_CLIPBOARD)
    }

    /// The same for `screen.act` with its own switch (`remoteScreenAct`, `screen_act_off`).
    pub fn require_screen_act(&self) -> Result<()> {
        self.require_screen(self.screen_act, "screen_act_off", KEY_SCREEN_ACT)
    }

    fn require_screen(&self, on: bool, off_code: &'static str, key: &str) -> Result<()> {
        if !self.enabled {
            return Err(Refusal::new(
                "remote_off",
                "remote tools are turned off in the app's settings (remoteTools)",
            ));
        }
        if !on {
            return Err(Refusal::new(
                off_code,
                format!("this screen tool is turned off in the app's settings ({key})"),
            ));
        }
        if !self.screen_disclosure {
            return Err(Refusal::new(
                "screen_disclosure_missing",
                "the screen disclosure grant is not given in the app's settings (remoteScreenDisclosure)",
            ));
        }
        Ok(())
    }

    /// True when `app_id` (or the app a launch names) is on `remoteBlockedApps`. An entry matches
    /// an id equal to it (case-insensitive) or, when either side is a path, the same file name, so
    /// a blocked `C:\Program Files\App\app.exe` also blocks the window of `app.exe`.
    pub fn app_blocked(&self, app_id: &str) -> bool {
        let id = app_id.trim();
        if id.is_empty() {
            return false;
        }
        let file = |s: &str| s.rsplit(['/', '\\']).next().unwrap_or(s).to_lowercase();
        self.blocked_apps.iter().any(|entry| {
            entry.eq_ignore_ascii_case(id)
                || ((entry.contains(['/', '\\']) || id.contains(['/', '\\']))
                    && file(entry) == file(id))
        })
    }
}

/// The text items of a published list; anything else is no list.
fn text_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Apps no remote tool ever launches, observes or drives, whatever `remoteBlockedApps` says: the host app
/// itself, System Settings (macOS, and Windows Settings: `ms-settings`, `SystemSettings.exe`), Windows
/// Security (`SecHealthUI.exe`) and the system's security and permission prompts (on a Mac
/// SecurityAgent and the LocalAuthentication UI process, `coreauthd`). Not a setting; the
/// owner's blocklist only adds to it. The crate's own list (`rightkit-automation`) says the same
/// for acting; this also covers launching and listing.
pub fn hard_floor(app_id: &str) -> bool {
    let id = app_id.trim().to_lowercase();
    let file = id.rsplit(['/', '\\']).next().unwrap_or(&id);
    let config = super::config::config();
    config
        .protected_app_prefixes
        .iter()
        .any(|prefix| id.starts_with(&prefix.to_lowercase()))
        || config
            .protected_exe_prefix
            .as_ref()
            .is_some_and(|prefix| file.starts_with(&prefix.to_lowercase()) && file.ends_with(".exe"))
        // Windows Settings: the `ms-settings:` scheme and its AppUserModelID.
        || id.starts_with("ms-settings")
        || id.contains("immersivecontrolpanel")
        || matches!(
            id.as_str(),
            "com.apple.systempreferences"
                | "com.apple.systemsettings"
                | "com.apple.keychainaccess"
                | "com.apple.securityagent"
                | "com.apple.coreservices.uiagent"
                | "com.apple.accessibility.universalaccessauthwarn"
                // The system's Touch ID and password prompt (LocalAuthentication's UI process).
                | "com.apple.localauthentication.uiagent"
                | "com.apple.coreauthd"
                | "coreauthd"
                | "securityagent"
        )
        || matches!(
            file,
            "systemsettings.exe"
                | "securityhealthsystray.exe"
                | "consent.exe"
                | "credentialuibroker.exe"
                | "logonui.exe"
                | "sechealthui.exe"
                | "coreauthd"
                | "securityagent"
        )
}

/// A positive whole number up to `hard`; the default for anything else (absent, zero, negative,
/// fractional, not a number).
fn cap(value: &Value, default: u64, hard: u64) -> u64 {
    let number = value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|f| f.is_finite() && *f >= 1.0 && f.fract() == 0.0)
            .map(|f| f as u64)
    });
    match number {
        Some(n) if n >= 1 => n.min(hard),
        _ => default,
    }
}
