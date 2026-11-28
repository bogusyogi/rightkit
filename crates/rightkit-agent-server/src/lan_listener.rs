//! `serve --lan`: a second listener on the machine's network address, and
//! `_{app}._tcp` mDNS advertising so `{app} discover` (and any DNS-SD browser) finds it.
//!
//! The LAN listener serves the same router as loopback behind `mark_lan` (which
//! marks every request [`LanOrigin`](crate::guest_shares::LanOrigin)) and the LAN
//! request-rate limit (`auth::lan_rate_limit`, 200 per second, burst 400):
//! `require_token` refuses the owner bearer there, so only guest links (`{app} share`),
//! `GET /health` and the web client page work from the network. Traffic is plain
//! HTTP; put a TLS proxy in front for an untrusted network.
//!
//! The listener exists only on a host that turns both `host_options::lan_listener` and
//! `host_options::guest_shares` on: without guest links it would serve nothing.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::OnceLock;
use std::time::Duration;

/// The longest DNS-SD service name (RFC 6763 section 7.2), without the leading underscore.
const MAX_SERVICE_NAME_CHARS: usize = 15;
/// The longest DNS label.
const MAX_HOST_LABEL_CHARS: usize = 63;

/// The DNS-SD service type this host advertises and browses: `_{app}._tcp.local.`.
pub(crate) fn service_type() -> Result<String, String> {
    let app = crate::host_identity::app();
    if app.chars().count() > MAX_SERVICE_NAME_CHARS {
        return Err(format!(
            "the app name {app:?} is longer than the {MAX_SERVICE_NAME_CHARS} characters a DNS-SD service name allows"
        ));
    }
    Ok(format!("_{app}._tcp.local."))
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct LanOptions {
    pub(crate) port: u16,
    pub(crate) advertise: bool,
}

/// Set by `{app} serve --lan` before the daemon starts.
pub(crate) static OPTIONS: OnceLock<LanOptions> = OnceLock::new();
static MDNS: OnceLock<rightkit_daemon::lan_discovery::Advertisement> = OnceLock::new();

/// Whether this host may run a LAN listener at all.
pub(crate) fn lan_enabled() -> bool {
    crate::host_options::lan_listener() && crate::host_options::guest_shares()
}

/// The address other machines reach this one on: the source address of the
/// default route (a UDP `connect` sends no packet).
pub(crate) fn lan_ip() -> Option<IpAddr> {
    rightkit_daemon::lan_discovery::lan_ip()
}

/// The only paths the LAN listener answers: the web client's own files, the health probe and
/// what a guest link can use. Everything else (including routes outside the bearer gate, such
/// as the driver gateway) is 404 from the network.
fn lan_path_allowed(path: &str) -> bool {
    if matches!(path, "/health" | "/guest/whoami") || crate::api_web::is_web_path(path) {
        return true;
    }
    let Some(rest) = path.strip_prefix("/sessions/") else {
        return false;
    };
    matches!(
        rest.split_once('/').map(|(_, tail)| tail),
        Some(
            "events" | "events/history" | "events/replay" | "resume" | "tool-approval" | "pending"
        )
    )
}

/// Every credential a request presents: the `Authorization` bearer and the `access_token` query
/// parameter. The approval check below looks at both, so a link cannot dodge it by travelling in
/// the query string of the event-stream route.
fn presented_tokens(req: &axum::extract::Request) -> Vec<String> {
    let mut tokens = Vec::new();
    if let Some(bearer) = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    {
        tokens.push(bearer.trim().to_owned());
    }
    if let Some(query) = req.uri().query() {
        tokens.extend(
            query
                .split('&')
                .filter_map(|pair| pair.strip_prefix("access_token="))
                .map(str::to_owned),
        );
    }
    tokens
}

async fn mark_lan(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if !lan_path_allowed(req.uri().path()) {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    }
    // A link that can approve tool calls must not cross cleartext HTTP, where
    // anyone on the network can read and replay it.
    if presented_tokens(&req)
        .iter()
        .filter_map(|token| crate::guest_shares::resolve(token))
        .any(|guest| guest.can_approve)
    {
        return (
            axum::http::StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({"error": {"code": "approval_link_needs_tls", "message": "a guest link with approval rights is refused over the plain-HTTP LAN listener; use it through an encrypted tunnel to the loopback daemon, or share a read-only link"}})),
        )
            .into_response();
    }
    req.extensions_mut().insert(crate::guest_shares::LanOrigin);
    next.run(req).await
}

/// Bind and serve the LAN listener; advertise it when asked. Returns the
/// address guests use.
pub(crate) async fn start(app: axum::Router, options: LanOptions) -> Result<SocketAddr, String> {
    if !lan_enabled() {
        return Err("this host does not offer a LAN listener".to_string());
    }
    let ip = lan_ip().ok_or_else(|| "no network address found for --lan".to_string())?;
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, options.port))
        .await
        .map_err(|error| format!("cannot bind the LAN port {}: {error}", options.port))?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    let address = SocketAddr::new(ip, port);
    if crate::guest_shares::LAN_ADDR.set(address).is_err() {
        tracing::warn!(
            event = "lan_listener_address_kept",
            "the LAN address was already recorded; the first one stays"
        );
    }
    let app = app
        .layer(axum::middleware::from_fn_with_state(
            crate::auth::LanRateLimiter::new(),
            crate::auth::lan_rate_limit,
        ))
        .layer(axum::middleware::from_fn(mark_lan));
    let listener = crate::listener_guard::Guarded::new(
        listener,
        crate::listener_guard::LAN_MAX_CONNECTIONS,
        crate::listener_guard::HEAD_TIMEOUT,
    );
    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            tracing::warn!(event = "lan_listener_failed", %error, "LAN listener stopped");
        }
    });
    tracing::info!(event = "lan_listener_started", address = %address, "LAN listener");
    if options.advertise {
        if let Err(error) = advertise(ip, port) {
            tracing::warn!(event = "mdns_advertise_failed", %error, "mDNS advertise failed");
            eprintln!("[serve] mDNS advertising failed: {error}");
        }
    }
    Ok(address)
}

