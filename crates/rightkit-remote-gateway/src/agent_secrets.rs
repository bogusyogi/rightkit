//! Agent logins and agent payments (an upstream issue and #346): `login.fill` and `payment.fill`.
//!
//! An agent that has to sign in or pay never sees the secret. It asks the host app to type one saved
//! item, one field at a time, into the field that has focus in the user's own browser. the host app reads
//! the page's address itself (never the agent's claim), checks it against the host the user saved
//! the item for, asks the owner natively, and types the value; the answer says `filled` and the
//! field's name, never a value. Decided by the owner, 2026-10-17 (docs/agent-credentials-payments.md,
//! docs/contracts/remote.md R10, R12): "The security has to be top-owner app"; password managers are an opt-in, a
//! The host app secure box covers what they do not, every sensitive step is confirmed with Touch ID,
//! Windows Hello or the PIN, an auto-approve limit he sets ("buy anything under $50 or $100"),
//! tokenized or virtual cards, US cards only.
//!
//! * **Switches** (all off, owner-only through the `remoteFormFill` prefix, `settings.rs`):
//!   `remoteFormFillLogins`, `remoteFormFillLoginManagers` (`name=chord` lines; empty means no
//!   manager is used), `remoteFormFillPayments`. Both tools need `remoteTools` as well, run on the
//!   local door only (the ChatGPT gateway is refused), and need an unlocked chat first
//!   (`unlock::effectful`).
//! * **The secure box.** One item per login or card in the OS credential store
//!   (`rightkit-credentials`, service `pulse.agent-secrets`), plus an index item that holds only
//!   labels, hosts and limits. Only the resident engine reads or writes them (`put`, `list`,
//!   `remove`, reached through the owner-only `remote.secure.*` methods of the hub or owner app);
//!   `put` needs the host app PIN. A card must be marked virtual or tokenized, must name the hosts
//!   it may be used on, and carries its own limits (`maxCents` per purchase, `autoCents` below
//!   which no question is asked, `monthCents` per calendar month). Nothing here reads a password
//!   manager's vault. A manager the user listed is used only by sending that manager's own
//!   autofill chord to the verified browser; its own unlock (Touch ID, Hello) then runs in the
//!   browser and the host app sees nothing.
//! * **Origin check.** The page's address comes from the browser (Apple Events on macOS, Safari,
//!   Chrome, Brave and Edge, which needs the Automation grant; UI Automation on the address bar of
//!   Chrome, Edge, Brave and Firefox on Windows, `browser_address.rs`, which also refuses a
//!   focused address bar, a field outside the checked window and a Chromium address that hides
//!   its scheme), must be `https` on the default port with no user-info, and its host must equal
//!   a host saved with the item, exactly. It is read before the approval is asked, again before
//!   the value is typed and again after; a change of address (the whole of it, not only the host)
//!   or, on Windows, of the page document in between ends the call (`origin_changed`). Only the
//!   top-level page is checked, not frames inside it.
//! * **One field, one handle.** The focused field is looked up once (`inject::target`); every
//!   check (in the checked window, not the address bar, in the page's own top-level document and,
//!   on Windows, in the very document whose address was read; a text field; secure or not) runs on
//!   that element, and the value is written into that same element, never into whatever has focus
//!   by then. A clear after a fill that must not stand clears that element too. Where the platform
//!   cannot write into the element itself, the host app refuses (`needs_user`) rather than type keys.
//! * **One-time codes.** A saved login may carry a TOTP secret (RFC 6238; `put` takes the base32
//!   secret or the `otpauth://totp/` address, alone for an existing login). It lives only in that
//!   login's secure-box item; `login.fill` with field `totp` computes the code after the same
//!   page check and approval (the same grant as username and password) and types it.
//! * **Approvals.** A login always asks (an act approval bound to chat, host and item; Touch ID
//!   signature, Windows Hello through owner app or the PIN: `approvals::decide_signed`); one
//!   approval is a short grant for the same chat, host and item, held in memory, good for a few
//!   fields (`LOGIN_GRANT`).
//! * **Purchases.** A payment is one purchase (`Purchase`): the chat, the page's exact address and
//!   host, the card, the amount and the set of fields the call names (`fields`), bound together
//!   into one approval and used once; each field of the set is typed at most once. Before anything
//!   is typed the amount is reserved in the month's ledger (in the secure box, `LEDGER`) in the
//!   same locked step that checks `maxCents` and `monthCents` against what is already committed
//!   or held, so two calls cannot both pass on the same headroom. It asks unless the amount is at most the
//!   card's `autoCents`, and always asks while an earlier purchase on that card and site has an
//!   unknown outcome. The purchase commits (the spend is booked) only once every field of the set
//!   was typed. A purchase that ends having typed nothing (a refusal, the grant running out)
//!   releases its reservation; one that ends after something may have been typed is recorded as
//!   `unknown`, stays counted against the month and is never retried: a new purchase is a new call
//!   and a new reservation. A hold an earlier engine run left open reads as unknown. Pressing the
//!   pay button stays `screen.act`'s own question.
//! * **Global caps** (the owner's, in Settings, owner-only): `remoteFormFillPaymentsAutoCapCents`
//!   (default 2500) clamps every card's `autoCents`, and `remoteFormFillPaymentsMonthCapCents`
//!   (default 10000) bounds the month's committed and held spend over all cards together, whatever a
//!   card item says (`Caps`, `spend_gate`). Above the auto limit every purchase asks: a Touch ID
//!   signature on a Mac or the host app PIN on Windows (`approvals::decide_signed` refuses Windows
//!   Hello's `"os"` word until it signs with an enrolled key); nothing turns that question off. A
//!   live purchase used again must still hold its reservation in the ledger. The amount the owner
//!   sees is the agent's claim, so these caps, not an issuer-cap attestation, are the gate. the host app
//!   never reads the region of a 3-D Secure, one-time-code or CAPTCHA step: those go to spot
//!   takeover (`screen.act` handover), during which every tool here answers `user_in_control`.
//! * **Never.** A value is never in an answer, an approval summary, the Activity record, a log or
//!   the webview; the agent's own words about the amount and the merchant are shown to the owner
//!   as the agent's claim, and the host app does not verify them against the page. the host app enforces only
//!   the card's own limits here, over the amounts the agent claims; a limit the card issuer sets on
//!   the virtual card is outside the host app, which neither checks nor relies on it.
//!
//! Unproven: the typing itself (`inject`) goes through the Accessibility value of the verified
//! field and fails closed (`needs_user`) where a page does not take it; that a browser's field
//! element dies with its page when the tab navigates (which is what keeps a held handle on the
//! checked page between the address checks) is expected, not observed; nothing here has run
//! against a real browser. Same-user limit: another process of the same user can read the secure
//! box items through the OS credential store as it can read any of its other items.

use super::settings::Settings;
use super::{Origin, Refusal, Result, approvals, audit, unlock};
use serde_json::{Map, Value, json};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SERVICE: &str = "pulse.agent-secrets";
const INDEX: &str = "index-v1";
const LEDGER: &str = "ledger-v1";
/// The longest one login grant lasts, and how many fields it may type.
const LOGIN_GRANT: Duration = Duration::from_secs(2 * 60);
const LOGIN_USES: u8 = 4;
/// The longest one purchase stays open for its fields (each of its set typed once, in any order).
const PAYMENT_GRANT: Duration = Duration::from_secs(5 * 60);
const MAX_RECORDS: usize = 64;
const MAX_LABEL: usize = 64;
const MAX_SECRET: usize = 512;
const MAX_HOSTS: usize = 8;
/// A sanity bound on any amount or limit: $100,000.
const MAX_CENTS: u64 = 10_000_000;
/// Only US dollars (the owner, 2026-10-17: US banking, never India-specific rails).
const CURRENCY: &str = "USD";

fn invalid(why: impl Into<String>) -> Refusal {
    Refusal::new("invalid_request", why)
}

fn unavailable(why: impl std::fmt::Display) -> Refusal {
    Refusal::new(
        "secrets_unavailable",
        format!("the app's secure box cannot be used ({why}); nothing was typed"),
    )
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn new_id() -> String {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0u8; 8];
    if SystemRandom::new().fill(&mut bytes).is_err() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
        bytes = nanos.to_be_bytes();
    }
    rightkit_fs::digest::hex(&bytes)
}

fn require_engine() -> Result<()> {
    if approvals::is_engine_process() {
        Ok(())
    } else {
        Err(Refusal::new(
            "approval_unavailable",
            "the secure box is held by the engine, and this process is not the engine",
        ))
    }
}

// ---- the secure box ---------------------------------------------------------------------------

fn open() -> Result<rightkit_credentials::Store> {
    let namespace =
        rightkit_credentials::Namespace::new(SERVICE.to_string()).map_err(unavailable)?;
    let store = rightkit_credentials::Store::open(rightkit_credentials::Config::new(namespace));
    if !store.is_durable() {
        return Err(unavailable(
            "the OS credential store is guarded or unavailable",
        ));
    }
    Ok(store)
}

