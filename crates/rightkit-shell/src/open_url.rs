//! Standalone "open URL" hook: usable without the full Shell plugin.
//!
//! One process-wide hub receives every URL the OS hands the app and fans it out to handlers:
//!
//! - macOS / iOS: Apple Event URLs (`tauri::RunEvent::Opened { urls }`) through
//!   [`handle_run_event`].
//! - Windows / Linux: URLs on the command line at cold start ([`capture_launch_args`]) and URLs on
//!   the argv of a second launch, forwarded by [`crate::instance`] ([`deliver_second_instance`],
//!   or [`init_single_instance`] which wires both).
//!
//! URLs that arrive before any handler is registered (cold start) are buffered and readable with
//! [`current_urls`]; [`on_open_url`] handlers receive URLs that arrive after registration. The
//! same URL delivered twice within [`DEDUP_WINDOW`] (for example argv and an Apple Event for one
//! click) reaches handlers once.
//!
//! Scheme registration (`mailto:`, custom schemes) stays with the app's installer or bundle
//! (`CFBundleURLTypes`, NSIS/MSI protocol entries, `.desktop` MIME types). This module only
//! delivers what the OS already routed to the process. The full Shell plugin feeds the same hub,
//! so handlers registered here also see its deliveries.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::instance::{self, Arbitration};

/// The same URL seen again inside this window is a duplicate delivery.
pub const DEDUP_WINDOW: Duration = Duration::from_secs(2);
const MAX_URL_LEN: usize = 4096;

/// Receives a batch of URLs that arrived together.
pub type UrlHandler = Arc<dyn Fn(&[String]) + Send + Sync>;

/// True for `scheme:rest` where the scheme is at least two characters, so Windows drive paths
/// such as `C:\mail` are not URLs. Control characters and over-long input are rejected.
pub fn is_url(raw: &str) -> bool {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_URL_LEN || raw.chars().any(char::is_control) {
        return false;
    }
    let Some((scheme, rest)) = raw.split_once(':') else {
        return false;
    };
    scheme.len() >= 2
        && !rest.is_empty()
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// The URL-shaped entries of a command line (flags, paths and drive letters are skipped).
pub fn urls_from_args<I, S>(args: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .map(|a| a.as_ref().trim().to_owned())
        .filter(|a| is_url(a))
        .collect()
}

/// Queue, de-dup and dispatch logic with an injectable clock. [`hub`] owns the process-wide one.
#[derive(Default)]
pub struct UrlHub {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Arrived before the first handler was registered.
    cold: Vec<String>,
    handlers: Vec<UrlHandler>,
    recent: Vec<(String, Instant)>,
}

impl UrlHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Deliver URLs. Invalid and duplicate entries are dropped; the rest are buffered (no handler
    /// yet) or passed to every handler in registration order, outside the hub lock. Returns how
    /// many URLs were new.
    pub fn deliver_at<I, S>(&self, urls: I, now: Instant) -> usize
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let (fresh, handlers) = {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            g.recent
                .retain(|(_, at)| now.saturating_duration_since(*at) < DEDUP_WINDOW);
            let mut fresh: Vec<String> = Vec::new();
            for raw in urls {
                let raw = raw.as_ref().trim();
                if !is_url(raw) || g.recent.iter().any(|(u, _)| u == raw) {
                    continue;
                }
                g.recent.push((raw.to_owned(), now));
                fresh.push(raw.to_owned());
            }
            if fresh.is_empty() {
                return 0;
            }
            if g.handlers.is_empty() {
                g.cold.extend(fresh.iter().cloned());
                return fresh.len();
            }
            (fresh, g.handlers.clone())
        };
        for h in handlers {
            h(&fresh);
        }
        fresh.len()
    }

    pub fn deliver<I, S>(&self, urls: I) -> usize
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.deliver_at(urls, Instant::now())
    }

    /// Register a handler for URLs that arrive from now on. Cold-start URLs stay readable through
    /// [`UrlHub::current_urls`]; they are not replayed.
    pub fn on_open_url(&self, handler: UrlHandler) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .handlers
            .push(handler);
    }

    /// URLs that arrived before the first handler was registered, in arrival order.
    pub fn current_urls(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cold
            .clone()
    }
}

/// The process-wide hub.
pub fn hub() -> &'static UrlHub {
    static HUB: OnceLock<UrlHub> = OnceLock::new();
    HUB.get_or_init(UrlHub::new)
}

/// Register a handler for URLs the OS opens in this app from now on.
pub fn on_open_url(handler: impl Fn(&[String]) + Send + Sync + 'static) {
    hub().on_open_url(Arc::new(handler));
}

/// URLs that arrived before the first [`on_open_url`] handler was registered (cold start).
pub fn current_urls() -> Vec<String> {
    hub().current_urls()
}

/// Deliver URLs from any source the app owns (for example a custom IPC). Returns how many were new.
pub fn deliver<I, S>(urls: I) -> usize
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    hub().deliver(urls)
}

/// Deliver the URLs on this process's command line (Windows/Linux cold start). Call once, early.
pub fn capture_launch_args() -> usize {
    deliver(urls_from_args(std::env::args().skip(1)))
}

/// Deliver the URLs on a second launch's argv (`argv[0]` is the executable and is skipped).
pub fn deliver_second_instance(argv: &[String]) -> usize {
    deliver(urls_from_args(argv.iter().skip(1)))
}