/// A DNS-label-safe form of `name`: ASCII letters, digits and `-`, at most 63 characters.
fn dns_label(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .take(MAX_HOST_LABEL_CHARS)
        .collect()
}

fn host_label() -> String {
    dns_label(
        &std::fs::read_to_string("/etc/hostname")
            .ok()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .or_else(|| std::env::var("HOSTNAME").ok())
            .unwrap_or_else(|| crate::host_identity::app().to_string()),
    )
}

fn advertise(ip: IpAddr, port: u16) -> Result<(), String> {
    let service = service_type()?;
    let host = host_label();
    let properties = [
        ("version", env!("CARGO_PKG_VERSION")),
        ("path", "/web"),
        ("auth", "guest-link"),
    ];
    let advertisement = rightkit_daemon::lan_discovery::advertise(
        &service,
        &format!("{} on {host}", rightkit_agent_config::app_name()),
        &format!("{host}.local."),
        ip,
        port,
        &properties[..],
    )?;
    if let Err(unused) = MDNS.set(advertisement) {
        // A second advertisement is not kept: dropping it withdraws it.
        drop(unused);
        tracing::warn!(
            event = "mdns_advertisement_kept",
            "an advertisement is already running; the first one stays"
        );
    }
    tracing::info!(event = "mdns_advertised", service = %service, port, "mDNS");
    Ok(())
}

/// One daemon found by `{app} discover`.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Found {
    pub(crate) name: String,
    pub(crate) host: String,
    pub(crate) addresses: Vec<String>,
    pub(crate) port: u16,
    pub(crate) version: Option<String>,
    pub(crate) url: String,
}

/// Text another machine advertised, safe to print: control characters (which a terminal would
/// act on) are dropped.
fn printable(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// `address` as the host part of a URL: an IPv6 literal needs brackets.
fn url_host(address: &str) -> String {
    if address.contains(':') && !address.starts_with('[') {
        format!("[{address}]")
    } else {
        address.to_string()
    }
}

/// Browse `_{app}._tcp` for `wait`.
pub(crate) fn discover(wait: Duration) -> Result<Vec<Found>, String> {
    let service = service_type()?;
    let mut found: Vec<Found> = Vec::new();
    for info in rightkit_daemon::lan_discovery::browse(&service, wait)? {
        let Some(first) = info.addresses.first().cloned() else {
            continue;
        };
        let row = Found {
            name: printable(
                info.fullname
                    .trim_end_matches(service.as_str())
                    .trim_end_matches('.'),
            ),
            host: printable(&info.hostname),
            port: info.port,
            version: info
                .properties
                .get("version")
                .map(|version| printable(version)),
            url: format!("http://{}:{}/web", url_host(&first), info.port),
            addresses: info.addresses.iter().map(|a| printable(a)).collect(),
        };
        if !found.iter().any(|seen| seen.name == row.name) {
            found.push(row);
        }
    }
    Ok(found)
}

#[cfg(test)]
mod lan_path_tests {
    use super::{dns_label, lan_path_allowed, presented_tokens, printable, url_host};

    #[test]
    fn the_pending_list_is_reachable_but_other_session_routes_are_not() {
        assert!(lan_path_allowed("/sessions/abc/pending"));
        assert!(lan_path_allowed("/sessions/abc/tool-approval"));
        assert!(!lan_path_allowed("/sessions/abc/messages"));
        assert!(!lan_path_allowed("/sessions/abc"));
        assert!(!lan_path_allowed("/shares"));
        assert!(!lan_path_allowed("/triggers/x/deliver/generic"));
        assert!(lan_path_allowed("/health"));
        assert!(lan_path_allowed("/guest/whoami"));
    }

    #[test]
    fn a_token_in_the_query_string_is_seen_as_well_as_the_bearer() {
        let request = axum::http::Request::builder()
            .uri("/sessions/abc/events?x=1&access_token=g_one")
            .header("authorization", "Bearer  g_two ")
            .body(axum::body::Body::empty())
            .expect("request");
        assert_eq!(presented_tokens(&request), vec!["g_two", "g_one"]);
    }

    #[test]
    fn advertised_text_is_made_safe_before_it_is_printed() {
        assert_eq!(printable("ok\u{1b}[31mred\n"), "ok[31mred");
        assert_eq!(url_host("192.168.1.5"), "192.168.1.5");
        assert_eq!(url_host("fe80::1"), "[fe80::1]");
        assert_eq!(url_host("[fe80::1]"), "[fe80::1]");
        assert_eq!(dns_label("my host.local"), "my-host-local");
        assert_eq!(dns_label(&"a".repeat(100)).len(), 63);
    }
}