fn read_json(store: &rightkit_credentials::Store, account: &str) -> Result<Option<Value>> {
    match store.get(account) {
        Ok(Some(secret)) => {
            let text = secret.expose_str().map_err(unavailable)?;
            serde_json::from_str(text)
                .map(Some)
                .map_err(|_| unavailable("a stored item is damaged"))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(unavailable(e)),
    }
}

fn write_json(store: &rightkit_credentials::Store, account: &str, value: &Value) -> Result<()> {
    store
        .set_str(account, &value.to_string())
        .map_err(unavailable)
}

/// The index: labels, hosts and limits only, never a value.
fn read_index(store: &rightkit_credentials::Store) -> Result<Vec<Value>> {
    Ok(read_json(store, INDEX)?
        .and_then(|v| v["items"].as_array().cloned())
        .unwrap_or_default())
}

fn write_index(store: &rightkit_credentials::Store, items: &[Value]) -> Result<()> {
    write_json(store, INDEX, &json!({ "v": 1, "items": items }))
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value[key].as_str()
}

fn cents(value: &Value, key: &str) -> Option<u64> {
    value[key].as_u64().filter(|n| *n <= MAX_CENTS)
}

fn clean_label(value: &Value) -> Result<String> {
    let label = value
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.chars().count() <= MAX_LABEL)
        .filter(|s| !s.chars().any(char::is_control))
        .ok_or_else(|| invalid("label is 1 to 64 characters with no control characters"))?;
    Ok(label.to_string())
}

fn clean_secret(value: &Value, what: &str) -> Result<String> {
    let secret = value
        .as_str()
        .filter(|s| !s.is_empty() && s.chars().count() <= MAX_SECRET)
        .filter(|s| !s.chars().any(|c| c == '\0'))
        .ok_or_else(|| invalid(format!("{what} is 1 to {MAX_SECRET} characters")))?;
    Ok(secret.to_string())
}

fn digits_only(value: &Value, what: &str, min: usize, max: usize) -> Result<String> {
    let raw = value
        .as_str()
        .ok_or_else(|| invalid(format!("{what} is digits")))?;
    let digits: String = raw.chars().filter(|c| !matches!(c, ' ' | '-')).collect();
    if digits.len() < min || digits.len() > max || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid(format!("{what} is {min} to {max} digits")));
    }
    Ok(digits)
}

fn luhn(digits: &str) -> bool {
    let mut sum = 0u32;
    for (i, byte) in digits.bytes().rev().enumerate() {
        let mut d = u32::from(byte - b'0');
        if i % 2 == 1 {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
    }
    sum.is_multiple_of(10)
}

/// A US ZIP: five digits, or nine with an optional dash.
fn clean_zip(value: &Value) -> Result<String> {
    let raw = value.as_str().map(str::trim).unwrap_or_default();
    let digits: String = raw.chars().filter(|c| *c != '-').collect();
    if (digits.len() == 5 || digits.len() == 9) && digits.bytes().all(|b| b.is_ascii_digit()) {
        Ok(raw.to_string())
    } else {
        Err(invalid("zip is a US ZIP code (five digits, or ZIP+4)"))
    }
}

// ---- the one-time code (TOTP, RFC 6238) -------------------------------------------------------

/// RFC 4648 base32 (the alphabet authenticator apps show), spaces, dashes and `=` ignored.
fn base32_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut bits, mut have) = (0u32, 0u32);
    for c in text.chars().filter(|c| !matches!(c, ' ' | '-' | '=')) {
        let v = match c.to_ascii_uppercase() {
            c @ 'A'..='Z' => c as u32 - 'A' as u32,
            c @ '2'..='7' => c as u32 - '2' as u32 + 26,
            _ => return None,
        };
        bits = (bits << 5) | v;
        have += 5;
        if have >= 8 {
            have -= 8;
            out.push((bits >> have) as u8);
            bits &= (1 << have) - 1;
        }
    }
    Some(out)
}

/// What the owner typed for a login's one-time code: a base32 secret, or the `otpauth://totp/...`
/// address an authenticator setup shows (`secret`, `digits`, `period`, `algorithm`). Answers the
/// stored form `{ secret, digits, period, algorithm }`; an error never contains the text.
fn parse_totp(value: &Value) -> Result<Value> {
    let bad = || {
        invalid(
            "totp is the base32 secret (or the otpauth:// address) of the site's authenticator setup; digits 6 to 8, period 15 to 120 seconds, SHA1, SHA256 or SHA512",
        )
    };
    let raw = value.as_str().map(str::trim).ok_or_else(bad)?;
    let (mut secret, mut digits, mut period, mut algorithm) =
        (raw.to_string(), 6u64, 30u64, "SHA1".to_string());
    if raw
        .get(..10)
        .is_some_and(|p| p.eq_ignore_ascii_case("otpauth://"))
    {
        let rest = &raw[10..];
        if !rest
            .get(..5)
            .is_some_and(|t| t.eq_ignore_ascii_case("totp/"))
        {
            return Err(bad());
        }
        secret = String::new();
        let query = rest.split_once('?').map(|(_, q)| q).ok_or_else(bad)?;
        for pair in query.split('&') {
            let (key, val) = pair.split_once('=').unwrap_or((pair, ""));
            match key.to_ascii_lowercase().as_str() {
                "secret" => secret = val.to_string(),
                "digits" => digits = val.parse().map_err(|_| bad())?,
                "period" => period = val.parse().map_err(|_| bad())?,
                "algorithm" => algorithm = val.to_ascii_uppercase(),
                _ => {}
            }
        }
    }
    let key = base32_decode(&secret).ok_or_else(bad)?;
    if !(10..=64).contains(&key.len())
        || !(6..=8).contains(&digits)
        || !(15..=120).contains(&period)
        || !matches!(algorithm.as_str(), "SHA1" | "SHA256" | "SHA512")
    {
        return Err(bad());
    }
    let clean: String = secret
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '='))
        .collect::<String>()
        .to_ascii_uppercase();
    Ok(json!({ "secret": clean, "digits": digits, "period": period, "algorithm": algorithm }))
}

/// The code of `cfg` (the stored form) at Unix time `unix`, and the seconds it stays valid.
fn totp_at(cfg: &Value, unix: u64) -> Option<(String, u64)> {
    use ring::hmac;
    let key = base32_decode(cfg["secret"].as_str()?)?;
    let digits = cfg["digits"].as_u64().filter(|d| (6..=8).contains(d))? as u32;
    let period = cfg["period"].as_u64().filter(|p| (15..=120).contains(p))?;
    let algorithm = match cfg["algorithm"].as_str()? {
        "SHA1" => hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
        "SHA256" => hmac::HMAC_SHA256,
        "SHA512" => hmac::HMAC_SHA512,
        _ => return None,
    };
    let tag = hmac::sign(
        &hmac::Key::new(algorithm, &key),
        &(unix / period).to_be_bytes(),
    );
    let bytes = tag.as_ref();
    let at = (bytes.last()? & 0x0f) as usize;
    let word = u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?) & 0x7fff_ffff;
    let code = word % 10u32.pow(digits);
    Some((
        format!("{code:0width$}", width = digits as usize),
        period - unix % period,
    ))
}

/// The current code. A code with under 5 seconds left is waited out, so the next one is typed and
/// still valid when the site checks it.
fn totp_now(cfg: &Value) -> Option<String> {
    let (code, left) = totp_at(cfg, now_secs())?;
    if left >= 5 {
        return Some(code);
    }
    std::thread::sleep(Duration::from_secs(left) + Duration::from_millis(300));
    totp_at(cfg, now_secs()).map(|(code, _)| code)
}

// ---- hosts and the page's address -------------------------------------------------------------

/// A host name as a person types it: ASCII letters, digits and hyphens in dot-separated labels,
/// at least two labels, not an IP address. Lower case.
fn valid_host(host: &str) -> Option<String> {
    let host = host.trim().to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host).to_string();
    if host.is_empty() || host.len() > 253 || !host.contains('.') {
        return None;
    }
    let labels: Vec<&str> = host.split('.').collect();
    let ok = labels.iter().all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    });
    // An IP address is no site: the last label of a name is never all digits.
    let numeric_tld = labels
        .last()
        .is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit()));
    (ok && !numeric_tld).then_some(host)
}