/// Single-instance arbitration wired to this hub, with no Shell plugin. The primary process
/// captures its own launch URLs; a second launch forwards its argv to the primary and the caller
/// must exit on [`Arbitration::Secondary`]. Keep the `Primary` guard alive for the process
/// lifetime.
pub fn init_single_instance(app_id: &str, version: &str) -> Arbitration {
    let arb = instance::arbitrate(
        app_id,
        version,
        Arc::new(|s| {
            deliver_second_instance(&s.argv);
        }),
    );
    if !matches!(arb, Arbitration::Secondary) {
        capture_launch_args();
    }
    arb
}

/// Feed Apple Event URLs from the Tauri run loop: call it for every event in
/// `app.run(|_, event| rightkit_shell::open_url::handle_run_event(&event))`. A no-op elsewhere.
pub fn handle_run_event(event: &tauri::RunEvent) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    if let tauri::RunEvent::Opened { urls } = event {
        deliver(urls.iter().map(|u| u.to_string()));
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    let _ = event;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    type Seen = Arc<StdMutex<Vec<Vec<String>>>>;

    fn recorder() -> (UrlHandler, Seen) {
        let seen: Seen = Arc::new(StdMutex::new(Vec::new()));
        let s = seen.clone();
        (
            Arc::new(move |u: &[String]| s.lock().unwrap().push(u.to_vec())),
            seen,
        )
    }

    #[test]
    fn url_shape() {
        assert!(is_url("mailto:a@b.c"));
        assert!(is_url("mailright://open/thread/1"));
        assert!(is_url(" https://x.test "));
        assert!(!is_url(r"C:\mail\x.eml"));
        assert!(!is_url("C:/mail"));
        assert!(!is_url("--background"));
        assert!(!is_url("plain"));
        assert!(!is_url("mailto:"));
        assert!(!is_url("a:b\nc"));
        assert!(!is_url(&format!("x-y:{}", "a".repeat(5000))));
    }

    #[test]
    fn args_filtered() {
        let got = urls_from_args(["--background", r"C:\x", "mailto:a@b.c", " app://z "]);
        assert_eq!(got, ["mailto:a@b.c", "app://z"]);
    }

    #[test]
    fn cold_start_buffers_in_order_without_handler() {
        let hub = UrlHub::new();
        let t = Instant::now();
        assert_eq!(hub.deliver_at(["mailto:a@b.c", "app://one"], t), 2);
        assert_eq!(hub.deliver_at(["app://two"], t), 1);
        assert_eq!(
            hub.current_urls(),
            ["mailto:a@b.c", "app://one", "app://two"]
        );
    }

    #[test]
    fn cold_buffer_dedups() {
        let hub = UrlHub::new();
        let t = Instant::now();
        hub.deliver_at(["app://one"], t);
        assert_eq!(hub.deliver_at(["app://one", "app://one"], t), 0);
        assert_eq!(hub.current_urls(), ["app://one"]);
    }

    #[test]
    fn handler_gets_only_later_urls_and_current_urls_is_stable() {
        let hub = UrlHub::new();
        let t = Instant::now();
        hub.deliver_at(["app://cold"], t);
        let (h, seen) = recorder();
        hub.on_open_url(h);
        assert!(seen.lock().unwrap().is_empty());
        hub.deliver_at(["app://warm"], t);
        assert_eq!(*seen.lock().unwrap(), vec![vec!["app://warm".to_owned()]]);
        assert_eq!(hub.current_urls(), ["app://cold"]);
    }

    #[test]
    fn handlers_run_in_registration_order() {
        let hub = UrlHub::new();
        let order = Arc::new(StdMutex::new(Vec::new()));
        for n in 0..3 {
            let o = order.clone();
            hub.on_open_url(Arc::new(move |_| o.lock().unwrap().push(n)));
        }
        hub.deliver(["app://x"]);
        assert_eq!(*order.lock().unwrap(), [0, 1, 2]);
    }

    #[test]
    fn duplicate_inside_window_dropped_then_allowed_after() {
        let hub = UrlHub::new();
        let (h, seen) = recorder();
        hub.on_open_url(h);
        let t = Instant::now();
        assert_eq!(hub.deliver_at(["app://x"], t), 1);
        assert_eq!(
            hub.deliver_at(["app://x"], t + Duration::from_millis(500)),
            0
        );
        assert_eq!(hub.deliver_at(["app://x"], t + DEDUP_WINDOW), 1);
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[test]
    fn batch_arrives_together_and_invalid_is_dropped() {
        let hub = UrlHub::new();
        let (h, seen) = recorder();
        hub.on_open_url(h);
        hub.deliver(["app://a", "not a url", "app://b"]);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![vec!["app://a".to_owned(), "app://b".to_owned()]]
        );
    }

    #[test]
    fn handler_may_reenter_the_hub() {
        let hub = Arc::new(UrlHub::new());
        let inner = hub.clone();
        hub.on_open_url(Arc::new(move |_| {
            let _ = inner.current_urls();
        }));
        assert_eq!(hub.deliver(["app://x"]), 1);
    }

    #[test]
    fn second_instance_argv_skips_executable() {
        let argv = ["C:/app.exe".to_owned(), "mailto:z@y.x".to_owned()];
        assert_eq!(urls_from_args(argv.iter().skip(1)), ["mailto:z@y.x"]);
    }
}