/// The host of a page address the browser reported, only when it is `https` on the default port
/// with no user-info, no control or non-ASCII character and a valid host. Anything else is `None`
/// (a page the host app will not type into).
pub fn host_of(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url
        .get(..8)
        .filter(|p| p.eq_ignore_ascii_case("https://"))?;
    let rest = &url[rest.len()..];
    if !url.is_ascii() || url.chars().any(|c| c.is_control() || c == ' ' || c == '\\') {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.contains('@') || authority.contains('[') {
        return None;
    }
    let host = match authority.split_once(':') {
        None => authority,
        Some((host, "443")) => host,
        Some(_) => return None,
    };
    valid_host(host)
}

struct Page {
    pid: i32,
    /// The front window the address was read from (Windows; `None` on a Mac).
    window: Option<u64>,
    /// The page's whole address as the browser gave it; a later read must give the same.
    url: String,
    host: String,
    /// Windows: the one top-level document whose Value was `url`; the field must be inside it.
    #[cfg(windows)]
    doc: super::browser_address::El,
}

#[cfg(target_os = "macos")]
fn no_browser() -> Refusal {
    Refusal::new(
        "no_browser",
        "the front app is not a browser the app can read the page address from (Safari, Chrome, Brave or Edge on a Mac); bring the page to the front",
    )
}

#[cfg(target_os = "macos")]
fn read_page() -> Result<Page> {
    use rightkit_automation::Platform;
    use rightkit_child_process::RunOptions;
    let front = rightkit_automation::NativePlatform::new()
        .frontmost()
        .ok_or_else(no_browser)?;
    // One fixed script per browser; nothing from the caller is part of a script.
    let script = match front.app_id.to_ascii_lowercase().as_str() {
        "com.apple.safari" => {
            "tell application id \"com.apple.Safari\" to get URL of current tab of front window"
        }
        "com.google.chrome" => {
            "tell application id \"com.google.Chrome\" to get URL of active tab of front window"
        }
        "com.brave.browser" => {
            "tell application id \"com.brave.Browser\" to get URL of active tab of front window"
        }
        "com.microsoft.edgemac" => {
            "tell application id \"com.microsoft.edgemac\" to get URL of active tab of front window"
        }
        _ => return Err(no_browser()),
    };
    let options = RunOptions::new(Duration::from_secs(5))
        .stdout_cap(4096)
        .stderr_cap(4096);
    let done = rightkit_child_process::run("/usr/bin/osascript", vec!["-e", script], &options)
        .map_err(|_| Refusal::new("spawn_failed", "osascript could not be started"))?;
    let unreadable = |why: &str| Refusal::new("origin_unreadable", why.to_string());
    if done.timed_out {
        return Err(unreadable(
            "the browser did not give its page address in 5 seconds",
        ));
    }
    if !done.status.success() {
        let stderr = String::from_utf8_lossy(&done.stderr.bytes).into_owned();
        if stderr.contains("-1743") || stderr.contains("-1744") {
            return Err(Refusal::new(
                "permission_missing",
                "The app is not allowed to ask the browser for its page address; allow it in System Settings, Privacy & Security, Automation",
            ));
        }
        return Err(unreadable("the browser did not give its page address"));
    }
    let url = String::from_utf8_lossy(&done.stdout.bytes)
        .trim()
        .to_string();
    let host = host_of(&url).ok_or_else(|| {
        unreadable("the page is not an https address the app will type into (https, default port, no user-info)")
    })?;
    Ok(Page {
        pid: front.pid,
        window: None,
        url,
        host,
    })
}

/// Windows: the committed page URL (the top-level document's Value, not the editable address bar
/// text) of the front Chrome, Edge, Brave or Firefox window, read through UI Automation
/// (`browser_address.rs`), then the same https and host rules as on a Mac.
#[cfg(windows)]
fn read_page() -> Result<Page> {
    use rightkit_automation::Platform;
    let front = rightkit_automation::NativePlatform::new()
        .frontmost()
        .ok_or_else(super::browser_address::no_browser)?;
    let address = super::browser_address::read(&front)?;
    let url = address.full_url()?;
    let host = host_of(&url).ok_or_else(|| {
        Refusal::new(
            "origin_unreadable",
            "the page is not an https address the app will type into (https, default port, no user-info)",
        )
    })?;
    Ok(Page {
        pid: front.pid,
        window: front.window_id,
        url,
        host,
        doc: address.doc,
    })
}

#[cfg(not(any(target_os = "macos", windows)))]
fn read_page() -> Result<Page> {
    Err(Refusal::new(
        "origin_unreadable",
        "The app reads the page address on a Mac or on Windows only; on this computer it cannot check the site, so it types nothing",
    ))
}

// ---- typing -----------------------------------------------------------------------------------

/// What the typing found; never the text.
struct Landed {
    confirmed: bool,
}

/// A fill that did not succeed. `maybe_typed`: the host app may already have written into the page (the
/// write was attempted, or a check after it failed), so the outcome is unknown, not "nothing".
struct Failed {
    refusal: Refusal,
    maybe_typed: bool,
}

impl Failed {
    fn untyped(refusal: Refusal) -> Failed {
        Failed {
            refusal,
            maybe_typed: false,
        }
    }
}

#[cfg(any(target_os = "macos", windows))]
mod inject {
    use super::*;
    use rightkit_automation::{Action, InputEvent, NativePlatform, Platform};

    type El = <NativePlatform as Platform>::El;

    fn failed(code: &'static str, why: &str) -> Refusal {
        Refusal::new(code, why.to_string())
    }

    /// The one field the host app checked, held by its own element handle. The value is written into this
    /// element, never into a second lookup of whatever has focus.
    pub struct Field {
        el: El,
    }

    /// Looks up the focused field of the verified page once and proves everything on that element:
    /// where it is (Windows: in the window whose address was read, not the browser's address bar,
    /// inside the very top-level document whose address was read, `browser_address.rs`; Mac: in
    /// the app's front window and in the page itself, not a frame, `field_in_page_mac`) and what it
    /// is (a text field; `secure` `Some(true)` a password field, `Some(false)` not one, `None`
    /// either).
    pub fn target(page: &Page, secure: Option<bool>) -> Result<Field> {
        let platform = NativePlatform::new();
        let el = platform
            .focused(page.pid)
            .map_err(|_| failed("no_field", "the browser did not name a focused field"))?
            .ok_or_else(|| {
                failed(
                    "no_field",
                    "no field has focus in the browser; focus the field first (screen.act focus)",
                )
            })?;
        #[cfg(windows)]
        crate::browser_address::guard_field(page.window, &page.doc, &el)?;
        #[cfg(not(windows))]
        field_in_page_mac(page.pid, &el)?;
        let raw = platform
            .read(&el)
            .ok_or_else(|| failed("no_field", "the focused field is gone"))?;
        let role = raw.role.to_ascii_lowercase();
        if !(role.contains("text") || role.contains("edit") || role.contains("field")) {
            return Err(failed(
                "field_mismatch",
                "the focused element is not a text field",
            ));
        }
        if let Some(want) = secure
            && raw.secure != want
        {
            return Err(failed(
                "field_mismatch",
                if want {
                    "the focused field is not a password field"
                } else {
                    "the focused field is a password field, not this one"
                },
            ));
        }
        Ok(Field { el })
    }

    /// Writes `secret` into the checked field itself. The answer says whether the field changed;
    /// the text is never read back into an answer or a log. A field that is gone, or a platform
    /// that cannot write into that element (`Unsupported`), types nothing; any other failure may
    /// have written part of it (`maybe_typed`).
    pub fn set_field(field: &Field, secret: &str) -> std::result::Result<Landed, Failed> {
        let platform = NativePlatform::new();
        let el = &field.el;
        let before = platform
            .read(el)
            .ok_or_else(|| Failed::untyped(failed("no_field", "the focused field is gone")))?
            .value
            .unwrap_or_default();
        let maybe = |refusal| Failed {
            refusal,
            maybe_typed: true,
        };
        match platform.semantic(el, &Action::SetValue(secret.to_string())) {
            Ok(_) => {}
            Err(rightkit_automation::Error::Unsupported(_))
                if platform.native_write_capable(el) =>
            {
                platform.native_write(el, secret).map_err(|_| {
                    maybe(failed(
                        "injection_failed",
                        "the field did not take the value",
                    ))
                })?;
            }
            Err(rightkit_automation::Error::Unsupported(_)) => {
                return Err(Failed::untyped(failed(
                    "needs_user",
                    "this page's field cannot be filled by the app; type it yourself",
                )));
            }
            Err(_) => {
                return Err(maybe(failed(
                    "injection_failed",
                    "the field did not take the value",
                )));
            }
        }
        let after = platform.read(el).and_then(|n| n.value).unwrap_or_default();
        Ok(Landed {
            confirmed: !after.is_empty() && after != before,
        })
    }

    /// Mac: from the checked field `el` walk up `AXParent` (bounded) to the window. The field is in
    /// the page itself only when exactly one `AXWebArea` lies between it and the window (a frame's
    /// own content is a second, nested `AXWebArea`) and that window is the app's front window, the
    /// one the page address was read from. No `AXWebArea` (the address bar, a native control) or an
    /// unfinished walk refuses. The published API reads `AXURL` only as an opaque CF type
    /// (`AttrValue::Other`), so the web area's own URL cannot be compared with the checked one
    /// (RightKit gap: a string read of a CFURL attribute); the single-web-area rule is what proves
    /// "top-level document", and the page address read again after the field was taken (and after
    /// the write) is what ties it to the checked page.
    #[cfg(not(windows))]
    fn field_in_page_mac(pid: i32, el: &El) -> Result<()> {
        use rightkit_automation::Target;
        let platform = NativePlatform::new();
        let refuse = |why: &str| failed("field_in_frame", why);
        let unproven = || {
            refuse(
                "The app could not prove the focused field is in the page itself and not in an embedded frame, so it types nothing; click a field of the page and try again",
            )
        };
        let front = platform
            .root(&Target {
                pid,
                window_id: None,
            })
            .ok()
            .and_then(|w| platform.window_of(&w))
            .ok_or_else(unproven)?;
        if platform.window_of(el) != Some(front) {
            return Err(failed(
                "field_mismatch",
                "the focused field is not in the browser window the app checked",
            ));
        }
        let mut cur = el.clone();
        let mut web_areas = 0usize;
        for _ in 0..64 {
            match cur.str_attr("AXRole").as_deref() {
                Some("AXWebArea") => web_areas += 1,
                Some("AXWindow") => {
                    return match web_areas {
                        1 => Ok(()),
                        0 => Err(refuse(
                            "the focused field is not in the page (the address bar or a browser control)",
                        )),
                        _ => Err(refuse(
                            "the focused field is inside an embedded frame, not the page itself",
                        )),
                    };
                }
                _ => {}
            }
            cur = cur.el_attr("AXParent").ok_or_else(unproven)?;
        }
        Err(unproven())
    }

    /// Clears the checked field itself (best effort), after a fill that must not stand; never the
    /// field that has focus by then.
    pub fn clear_field(field: &Field) {
        let _ = NativePlatform::new().semantic(&field.el, &Action::SetValue(String::new()));
    }

    /// Sends a password manager's own autofill chord to the browser in front.
    pub fn send_chord(pid: i32, spec: &str) -> Result<()> {
        let chord = rightkit_automation::keys::parse_chord(spec, cfg!(target_os = "macos"))
            .map_err(|_| failed("invalid_request", "the manager's key chord is not valid"))?;
        NativePlatform::new()
            .foreground(pid, &InputEvent::Key(chord))
            .map_err(|_| failed("injection_failed", "the key chord was not delivered"))
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod inject {
    use super::*;

    fn unsupported() -> Refusal {
        Refusal::new(
            "unsupported_platform",
            "saved logins and cards are typed on macOS and Windows only",
        )
    }

    pub struct Field;

    pub fn target(_: &Page, _: Option<bool>) -> Result<Field> {
        Err(unsupported())
    }

    pub fn set_field(_: &Field, _: &str) -> std::result::Result<Landed, Failed> {
        Err(Failed::untyped(unsupported()))
    }

    pub fn clear_field(_: &Field) {}

    pub fn send_chord(_: i32, _: &str) -> Result<()> {
        Err(unsupported())
    }
}

// ---- grants and spend -------------------------------------------------------------------------

/// A short login authorization the owner gave for one bind, in engine memory.
struct Grant {
    key: String,
    until: Instant,
    left: u8,
}

static GRANTS: Mutex<Vec<Grant>> = Mutex::new(Vec::new());
static LEDGER_LOCK: Mutex<()> = Mutex::new(());

/// Uses one field of a live grant for `key`; false when there is none left.
fn grant_use(key: &str) -> bool {
    let mut grants = GRANTS.lock().unwrap_or_else(|e| e.into_inner());
    grants.retain(|g| g.until > Instant::now() && g.left > 0);
    match grants.iter_mut().find(|g| g.key == key) {
        Some(g) => {
            g.left -= 1;
            true
        }
        None => false,
    }
}

fn grant_add(key: &str, ttl: Duration, uses: u8) {
    let mut grants = GRANTS.lock().unwrap_or_else(|e| e.into_inner());
    grants.retain(|g| g.until > Instant::now() && g.key != key);
    // A used grant leaves no spare use: the field that started it is the first one.
    grants.push(Grant {
        key: key.to_string(),
        until: Instant::now() + ttl,
        left: uses.saturating_sub(1),
    });
}

/// `YYYY-MM` of a Unix time (UTC), for the monthly total.
fn month_key(secs: u64) -> String {
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}")
}

/// This engine run. A hold an earlier run left `reserved` (it stopped mid-purchase) reads as
/// `unknown`: the host app cannot tell what it typed before it stopped.
fn run_id() -> &'static str {
    static RUN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    RUN.get_or_init(new_id)
}

/// The month's spending, in the secure box. `cards`: what finished purchases cost (committed).
/// `holds`: one per purchase not committed, `reserved` while it is open, `unknown` once it ended
/// after something may have been typed. Every hold counts against `monthCents` as much as a
/// committed spend does; only a purchase that typed every field commits its hold, and only one
/// that typed nothing releases it. A ledger from an earlier month starts the month at nothing.
struct Ledger {
    month: String,
    cards: Map<String, Value>,
    holds: Vec<Value>,
}

impl Ledger {
    fn load(store: &rightkit_credentials::Store) -> Result<Ledger> {
        let month = month_key(now_secs());
        let saved = read_json(store, LEDGER)?.unwrap_or(Value::Null);
        let rolled = saved["month"].as_str() != Some(month.as_str());
        if rolled && saved["holds"].as_array().is_none_or(|h| h.is_empty()) {
            return Ok(Ledger {
                month,
                cards: Map::new(),
                holds: Vec::new(),
            });
        }
        let saved_month = saved["month"].as_str().unwrap_or_default().to_string();
        let damaged = || unavailable("the month's spending record is damaged");
        let mut holds = saved["holds"].as_array().cloned().unwrap_or_default();
        for hold in &mut holds {
            let whole = hold["tx"].is_string()
                && hold["card"].is_string()
                && cents(hold, "cents").is_some()
                && matches!(hold["state"].as_str(), Some("reserved" | "unknown"));
            if !whole {
                return Err(damaged());
            }
            if hold["state"] == "reserved" && hold["run"].as_str() != Some(run_id()) {
                hold["state"] = Value::from("unknown");
            }
            // The month a hold was made in; an older ledger's holds were made in its own month.
            if hold["month"].as_str().is_none() {
                hold["month"] = Value::from(saved_month.clone());
            }
        }
        if rolled {
            // A new month starts spending at nothing, but a hold whose outcome is unknown stays
            // (counted in the month it was made, shown, and still blocking that site) until settled.
            // A hold still `reserved` here was made by this run (an earlier run's became unknown
            // above), so its purchase is open or ending in this process: it stays in its own month
            // so `end_purchase` finds it by id.
            holds.retain(|h| h["state"] == "unknown" || h["run"].as_str() == Some(run_id()));
            return Ok(Ledger {
                month,
                cards: Map::new(),
                holds,
            });
        }
        let cards = match &saved["cards"] {
            Value::Null => Map::new(),
            Value::Object(cards) => cards.clone(),
            _ => return Err(damaged()),
        };
        Ok(Ledger {
            month,
            cards,
            holds,
        })
    }

    fn save(&self, store: &rightkit_credentials::Store) -> Result<()> {
        write_json(
            store,
            LEDGER,
            &json!({ "v": 2, "month": self.month, "cards": self.cards, "holds": self.holds }),
        )
    }

    fn committed(&self, card: &str) -> u64 {
        self.cards.get(card).and_then(Value::as_u64).unwrap_or(0)
    }

    /// Holds in `state`, every month (what the secure box shows).
    fn held(&self, card: &str, state: &str) -> u64 {
        self.holds
            .iter()
            .filter(|h| h["card"].as_str() == Some(card) && h["state"].as_str() == Some(state))
            .filter_map(|h| h["cents"].as_u64())
            .fold(0, u64::saturating_add)
    }

    /// Holds in `state` made in this ledger's month (what counts against its limit).
    fn held_this_month(&self, card: &str, state: &str) -> u64 {
        self.holds
            .iter()
            .filter(|h| h["card"].as_str() == Some(card) && h["state"].as_str() == Some(state))
            .filter(|h| h["month"].as_str().is_none_or(|m| m == self.month))
            .filter_map(|h| h["cents"].as_u64())
            .fold(0, u64::saturating_add)
    }

    /// Everything that counts against the card's month limit.
    fn used(&self, card: &str) -> u64 {
        self.committed(card)
            .saturating_add(self.held_this_month(card, "reserved"))
            .saturating_add(self.held_this_month(card, "unknown"))
    }

    /// Everything that counts against the global month cap: every card's committed spend and every
    /// hold of this month, whatever card (or deleted card) it was made with.
    fn used_all(&self) -> u64 {
        let committed = self
            .cards
            .values()
            .filter_map(Value::as_u64)
            .fold(0, u64::saturating_add);
        self.holds
            .iter()
            .filter(|h| h["month"].as_str().is_none_or(|m| m == self.month))
            .filter_map(|h| h["cents"].as_u64())
            .fold(committed, u64::saturating_add)
    }

    /// Holds in `state` whatever the card, every month.
    fn held_all(&self, state: &str) -> usize {
        self.holds
            .iter()
            .filter(|h| h["state"].as_str() == Some(state))
            .count()
    }

    /// An earlier purchase with this card on this site ended with an unknown outcome.
    fn unknown_on(&self, card: &str, host: &str) -> bool {
        self.holds.iter().any(|h| {
            h["card"].as_str() == Some(card)
                && h["host"].as_str() == Some(host)
                && h["state"] == "unknown"
        })
    }
}

/// How a purchase ends.
#[derive(Clone, Copy)]
enum End {
    /// Every field of its set was typed: the spend is booked.
    Commit,
    /// Nothing was typed: the reservation goes.
    Release,
    /// Something may have been typed: the hold stays, counted, as `unknown`.
    Unknown,
}

/// One purchase the owner (or the card's no-question limit) allowed, in engine memory: bound to
/// the chat, the page's exact address and host, the card, the amount and the set of fields (`key`),
/// with its amount reserved as hold `tx` before anything was typed. Used once.
struct Purchase {
    key: String,
    tx: String,
    client: String,
    host: String,
    card: String,
    last4: String,
    amount: u64,
    /// The fields of the set not typed yet; each is typed at most once.
    left: Vec<String>,
    /// A field is being typed right now (one at a time).
    busy: bool,
    /// The host app may already have written into the page for this purchase.
    typed: bool,
    until: Instant,
}

static PURCHASES: Mutex<Vec<Purchase>> = Mutex::new(Vec::new());

/// Ends purchase `p`: books, releases or marks its hold, and records the end in Activity. A ledger
/// that cannot be written keeps the hold as it was, still counted (fail closed); the answer says
/// whether the record was written.
fn end_purchase(store: &rightkit_credentials::Store, p: &Purchase, end: End) -> bool {
    let written = (|| -> Result<()> {
        let _guard = LEDGER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut ledger = Ledger::load(store)?;
        let this = |h: &Value| h["tx"].as_str() == Some(p.tx.as_str());
        match end {
            End::Commit => {
                // Booked into the ledger's current month. A purchase whose hold was made before
                // the ledger rolled over still counts here: the earlier month's totals are gone,
                // and leaving it out of both months would let spending pass the limit.
                ledger.holds.retain(|h| !this(h));
                let total = ledger.committed(&p.card).saturating_add(p.amount);
                ledger.cards.insert(p.card.clone(), Value::from(total));
            }
            End::Release => ledger.holds.retain(|h| !this(h)),
            End::Unknown => {
                if ledger.holds.iter().any(this) {
                    for hold in ledger.holds.iter_mut().filter(|h| this(h)) {
                        hold["state"] = Value::from("unknown");
                    }
                } else {
                    // The reservation is gone (a reset ledger) after something was typed: count
                    // the purchase again as an unknown hold, once, keyed by its reservation id.
                    ledger.holds.push(json!({
                        "tx": p.tx, "card": p.card, "host": p.host, "cents": p.amount,
                        "state": "unknown", "run": run_id(), "month": ledger.month,
                    }));
                }
            }
        }
        ledger.save(store)
    })()
    .is_ok();
    let outcome = match (end, written) {
        (End::Commit, true) => "purchase_committed",
        (End::Release, true) => "purchase_released",
        (End::Unknown, _) => "purchase_outcome_unknown",
        (_, false) => "purchase_record_failed",
    };
    audit::record_secret(
        &p.client,
        "payment.fill",
        &p.host,
        "purchase",
        outcome,
        Some(p.amount),
        Some(&p.last4),
    );
    written
}

/// Ends every purchase whose grant ran out (one being typed is left to finish): unknown when it
/// may have typed something, released when it typed nothing. Called with `PURCHASES` held.
fn sweep(store: &rightkit_credentials::Store, purchases: &mut Vec<Purchase>) {
    let now = Instant::now();
    let mut at = 0;
    while at < purchases.len() {
        if purchases[at].until <= now && !purchases[at].busy {
            let p = purchases.remove(at);
            let end = if p.typed { End::Unknown } else { End::Release };
            end_purchase(store, &p, end);
        } else {
            at += 1;
        }
    }
}

/// The fields of a card `payment.fill` types.
const PAYMENT_FIELDS: [&str; 7] = ["number", "expMonth", "expYear", "exp", "cvv", "name", "zip"];

/// The purchase's field set (`fields`): one to seven distinct card fields, in a fixed order.
fn purchase_fields(value: &Value) -> Result<Vec<String>> {
    let bad = || {
        invalid(
            "fields is the list of card fields this checkout needs (number, expMonth, expYear, exp, cvv, name, zip), each once; the same list on every call of one purchase",
        )
    };
    let list = value
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= PAYMENT_FIELDS.len())
        .ok_or_else(bad)?;
    let mut fields = Vec::new();
    for item in list {
        let name = item
            .as_str()
            .filter(|n| PAYMENT_FIELDS.contains(n))
            .ok_or_else(bad)?;
        if fields.iter().any(|f| f == name) {
            return Err(bad());
        }
        fields.push(name.to_string());
    }
    fields.sort_by_key(|f| PAYMENT_FIELDS.iter().position(|p| p == f));
    Ok(fields)
}

/// What a card's limits say about one purchase.
#[derive(Debug, PartialEq, Eq)]
enum Gate {
    /// At most `autoCents`: no question.
    Auto,
    /// Above it: the owner confirms this purchase.
    Ask,
    Refuse(&'static str, &'static str),
}

/// The owner's global caps (`Settings`), which clamp what any card item says.
struct Caps {
    /// Largest purchase approved without asking, over all cards.
    auto: u64,
    /// Month total over all cards.
    month: u64,
    /// What every card has used this month (committed plus held).
    used_all: u64,
}

fn spend_gate(max: u64, auto: u64, month: u64, spent: u64, amount: u64, caps: &Caps) -> Gate {
    // The global caps win over a card item: no card can approve more without asking than the
    // owner's cap, and no set of cards can spend past the owner's month total. An amount above
    // `auto` asks every time (Touch ID signature or the host app PIN, `approvals::decide_signed`);
    // nothing turns that question off.
    let auto = auto.min(caps.auto);
    if caps.used_all.saturating_add(amount) > caps.month {
        Gate::Refuse(
            "over_global_month_limit",
            "this would take all cards past the monthly total you set for agent payments in the app's settings",
        )
    } else if amount == 0 || amount > max {
        Gate::Refuse(
            "over_purchase_limit",
            "this is more than the limit you set for one purchase on this card",
        )
    } else if spent.saturating_add(amount) > month {
        Gate::Refuse(
            "over_month_limit",
            "this would take the card past the limit you set for the month",
        )
    } else if amount <= auto {
        Gate::Auto
    } else {
        Gate::Ask
    }
}

fn dollars(cents: u64) -> String {
    format!("${}.{:02}", cents / 100, cents % 100)
}

// ---- the owner's side: put, list, remove ------------------------------------------------------

/// The credential-store account of one item's values.
fn item_account(id: &str) -> String {
    format!("item-{id}")
}

fn index_find<'a>(items: &'a [Value], id: &str) -> Option<&'a Value> {
    items.iter().find(|v| v["id"].as_str() == Some(id))
}

fn hosts_of(value: &Value) -> Result<Vec<String>> {
    let list = value
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= MAX_HOSTS)
        .ok_or_else(|| invalid("hosts is 1 to 8 site names"))?;
    list.iter()
        .map(|h| {
            h.as_str()
                .and_then(valid_host)
                .ok_or_else(|| invalid("a host is a site name such as shop.example.com"))
        })
        .collect()
}

/// The engine's `remote.secure.put`: saves one login or card in the secure box. Owner-only (the
/// hub or owner app, executable verified by the resident engine) and needs the host app PIN every time
/// (`pin`). `kind` is `login` (`label`, `host`, `username`, `password`) or `card` (`label`,
/// `hosts`, `number`, `expMonth`, `expYear`, `cvv`, `name`, `zip`, `virtual` true, `maxCents`,
/// `autoCents`, `monthCents`); an `id` replaces that item. A login may add `totp` (base32 secret or
/// `otpauth://totp/` address); `kind` login with an `id`, `totp` and no username or password adds or
/// replaces only the one-time code of that login. Replacing a login by `id` without `totp` keeps
/// its saved one-time code; `"totp": false` removes it. Answers the item's metadata.
pub fn put(params: &Value) -> Result<Value> {
    require_engine()?;
    let pin = params["pin"]
        .as_str()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| {
            Refusal::new(
                "unlock_needs_auth",
                "saving a login or a card needs your PIN (set one in Settings > Remote first)",
            )
        })?;
    unlock::confirm_pin(pin)?;
    let kind = params["kind"].as_str().unwrap_or_default();
    // Only a one-time code for a login already saved: the password is not sent again.
    if kind == "login"
        && params["username"].is_null()
        && params["password"].is_null()
        && let Some(id) = params["id"].as_str()
    {
        return add_totp(id, &params["totp"]);
    }
    let label = clean_label(&params["label"])?;
    let (secret, meta) = match kind {
        "login" => {
            let host = params["host"]
                .as_str()
                .and_then(valid_host)
                .ok_or_else(|| invalid("host is a site name such as example.com"))?;
            let username = clean_secret(&params["username"], "username")?;
            let password = clean_secret(&params["password"], "password")?;
            let mut secret = json!({ "username": username, "password": password });
            let mut meta = json!({ "kind": "login", "label": label, "host": host });
            // `"totp": false` is the explicit removal; absent keeps the saved code (below).
            if !params["totp"].is_null() && params["totp"].as_bool() != Some(false) {
                secret["totp"] = parse_totp(&params["totp"])?;
                meta["totp"] = Value::from(true);
            }
            (secret, meta)
        }
        "card" => {
            if params["virtual"].as_bool() != Some(true) {
                return Err(Refusal::new(
                    "card_not_virtual",
                    "only a virtual or tokenized card can be saved: make one at your card issuer, limited to the shops you use, and save that",
                ));
            }
            let number = digits_only(&params["number"], "number", 13, 19)?;
            if !luhn(&number) {
                return Err(invalid("that card number does not pass the check digit"));
            }
            let exp_month = params["expMonth"]
                .as_u64()
                .filter(|m| (1..=12).contains(m))
                .ok_or_else(|| invalid("expMonth is 1 to 12"))?;
            let exp_year = params["expYear"]
                .as_u64()
                .map(|y| if y < 100 { y + 2000 } else { y })
                .filter(|y| (2000..=2200).contains(y))
                .ok_or_else(|| invalid("expYear is a year, such as 2029 or 29"))?;
            let cvv = digits_only(&params["cvv"], "cvv", 3, 4)?;
            let name = clean_secret(&params["name"], "name")?;
            let zip = clean_zip(&params["zip"])?;
            let hosts = hosts_of(&params["hosts"])?;
            let max = cents(params, "maxCents")
                .filter(|n| *n > 0)
                .ok_or_else(|| invalid("maxCents is the most one purchase may cost, above 0"))?;
            let auto = cents(params, "autoCents").unwrap_or(0);
            let month = cents(params, "monthCents")
                .filter(|n| *n > 0)
                .ok_or_else(|| invalid("monthCents is the most a month may cost, above 0"))?;
            if auto > max {
                return Err(invalid("autoCents cannot be above maxCents"));
            }
            let last4 = number[number.len() - 4..].to_string();
            (
                json!({
                    "number": number, "expMonth": exp_month, "expYear": exp_year, "cvv": cvv,
                    "name": name, "zip": zip,
                }),
                json!({
                    "kind": "card", "label": label, "hosts": hosts, "last4": last4,
                    "maxCents": max, "autoCents": auto, "monthCents": month,
                }),
            )
        }
        _ => return Err(invalid("kind is login or card")),
    };
    let store = open()?;
    let mut items = read_index(&store)?;
    let id = match params["id"].as_str() {
        Some(id) => {
            let kind_matches = index_find(&items, id).is_some_and(|v| v["kind"] == meta["kind"]);
            if !kind_matches {
                return Err(invalid("no saved item of that kind has that id"));
            }
            id.to_string()
        }
        None => {
            if items.len() >= MAX_RECORDS {
                return Err(invalid("the secure box holds at most 64 items"));
            }
            new_id()
        }
    };
    let mut meta = meta;
    let mut secret = secret;
    // Replacing a login's username or password keeps its saved one-time code unless the call
    // says `"totp": false`.
    if kind == "login"
        && params["id"].as_str().is_some()
        && params["totp"].is_null()
        && let Some(saved) = read_json(&store, &item_account(&id))?
        && !saved["totp"].is_null()
    {
        secret["totp"] = saved["totp"].clone();
        meta["totp"] = Value::from(true);
    }
    meta["id"] = Value::from(id.clone());
    write_json(&store, &item_account(&id), &secret)?;
    items.retain(|v| v["id"].as_str() != Some(id.as_str()));
    items.push(meta.clone());
    if let Err(e) = write_index(&store, &items) {
        let _ = store.delete(&item_account(&id));
        return Err(e);
    }
    Ok(meta)
}

/// Adds (or replaces) the one-time code of a saved login, keeping its username and password. The
/// index only learns that the login has one (`totp` true), never the secret.
fn add_totp(id: &str, totp: &Value) -> Result<Value> {
    let cfg = parse_totp(totp)?;
    let store = open()?;
    let mut items = read_index(&store)?;
    let mut meta = index_find(&items, id)
        .filter(|v| v["kind"] == "login")
        .cloned()
        .ok_or_else(|| invalid("no saved login has that id"))?;
    let mut stored = read_json(&store, &item_account(id))?
        .ok_or_else(|| unavailable("a saved login is missing"))?;
    stored["totp"] = cfg;
    write_json(&store, &item_account(id), &stored)?;
    meta["totp"] = Value::from(true);
    items.retain(|v| v["id"].as_str() != Some(id));
    items.push(meta.clone());
    write_index(&store, &items)?;
    Ok(meta)
}

/// The engine's `remote.secure.list`: the index (labels, hosts, last four digits, limits) and what
/// each card used this month (`spentCents`: everything counted against its month limit), of which
/// `heldCents` is reserved by purchases still open and `unknownCents` by purchases whose outcome
/// The host app cannot know. Never a value. Owner-only.
pub fn list() -> Result<Value> {
    require_engine()?;
    let store = open()?;
    let items = read_index(&store)?;
    let ledger = Ledger::load(&store)?;
    let (mut spent, mut held, mut unknown) = (Map::new(), Map::new(), Map::new());
    for item in &items {
        if item["kind"] == "card"
            && let Some(id) = item["id"].as_str()
        {
            spent.insert(id.to_string(), Value::from(ledger.used(id)));
            held.insert(id.to_string(), Value::from(ledger.held(id, "reserved")));
            unknown.insert(id.to_string(), Value::from(ledger.held(id, "unknown")));
        }
    }
    // The owner's global caps next to each card's own limits: the effective auto limit of a card
    // is the lower of the two, which is what the surfaces show (`capAutoCents`, `capMonthCents`).
    let settings = Settings::load();
    Ok(json!({
        "items": items, "month": ledger.month, "spentCents": spent,
        "heldCents": held, "unknownCents": unknown,
        "capAutoCents": settings.payments_auto_cap_cents,
        "capMonthCents": settings.payments_month_cap_cents,
        "usedAllCents": ledger.used_all(),
        "held": ledger.held_all("unknown"),
    }))
}

/// The engine's `remote.secure.remove`: deletes one saved item. Only ever reduces what the host app can
/// type, so it needs no PIN. Owner-only.
pub fn remove(id: &str) -> Result<Value> {
    require_engine()?;
    let store = open()?;
    let mut items = read_index(&store)?;
    let before = items.len();
    items.retain(|v| v["id"].as_str() != Some(id));
    if items.len() == before {
        return Ok(json!({ "removed": false }));
    }
    write_index(&store, &items)?;
    store.delete(&item_account(id)).map_err(unavailable)?;
    Ok(json!({ "removed": true }))
}

// ---- the agent's tools ------------------------------------------------------------------------

/// An allowlist, so a door added later is refused until it is named here (it never inherits
/// secrets or payments by being "not the gateway").
fn local_only(origin: Origin, tool: &str) -> Result<()> {
    match origin {
        Origin::Local => Ok(()),
        _ => Err(Refusal::new(
            "not_built",
            format!(
                "{tool} is offered only on this computer's own door, not through {}",
                origin.as_str()
            ),
        )),
    }
}

/// The chat or client the call is for, short, for an approval summary.
fn who() -> String {
    let client = approvals::current_client();
    match client.split_once(':') {
        Some(("agent", rest)) if rest.starts_with("chat:") => {
            format!("An agent in chat {}", &rest["chat:".len()..])
        }
        Some(("chat", id)) => format!("The chat {}", id.chars().take(8).collect::<String>()),
        Some(("local", name)) => format!("The program {name}"),
        _ if client == "agent" => "the app's own agent".to_string(),
        _ => "An agent".to_string(),
    }
}

fn claim(value: &Value, key: &str, max: usize) -> Option<String> {
    value[key]
        .as_str()
        .map(|s| {
            s.chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(max)
                .collect::<String>()
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
}

/// The one saved item of `kind` for `host` the call means: by `account` label when given. Refusals
/// name labels only.
fn resolve(
    items: &[Value],
    kind: &str,
    host: &str,
    label: Option<&str>,
    none: &'static str,
) -> Result<Value> {
    let hit = |v: &&Value| {
        v["kind"] == kind
            && (v["host"].as_str() == Some(host)
                || v["hosts"]
                    .as_array()
                    .is_some_and(|hs| hs.iter().any(|h| h.as_str() == Some(host))))
    };
    let found: Vec<&Value> = items
        .iter()
        .filter(hit)
        .filter(|v| label.is_none_or(|l| v["label"].as_str() == Some(l)))
        .collect();
    match found.as_slice() {
        [] => Err(Refusal::new(
            none,
            format!("The app has nothing saved for {host}; add it in the app's secure box first"),
        )),
        [one] => Ok((*one).clone()),
        many => {
            let labels: Vec<&str> = many.iter().filter_map(|v| v["label"].as_str()).collect();
            Err(Refusal::new(
                "ambiguous",
                format!(
                    "more than one saved item fits {host}: {}; repeat the call with account set to one",
                    labels.join(", ")
                ),
            ))
        }
    }
}

fn bind_for(parts: &[&str]) -> String {
    let bytes: Vec<&[u8]> = parts.iter().map(|p| p.as_bytes()).collect();
    approvals::bind_of(&bytes)
}

/// Lets the call through on a live grant, or on the owner's approval `approval` (used once, then
/// a grant starts); otherwise files the question and refuses `approval_required`.
fn authorize(
    origin: Origin,
    tool: &'static str,
    bind: &str,
    summary: &str,
    approval: Option<&str>,
    ttl: Duration,
    uses: u8,
) -> Result<()> {
    if grant_use(bind) {
        return Ok(());
    }
    match approval {
        Some(id) => {
            approvals::take_act_held(origin, tool, bind, id)?;
            grant_add(bind, ttl, uses);
            Ok(())
        }
        None => Err(approvals::request_act_held(origin, tool, bind, summary)),
    }
}

/// Windows: the page document read again is the same element as the checked one (a navigation
/// replaces it). On a Mac the browser gives no document handle; the field's own element and the
/// whole address stand in for it.
#[cfg(windows)]
fn same_document(before: &Page, now: &Page) -> bool {
    super::browser_address::same_element(&before.doc, &now.doc)
}

#[cfg(not(windows))]
fn same_document(_: &Page, _: &Page) -> bool {
    true
}

/// The page, read again, must be the same page (the whole address, and on Windows the same
/// document) in the same front app and window.
fn same_page(before: &Page) -> Result<()> {
    let now = read_page()?;
    if now.pid == before.pid
        && now.window == before.window
        && now.host == before.host
        && now.url == before.url
        && same_document(before, &now)
    {
        Ok(())
    } else {
        Err(Refusal::new(
            "origin_changed",
            "the page changed while the app was working, so it typed nothing more",
        ))
    }
}

/// Types `secret` into the focused field of the verified page: the field is taken and checked
/// once (`inject::target`), the page is read again after that (so the field was taken on the
/// checked page), the value goes into that same field, and the page is read once more after. A
/// page that changed after the write clears that field and counts as maybe typed.
fn type_verified(
    page: &Page,
    secret: &str,
    secure: Option<bool>,
) -> std::result::Result<Landed, Failed> {
    let field = inject::target(page, secure).map_err(Failed::untyped)?;
    same_page(page).map_err(Failed::untyped)?;
    let landed = inject::set_field(&field, secret)?;
    if let Err(changed) = same_page(page) {
        inject::clear_field(&field);
        return Err(Failed {
            refusal: changed,
            maybe_typed: true,
        });
    }
    Ok(landed)
}

fn done(field: &str, host: &str, landed: &Landed) -> Value {
    json!({ "state": "filled", "field": field, "host": host, "confirmed": landed.confirmed })
}

/// `login.fill`. `args`: `field` (`username`, `password` or `totp`) or `manager` (a name listed in
/// `remoteFormFillLoginManagers`), `account` (the label of the saved login, when the site has
/// more than one), `approval` (the id the owner approved). Types into the focused field of the
/// browser in front, after checking the page's address; answers `filled` or a refusal, never the
/// value.
pub fn login_fill(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
    // Nothing types, launches or takes focus while the user holds the screen (spot takeover, #355).
    super::screen::user_in_control()?;
    local_only(origin, "login.fill")?;
    settings.require_logins()?;
    require_engine()?;
    let field = args["field"].as_str();
    let manager = args["manager"].as_str();
    if field.is_some() == manager.is_some()
        || field.is_some_and(|f| !matches!(f, "username" | "password" | "totp"))
    {
        return Err(invalid(
            "login.fill takes field (username, password or totp) or manager (a listed password manager), not both",
        ));
    }
    let account = claim(args, "account", MAX_LABEL);
    let approval = args["approval"].as_str();
    let page = read_page()?;
    let client = approvals::current_client();
    let result = if let Some(name) = manager {
        let chord = settings
            .login_managers
            .iter()
            .find_map(|line| {
                let (n, chord) = line.split_once('=')?;
                (n.trim().eq_ignore_ascii_case(name) && !chord.trim().is_empty())
                    .then(|| chord.trim().to_string())
            })
            .ok_or_else(|| {
                Refusal::new(
                    "manager_off",
                    "that password manager is not listed in the app's settings (remoteFormFillLoginManagers); the owner opts in to each one",
                )
            })?;
        let bind = bind_for(&[
            "pulse.login.manager.v1",
            &client,
            &page.host,
            &name.to_ascii_lowercase(),
        ]);
        let summary = format!(
            "{} wants the app to start {name} on {}. The app checked that the page is {}; {name} asks for its own unlock and the app never sees what it fills.",
            who(),
            page.host,
            page.host
        );
        authorize(
            origin,
            "login.fill",
            &bind,
            &summary,
            approval,
            LOGIN_GRANT,
            1,
        )
        .and_then(|()| {
            same_page(&page)?;
            inject::send_chord(page.pid, &chord)?;
            same_page(&page)?;
            Ok(json!({ "state": "requested", "manager": name, "host": page.host }))
        })
    } else {
        let field = field.unwrap_or_default();
        let store = open()?;
        let item = resolve(
            &read_index(&store)?,
            "login",
            &page.host,
            account.as_deref(),
            "no_login",
        )?;
        if field == "totp" && item["totp"].as_bool() != Some(true) {
            return Err(Refusal::new(
                "no_totp",
                "the saved login has no one-time code secret; add it in the app's secure box first",
            ));
        }
        let id = item["id"].as_str().unwrap_or_default().to_string();
        let label = item["label"].as_str().unwrap_or_default().to_string();
        let bind = bind_for(&["pulse.login.fill.v1", &client, &page.host, &id]);
        let code_note = if item["totp"].as_bool() == Some(true) {
            " and its one-time code"
        } else {
            ""
        };
        let summary = format!(
            "{} wants the app to type the saved login \"{label}\"{code_note} into {}. The app checked that the page is {}; the agent never sees the password.",
            who(),
            page.host,
            page.host
        );
        authorize(
            origin,
            "login.fill",
            &bind,
            &summary,
            approval,
            LOGIN_GRANT,
            LOGIN_USES,
        )
        .and_then(|()| {
            // The value is read only now, after the page and the owner are settled.
            let stored = read_json(&store, &item_account(&id))?
                .ok_or_else(|| unavailable("a saved login is missing"))?;
            let damaged = || unavailable("a saved login is damaged");
            let value = if field == "totp" {
                // Computed now, after the approval and the page check, so it is fresh when typed.
                totp_now(&stored["totp"]).ok_or_else(damaged)?
            } else {
                text(&stored, field).ok_or_else(damaged)?.to_string()
            };
            let secure = Some(field == "password");
            type_verified(&page, &value, secure)
                .map(|landed| done(field, &page.host, &landed))
                .map_err(|failed| failed.refusal)
        })
    };
    let outcome = match &result {
        Ok(_) => "ok".to_string(),
        Err(refusal) => refusal.code.to_string(),
    };
    audit::record_secret(
        &client,
        "login.fill",
        &page.host,
        field.or(manager).unwrap_or(""),
        &outcome,
        None,
        None,
    );
    result
}

/// The value of one field of saved card `id`, read only once the purchase holds its reservation.
fn card_value(store: &rightkit_credentials::Store, id: &str, field: &str) -> Result<String> {
    let stored = read_json(store, &item_account(id))?
        .ok_or_else(|| unavailable("a saved card is missing"))?;
    let damaged = || unavailable("a saved card is damaged");
    Ok(match field {
        "number" => text(&stored, "number").ok_or_else(damaged)?.to_string(),
        "cvv" => text(&stored, "cvv").ok_or_else(damaged)?.to_string(),
        "name" => text(&stored, "name").ok_or_else(damaged)?.to_string(),
        "zip" => text(&stored, "zip").ok_or_else(damaged)?.to_string(),
        "expMonth" => format!("{:02}", stored["expMonth"].as_u64().ok_or_else(damaged)?),
        "expYear" => stored["expYear"].as_u64().ok_or_else(damaged)?.to_string(),
        _ => format!(
            "{:02}/{:02}",
            stored["expMonth"].as_u64().ok_or_else(damaged)?,
            stored["expYear"].as_u64().ok_or_else(damaged)? % 100
        ),
    })
}

/// `payment.fill`. `args`: `field` (`number`, `expMonth`, `expYear`, `exp` as MM/YY, `cvv`, `name`
/// or `zip`), `fields` (every field this checkout needs, `field` among them, the same list on every
/// call of the purchase), `amountCents` and `currency` (`USD`) of this purchase, `merchant` (the
/// agent's name for it, shown to the owner as the agent's claim), `card` (the label, when the site
/// has more than one), `approval`. Types the field of a saved virtual card into the focused field
/// of the verified checkout page, inside the card's limits (`Purchase`); answers `filled` with the
/// purchase's state (`open` with the fields left, `committed`, or `held` when the booking could
/// not be written and the hold stays counted) or a refusal, never a value. A failed fill ends the
/// purchase; when it may have typed something its message says the outcome is unknown.
pub fn payment_fill(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
    // Nothing types, launches or takes focus while the user holds the screen (spot takeover, #355).
    super::screen::user_in_control()?;
    local_only(origin, "payment.fill")?;
    settings.require_payments()?;
    require_engine()?;
    let field = args["field"].as_str().unwrap_or_default();
    if !matches!(
        field,
        "number" | "expMonth" | "expYear" | "exp" | "cvv" | "name" | "zip"
    ) {
        return Err(invalid(
            "field is number, expMonth, expYear, exp, cvv, name or zip",
        ));
    }
    let fields = purchase_fields(&args["fields"])?;
    if !fields.iter().any(|f| f == field) {
        return Err(invalid("field must be one of this purchase's fields"));
    }
    let amount = args["amountCents"]
        .as_u64()
        .filter(|n| (1..=MAX_CENTS).contains(n))
        .ok_or_else(|| invalid("amountCents is the whole purchase in cents, above 0"))?;
    if args["currency"].as_str().unwrap_or(CURRENCY) != CURRENCY {
        return Err(Refusal::new(
            "currency_not_supported",
            "The app pays in US dollars only",
        ));
    }
    let merchant = claim(args, "merchant", 80).unwrap_or_else(|| "(no name given)".to_string());
    let card_label = claim(args, "card", MAX_LABEL);
    let approval = args["approval"].as_str();
    let page = read_page()?;
    let client = approvals::current_client();
    let store = open()?;
    let item = resolve(
        &read_index(&store)?,
        "card",
        &page.host,
        card_label.as_deref(),
        "no_card",
    )?;
    let id = item["id"].as_str().unwrap_or_default().to_string();
    let label = item["label"].as_str().unwrap_or_default().to_string();
    let last4 = item["last4"].as_str().unwrap_or("????").to_string();
    let max = item["maxCents"].as_u64().unwrap_or(0);
    let auto = item["autoCents"].as_u64().unwrap_or(0);
    let month = item["monthCents"].as_u64().unwrap_or(0);
    let amount_text = amount.to_string();
    let field_set = fields.join(",");
    // One purchase: the chat, the page's exact address and host, the card, the amount and the
    // field set. The owner's approval is for this key and nothing else.
    let key = bind_for(&[
        "pulse.payment.purchase.v2",
        &client,
        &page.host,
        &page.url,
        &id,
        &amount_text,
        CURRENCY,
        &field_set,
    ]);
    let result = (|| -> Result<Value> {
        let tx = {
            let mut purchases = PURCHASES.lock().unwrap_or_else(|e| e.into_inner());
            sweep(&store, &mut purchases);
            let at = match purchases.iter().position(|p| p.key == key) {
                Some(at) => {
                    // A live purchase used again re-checks that its reservation still stands in
                    // the ledger (a ledger that was reset or cannot be read authorizes nothing
                    // more) and that the owner's month cap still covers everything counted.
                    let tx = purchases[at].tx.clone();
                    let lost = {
                        let _guard = LEDGER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
                        let ledger = Ledger::load(&store)?;
                        let standing = ledger.holds.iter().any(|h| {
                            h["tx"].as_str() == Some(tx.as_str()) && h["state"] == "reserved"
                        });
                        !standing || ledger.used_all() > settings.payments_month_cap_cents
                    };
                    if lost {
                        let p = purchases.remove(at);
                        end_purchase(
                            &store,
                            &p,
                            if p.typed { End::Unknown } else { End::Release },
                        );
                        return Err(Refusal::new(
                            "reservation_lost",
                            "this purchase's reservation no longer stands (or the monthly total you set is now exceeded), so it was ended; nothing more is typed",
                        ));
                    }
                    at
                }
                None => {
                    // The limits are checked and the amount reserved in one locked step, before
                    // anything is typed, so two calls cannot both pass on the same headroom.
                    let _guard = LEDGER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
                    let mut ledger = Ledger::load(&store)?;
                    let spent = ledger.used(&id);
                    let unknown = ledger.unknown_on(&id, &page.host);
                    let caps = Caps {
                        auto: settings.payments_auto_cap_cents,
                        month: settings.payments_month_cap_cents,
                        used_all: ledger.used_all(),
                    };
                    // What the owner is shown and what the gate applies: the lower of the card's
                    // own limit and the owner's global cap.
                    let auto = auto.min(caps.auto);
                    let gate = match spend_gate(max, auto, month, spent, amount, &caps) {
                        // After an unknown outcome on this site the owner decides, every time.
                        Gate::Auto if unknown => Gate::Ask,
                        gate => gate,
                    };
                    match gate {
                        Gate::Refuse(code, why) => {
                            return Err(Refusal::new(
                                code,
                                format!(
                                    "{why} ({} for this purchase; {} per purchase, {} used of {} this month); nothing was typed",
                                    dollars(amount),
                                    dollars(max),
                                    dollars(spent),
                                    dollars(month)
                                ),
                            ));
                        }
                        Gate::Auto => {}
                        Gate::Ask => {
                            let why = if unknown {
                                "An earlier payment with this card on this site ended without the app knowing whether it went through, so this one is yours to confirm.".to_string()
                            } else {
                                format!(
                                    "Above your {} no-question limit, so this one is yours to confirm.",
                                    dollars(auto)
                                )
                            };
                            let summary = format!(
                                "{} wants the app to pay {} {CURRENCY} at {} with your virtual card \"{label}\" ending {last4}, typing {} on this page only. The app checked that the page is {}; the amount and the shop's name are the agent's own words, which the app has not checked against the page. {why} Used {} of {} this month. The agent never sees the card.",
                                who(),
                                dollars(amount),
                                merchant,
                                fields.join(", "),
                                page.host,
                                dollars(spent),
                                dollars(month),
                            );
                            match approval {
                                Some(approval_id) => approvals::take_act_held(
                                    origin,
                                    "payment.fill",
                                    &key,
                                    approval_id,
                                )?,
                                None => {
                                    // The facts again as fields, for a card that shows them apart
                                    // from the summary: the agent's claims (merchant, amount) next
                                    // to what the host app knows (card, page, room, caps). Never a secret.
                                    let detail = json!({
                                        "merchant": merchant,
                                        "amountCents": amount,
                                        "cardLabel": label,
                                        "last4": last4,
                                        "host": page.host,
                                        "monthLeftCents": month.saturating_sub(spent),
                                        "autoCents": auto,
                                        "capCents": caps.auto,
                                    });
                                    return Err(approvals::request_act_held_detail(
                                        origin,
                                        "payment.fill",
                                        &key,
                                        &summary,
                                        detail,
                                    ));
                                }
                            }
                        }
                    }
                    let tx = new_id();
                    ledger.holds.push(json!({
                        "tx": tx, "card": id, "host": page.host, "cents": amount,
                        "state": "reserved", "run": run_id(), "month": ledger.month,
                    }));
                    // A reservation that cannot be written authorizes nothing.
                    ledger.save(&store)?;
                    purchases.push(Purchase {
                        key: key.clone(),
                        tx,
                        client: client.clone(),
                        host: page.host.clone(),
                        card: id.clone(),
                        last4: last4.clone(),
                        amount,
                        left: fields.clone(),
                        busy: false,
                        typed: false,
                        until: Instant::now() + PAYMENT_GRANT,
                    });
                    purchases.len() - 1
                }
            };
            let p = &mut purchases[at];
            if p.busy {
                return Err(Refusal::new(
                    "busy",
                    "another field of this purchase is being typed; call again when that one answers",
                ));
            }
            let Some(next) = p.left.iter().position(|f| f == field) else {
                return Err(Refusal::new(
                    "field_done",
                    "this purchase already typed that field; each field is typed once per purchase",
                ));
            };
            p.left.remove(next);
            p.busy = true;
            p.tx.clone()
        };
        let typed = card_value(&store, &id, field)
            .map_err(Failed::untyped)
            .and_then(|value| type_verified(&page, &value, None));
        let mut purchases = PURCHASES.lock().unwrap_or_else(|e| e.into_inner());
        // A purchase being typed is never swept, so it is still here.
        let at = purchases
            .iter()
            .position(|p| p.tx == tx)
            .ok_or_else(|| unavailable("the purchase being typed is gone"))?;
        purchases[at].busy = false;
        match typed {
            Ok(landed) => {
                purchases[at].typed = true;
                let mut answer = done(field, &page.host, &landed);
                if purchases[at].left.is_empty() {
                    let p = purchases.remove(at);
                    let booked = end_purchase(&store, &p, End::Commit);
                    answer["purchase"] = Value::from(if booked { "committed" } else { "held" });
                } else {
                    answer["purchase"] = Value::from("open");
                    answer["fieldsLeft"] = json!(purchases[at].left);
                }
                Ok(answer)
            }
            Err(failed) => {
                // A failed fill ends the purchase; it is never retried under the same grant.
                let p = purchases.remove(at);
                let maybe = p.typed || failed.maybe_typed;
                end_purchase(&store, &p, if maybe { End::Unknown } else { End::Release });
                let mut refusal = failed.refusal;
                refusal.message.push_str(if maybe {
                    "; this purchase is ended and its outcome is unknown: part of the card may already be on the page, so its amount stays counted against the month; do not start it again without the owner"
                } else {
                    "; this purchase is ended and nothing of the card was typed, so its reservation is released"
                });
                Err(refusal)
            }
        }
    })();
    let outcome = match &result {
        Ok(_) => "ok".to_string(),
        Err(refusal) => refusal.code.to_string(),
    };
    audit::record_secret(
        &client,
        "payment.fill",
        &page.host,
        field,
        &outcome,
        Some(amount),
        Some(&last4),
    );
    result
}
