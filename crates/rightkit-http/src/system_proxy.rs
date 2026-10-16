//! The proxy the operating system is configured with, for [`ProxyMode::System`].
//!
//! Windows keeps a per-user proxy configuration (Settings > Network > Proxy, the old
//! "Internet Options"): a static proxy, a bypass list, a proxy auto-config (PAC) script URL
//! and an auto-detect flag. This module reads the static proxy and its bypass list through
//! `WinHttpGetIEProxyConfigForCurrentUser` and works out, per request scheme, which endpoint
//! Windows would use.
//!
//! A client here uses one proxy for every request it makes, so the result is one of three
//! things ([`SystemProxyChoice`]): no proxy, one proxy that serves both `http` and `https`
//! with a bypass list of plain hosts, or a configuration this client cannot follow exactly.
//! The last is reported with the first element it cannot follow, never approximated: no
//! entry is dropped while the proxy stays in use, and no scheme is sent to the endpoint
//! configured for the other. That covers endpoints that differ by scheme, a bypass entry
//! limited to one scheme, `<local>`, a wildcard this client cannot express and a PAC script
//! (which chooses a proxy per URL). Automatic detection with no static proxy and no PAC script
//! is [`SystemProxyChoice::AutoDetectOnly`], not an error; with a static proxy the flag is ignored.
//!
//! [`ProxyMode::System`]: crate::ProxyMode::System

/// The operating system's proxy configuration, as the OS stores it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SystemProxySettings {
    /// `host:port`, or a per-protocol list such as `http=a:80;https=b:443`.
    pub(crate) proxy: Option<String>,
    /// Hosts that connect directly, separated by `;` or whitespace. `<local>` and `*`
    /// wildcards are allowed by the OS, and an entry may be limited to one scheme
    /// (`https://host`).
    pub(crate) bypass: Option<String>,
    /// A proxy auto-config (PAC) script is configured.
    pub(crate) auto_config: bool,
    /// The PAC script URL, when one is set. Only [`ProxyMode::SystemForUrl`] uses it.
    ///
    /// [`ProxyMode::SystemForUrl`]: crate::ProxyMode::SystemForUrl
    pub(crate) auto_config_url: Option<String>,
    /// "Automatically detect settings" is on.
    pub(crate) auto_detect: bool,
}

/// Where one URL goes, as the operating system routes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UrlRoute {
    /// Connect without a proxy.
    Direct,
    /// Through this `http://` or `https://` proxy URL.
    Proxy(String),
}

/// A request URL reduced to what proxy routing needs: lowercase scheme and host (no brackets).
pub(crate) fn scheme_and_host(url: &str) -> Option<(String, String)> {
    let uri: ureq::http::Uri = url.parse().ok()?;
    let scheme = uri.scheme_str()?.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = uri
        .host()?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    (!host.is_empty()).then_some((scheme, host))
}

/// Case-insensitive match of `text` against `pattern`, where `*` stands for any run of
/// characters (including none).
fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if let Some((sp, st)) = star {
            p = sp + 1;
            t = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// Whether the Windows bypass list sends `scheme://host` around the proxy.
///
/// An entry is `<local>` (a name with no dot), a host with `*` wildcards anywhere, a leading
/// `.suffix`, or any of those limited to one scheme (`https://host`). A port on an entry is
/// not compared: the entry then covers the host on every port.
pub(crate) fn bypass_matches(bypass: &str, scheme: &str, host: &str) -> bool {
    bypass
        .split(|c: char| c == ';' || c.is_whitespace())
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .any(|entry| {
            if entry.eq_ignore_ascii_case("<local>") {
                return !host.contains('.') && !host.contains(':');
            }
            let (entry_scheme, rest) = match entry.split_once("://") {
                Some((s, rest)) => (Some(s), rest),
                None => (None, entry),
            };
            if entry_scheme.is_some_and(|s| !s.eq_ignore_ascii_case(scheme)) {
                return false;
            }
            let rest = rest.trim_end_matches('/');
            // Drop a trailing `:port` (but not the colons of an IPv6 literal).
            let rest = match rest.rsplit_once(':') {
                Some((head, port))
                    if !port.is_empty()
                        && port.chars().all(|c| c.is_ascii_digit())
                        && !head.contains(':') =>
                {
                    head
                }
                _ => rest,
            };
            let rest = rest.trim_start_matches('[').trim_end_matches(']');
            if rest.is_empty() {
                return false;
            }
            match rest.strip_prefix('.') {
                Some(suffix) if !suffix.contains('*') => {
                    let host = host.to_ascii_lowercase();
                    let suffix = suffix.to_ascii_lowercase();
                    host == suffix || host.ends_with(&format!(".{suffix}"))
                }
                _ => glob_match(rest, host),
            }
        })
}

/// Where the static proxy settings send `scheme://host`, per URL and exact: the bypass list
/// is evaluated for this host, and the scheme's own endpoint is used (a scheme with none
/// connects directly). `None` when no static proxy usable by this crate is configured, so the
/// caller can fall back to the environment.
pub(crate) fn static_route_for_url(
    settings: &SystemProxySettings,
    scheme: &str,
    host: &str,
) -> Option<UrlRoute> {
    let raw = settings.proxy.as_deref().unwrap_or_default().trim();
    let (http, https) = endpoints_by_scheme(raw);
    if http.is_none() && https.is_none() {
        return None;
    }
    let bypass = settings.bypass.as_deref().unwrap_or_default();
    if bypass_matches(bypass, scheme, host) {
        return Some(UrlRoute::Direct);
    }
    let endpoint = if scheme == "https" { https } else { http };
    Some(endpoint.map_or(UrlRoute::Direct, UrlRoute::Proxy))
}

/// The route for `scheme` from a WinHTTP proxy string (`host:port`, a `;`-separated list of
/// candidates, or `http=a;https=b`): the first candidate for the scheme. `None` string is
/// direct.
pub(crate) fn route_from_proxy_string(proxy: Option<&str>, scheme: &str) -> UrlRoute {
    let (http, https) = endpoints_by_scheme(proxy.unwrap_or_default());
    let endpoint = if scheme == "https" { https } else { http };
    endpoint.map_or(UrlRoute::Direct, UrlRoute::Proxy)
}

/// What the settings mean for a client that uses one proxy for every request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SystemProxyChoice {
    /// Nothing is configured (or only entries for protocols this crate does not speak, such
    /// as `socks=` and `ftp=`).
    None,
    /// One endpoint serves both `http` and `https` requests, and every bypass entry is a
    /// host that holds for both.
    Single {
        /// The proxy URL, always `http://` or `https://`.
        url: String,
        /// `NO_PROXY`-style entries for the hosts that connect directly.
        no_proxy: Vec<String>,
    },
    /// "Automatically detect settings" is on and nothing else is configured: no static
    /// proxy string and no PAC script. This is the Windows default. There is nothing the
    /// user named that this client would be ignoring, so it is not an error; the caller
    /// falls back to the environment proxy, then to a direct connection, and reports it.
    AutoDetectOnly,
    /// The configuration holds something this client cannot follow exactly. Carries the
    /// first such element, in words a person can match against the Windows proxy settings.
    Unsupported(String),
}

/// The endpoint each request scheme uses, as Windows resolves a static proxy string.
///
/// A per-protocol entry (`http=`, `https=`) serves that scheme. A bare `host:port` serves every
/// scheme that has no entry of its own. A scheme with neither connects directly. `socks=` and
/// `ftp=` entries are ignored: this crate speaks to `http` and `https` proxies only. An entry
/// without a scheme is an `http` proxy, as Windows treats it; an entry whose own scheme is
/// neither `http` nor `https` is unusable and reads as no entry.
///
/// Returns `(http endpoint, https endpoint)`, each a proxy URL.
pub(crate) fn endpoints_by_scheme(proxy: &str) -> (Option<String>, Option<String>) {
    let mut any = None;
    let mut http = None;
    let mut https = None;
    for entry in proxy
        .split(|c: char| c == ';' || c.is_whitespace())
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        match entry.split_once('=') {
            Some((protocol, value)) => match protocol.trim().to_ascii_lowercase().as_str() {
                "http" => http = http.or(Some(value.trim())),
                "https" => https = https.or(Some(value.trim())),
                _ => {}
            },
            None => any = any.or(Some(entry)),
        }
    }
    let url = |chosen: Option<&str>| {
        let chosen = chosen.filter(|v| !v.is_empty())?;
        let url = if chosen.contains("://") {
            chosen.to_string()
        } else {
            format!("http://{chosen}")
        };
        let scheme = url
            .split("://")
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        (scheme == "http" || scheme == "https").then_some(url)
    };
    // An explicit entry that is unusable does not fall back to the bare one: the user named a
    // different proxy for that scheme, and using another would be a guess.
    let http = if http.is_some() { url(http) } else { url(any) };
    let https = if https.is_some() {
        url(https)
    } else {
        url(any)
    };
    (http, https)
}

/// The bypass list as `NO_PROXY` entries: a host name as it is, `*.example.com` as
/// `.example.com`.
///
/// `Err` with the first entry that has no such form, because a client that used the proxy
/// without it would send through the proxy what the user configured as direct:
///
/// * an entry limited to one scheme (`https://host`): a by-host bypass would also apply to
///   the other scheme;
/// * `<local>` (names without a dot);
/// * a wildcard anywhere but in a leading `*.` (`10.*`, `*`).
pub(crate) fn bypass_hosts(bypass: &str) -> Result<Vec<String>, String> {
    let mut no_proxy = Vec::new();
    for entry in bypass
        .split(|c: char| c == ';' || c.is_whitespace())
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        let host = entry.strip_prefix('*').unwrap_or(entry);
        let expressible = !entry.contains("://")
            && !entry.contains('<')
            && !host.contains('*')
            && !host.is_empty()
            && host != "."
            && (host.len() == entry.len() || host.starts_with('.'));
        if !expressible {
            return Err(entry.to_string());
        }
        if !no_proxy.iter().any(|known: &String| known == host) {
            no_proxy.push(host.to_string());
        }
    }
    Ok(no_proxy)
}

/// What `settings` mean for a client with one proxy for every request.
pub(crate) fn proxy_choice(settings: &SystemProxySettings) -> SystemProxyChoice {
    // A PAC script chooses a proxy per URL: refused, whatever else is set.
    if settings.auto_config {
        return SystemProxyChoice::Unsupported("a proxy auto-config script is set".into());
    }
    let raw = settings.proxy.as_deref().unwrap_or_default().trim();
    // Automatic detection with no static proxy names nothing to follow. With a static proxy
    // the flag is ignored and the static proxy is followed exactly (or refused below).
    if settings.auto_detect && raw.is_empty() {
        return SystemProxyChoice::AutoDetectOnly;
    }
    let (http, https) = endpoints_by_scheme(raw);
    match (http, https) {
        (None, None) => SystemProxyChoice::None,
        (Some(http), Some(https)) if http.eq_ignore_ascii_case(&https) => {
            match bypass_hosts(settings.bypass.as_deref().unwrap_or_default()) {
                Ok(no_proxy) => SystemProxyChoice::Single {
                    url: http,
                    no_proxy,
                },
                Err(entry) => SystemProxyChoice::Unsupported(format!(
                    "the bypass entry `{entry}` cannot be followed exactly"
                )),
            }
        }
        _ => {
            SystemProxyChoice::Unsupported("HTTP and HTTPS are given different proxy routes".into())
        }
    }
}

/// The current user's proxy configuration, or the default (nothing configured) on a
/// system without one.
#[cfg(not(windows))]
pub(crate) fn system_proxy_settings() -> SystemProxySettings {
    SystemProxySettings::default()
}

/// The current user's proxy configuration from WinHTTP's view of the Internet
/// settings. A failed query reads as nothing configured.
#[cfg(windows)]
pub(crate) fn system_proxy_settings() -> SystemProxySettings {
    use win::take;

    /// `WINHTTP_CURRENT_USER_IE_PROXY_CONFIG`.
    #[repr(C)]
    struct IeProxyConfig {
        auto_detect: i32,
        auto_config_url: *mut u16,
        proxy: *mut u16,
        proxy_bypass: *mut u16,
    }
    #[link(name = "winhttp")]
    unsafe extern "system" {
        fn WinHttpGetIEProxyConfigForCurrentUser(config: *mut IeProxyConfig) -> i32;
    }

    let mut config = IeProxyConfig {
        auto_detect: 0,
        auto_config_url: std::ptr::null_mut(),
        proxy: std::ptr::null_mut(),
        proxy_bypass: std::ptr::null_mut(),
    };
    // SAFETY: `config` is a valid out-parameter of the layout the function fills.
    if unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut config) } == 0 {
        return SystemProxySettings::default();
    }
    // SAFETY: the call succeeded, so each pointer is null or an OS-allocated string that is
    // taken (and freed) exactly once here.
    let (pac, proxy, bypass) = unsafe {
        (
            take(config.auto_config_url),
            take(config.proxy),
            take(config.proxy_bypass),
        )
    };
    let auto_config_url = pac.map(|url| url.trim().to_string()).filter(|u| !u.is_empty());
    SystemProxySettings {
        proxy,
        bypass,
        auto_config: auto_config_url.is_some(),
        auto_config_url,
        auto_detect: config.auto_detect != 0,
    }
}

/// Windows string and handle helpers shared by the proxy queries.
#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalFree(memory: *mut c_void) -> *mut c_void;
    }

    /// Copy a NUL-terminated UTF-16 string the OS allocated, then free it.
    ///
    /// # Safety
    /// `text` is null or a NUL-terminated string allocated with `GlobalAlloc` that nothing
    /// else frees.
    pub(super) unsafe fn take(text: *mut u16) -> Option<String> {
        if text.is_null() {
            return None;
        }
        let mut len = 0;
        // SAFETY: the string is NUL-terminated, so every offset up to the NUL is readable.
        while unsafe { *text.add(len) } != 0 {
            len += 1;
        }
        // SAFETY: `len` code units were just read from this allocation.
        let value = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
        // SAFETY: the OS documents that the caller frees these strings with GlobalFree.
        unsafe { GlobalFree(text.cast()) };
        Some(value)
    }
}

/// No resolver off Windows: there is no automatic or PAC configuration to evaluate.
#[cfg(not(windows))]
pub(crate) fn winhttp_route_for_url(_url: &str, _settings: &SystemProxySettings) -> Option<UrlRoute> {
    None
}

/// Ask WinHTTP's own resolver (`WinHttpGetProxyForUrl`) which proxy `url` uses, evaluating
/// the PAC script from the system settings and/or automatic detection (WPAD), as WinHTTP's
/// `WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY` does. `None` when the resolver failed (no PAC
/// found, script unreachable, no WPAD): the caller decides what that means.
#[cfg(windows)]
pub(crate) fn winhttp_route_for_url(url: &str, settings: &SystemProxySettings) -> Option<UrlRoute> {
    use std::ffi::c_void;

    const ACCESS_TYPE_NO_PROXY: u32 = 1;
    const AUTOPROXY_AUTO_DETECT: u32 = 0x0000_0001;
    const AUTOPROXY_CONFIG_URL: u32 = 0x0000_0002;
    const AUTO_DETECT_DHCP: u32 = 0x0000_0001;
    const AUTO_DETECT_DNS_A: u32 = 0x0000_0002;
    const ACCESS_TYPE_NAMED_PROXY: u32 = 3;
    /// Bound on each phase of a PAC download or WPAD probe, in milliseconds.
    const TIMEOUT_MS: i32 = 5000;

    /// `WINHTTP_AUTOPROXY_OPTIONS`.
    #[repr(C)]
    struct AutoProxyOptions {
        flags: u32,
        auto_detect_flags: u32,
        auto_config_url: *const u16,
        reserved: *mut c_void,
        reserved2: u32,
        auto_logon_if_challenged: i32,
    }
    /// `WINHTTP_PROXY_INFO`.
    #[repr(C)]
    struct ProxyInfo {
        access_type: u32,
        proxy: *mut u16,
        proxy_bypass: *mut u16,
    }
    #[link(name = "winhttp")]
    unsafe extern "system" {
        fn WinHttpOpen(
            agent: *const u16,
            access_type: u32,
            proxy: *const u16,
            bypass: *const u16,
            flags: u32,
        ) -> *mut c_void;
        fn WinHttpSetTimeouts(
            session: *mut c_void,
            resolve: i32,
            connect: i32,
            send: i32,
            receive: i32,
        ) -> i32;
        fn WinHttpGetProxyForUrl(
            session: *mut c_void,
            url: *const u16,
            options: *mut AutoProxyOptions,
            info: *mut ProxyInfo,
        ) -> i32;
        fn WinHttpCloseHandle(handle: *mut c_void) -> i32;
    }

    /// Closes the WinHTTP session on every exit path.
    struct Session(*mut c_void);
    impl Drop for Session {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful `WinHttpOpen` and is closed once.
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }

    let (scheme, _) = scheme_and_host(url)?;
    let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain(Some(0)).collect() };
    let wide_url = wide(url);
    let wide_pac = settings
        .auto_config_url
        .as_deref()
        .filter(|u| !u.is_empty())
        .map(wide);

    let mut flags = 0;
    if settings.auto_detect {
        flags |= AUTOPROXY_AUTO_DETECT;
    }
    if wide_pac.is_some() {
        flags |= AUTOPROXY_CONFIG_URL;
    }
    if flags == 0 {
        return None;
    }
    let mut options = AutoProxyOptions {
        flags,
        auto_detect_flags: if settings.auto_detect {
            AUTO_DETECT_DHCP | AUTO_DETECT_DNS_A
        } else {
            0
        },
        auto_config_url: wide_pac
            .as_ref()
            .map_or(std::ptr::null(), |pac| pac.as_ptr()),
        reserved: std::ptr::null_mut(),
        reserved2: 0,
        auto_logon_if_challenged: 1,
    };
    let mut info = ProxyInfo {
        access_type: 0,
        proxy: std::ptr::null_mut(),
        proxy_bypass: std::ptr::null_mut(),
    };

    // SAFETY: a null agent and proxy strings are allowed with `ACCESS_TYPE_NO_PROXY`; the
    // session is closed by `Session::drop`.
    let session = unsafe {
        WinHttpOpen(
            std::ptr::null(),
            ACCESS_TYPE_NO_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            0,
        )
    };
    if session.is_null() {
        return None;
    }
    let session = Session(session);
    // SAFETY: `session.0` is a live session handle. A failure leaves the default timeouts.
    unsafe { WinHttpSetTimeouts(session.0, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS) };
    // SAFETY: `wide_url` and `wide_pac` are NUL-terminated and outlive the call; `options`
    // and `info` are valid for the layouts the function reads and fills.
    let ok = unsafe {
        WinHttpGetProxyForUrl(session.0, wide_url.as_ptr(), &mut options, &mut info)
    };
    if ok == 0 {
        return None;
    }
    // SAFETY: the call succeeded, so each string is null or OS-allocated and is taken (and
    // freed) exactly once here.
    let (proxy, _bypass) = unsafe { (win::take(info.proxy), win::take(info.proxy_bypass)) };
    if info.access_type == ACCESS_TYPE_NAMED_PROXY {
        Some(route_from_proxy_string(proxy.as_deref(), &scheme))
    } else {
        Some(UrlRoute::Direct)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(proxy: &str, bypass: &str) -> SystemProxySettings {
        SystemProxySettings {
            proxy: Some(proxy.into()).filter(|p: &String| !p.is_empty()),
            bypass: Some(bypass.into()).filter(|b: &String| !b.is_empty()),
            ..SystemProxySettings::default()
        }
    }

    fn single(url: &str, no_proxy: &[&str]) -> SystemProxyChoice {
        SystemProxyChoice::Single {
            url: url.into(),
            no_proxy: no_proxy.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn unsupported(element: &str) -> SystemProxyChoice {
        SystemProxyChoice::Unsupported(element.into())
    }

    #[test]
    fn bypass_matching_is_exact_per_url() {
        let rows: &[(&str, &str, &str, bool)] = &[
            ("<local>", "https", "intranet", true),
            ("<local>", "https", "intranet.corp", false),
            ("<LOCAL>", "http", "[::1]", false),
            ("*.corp.example", "https", "a.corp.example", true),
            ("*.corp.example", "https", "corp.example", false),
            ("10.*", "https", "10.1.2.3", true),
            ("10.*", "https", "110.1.2.3", false),
            ("192.168.*.1", "http", "192.168.7.1", true),
            ("*", "http", "anything.example", true),
            ("Host.Example", "https", "host.example", true),
            ("host.example:8443", "https", "host.example", true),
            (".suffix.example", "https", "a.suffix.example", true),
            (".suffix.example", "https", "suffix.example", true),
            (".suffix.example", "https", "notsuffix.example", false),
            ("https://only.example", "https", "only.example", true),
            ("https://only.example", "http", "only.example", false),
            ("a;b c", "http", "d", false),
            ("a;b c", "http", "b", true),
            ("", "http", "x", false),
        ];
        for (bypass, scheme, host, expected) in rows {
            assert_eq!(bypass_matches(bypass, scheme, host), *expected, "{bypass} {scheme}://{host}");
        }
    }

    #[test]
    fn urls_reduce_to_scheme_and_host() {
        assert_eq!(
            scheme_and_host("HTTPS://Feed.Example.com:8443/a?b=c"),
            Some(("https".into(), "feed.example.com".into()))
        );
        assert_eq!(
            scheme_and_host("http://[::1]:80/"),
            Some(("http".into(), "::1".into()))
        );
        for bad in ["ftp://h/", "/path", "not a url", ""] {
            assert_eq!(scheme_and_host(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_resolver_proxy_string_gives_the_first_candidate_for_the_scheme() {
        let http = |s: &str| UrlRoute::Proxy(s.into());
        assert_eq!(route_from_proxy_string(Some("a:80;b:80"), "https"), http("http://a:80"));
        assert_eq!(
            route_from_proxy_string(Some("http=h:80;https=s:443"), "https"),
            http("http://s:443")
        );
        assert_eq!(route_from_proxy_string(Some("http=h:80"), "https"), UrlRoute::Direct);
        assert_eq!(route_from_proxy_string(None, "http"), UrlRoute::Direct);
        assert_eq!(route_from_proxy_string(Some(""), "http"), UrlRoute::Direct);
    }

    #[test]
    fn the_static_route_for_a_url_applies_bypass_and_scheme() {
        let s = settings("http=h:80;https=s:443", "<local>;*.corp");
        assert_eq!(static_route_for_url(&s, "http", "x.example"), Some(UrlRoute::Proxy("http://h:80".into())));
        assert_eq!(static_route_for_url(&s, "https", "x.example"), Some(UrlRoute::Proxy("http://s:443".into())));
        assert_eq!(static_route_for_url(&s, "https", "box"), Some(UrlRoute::Direct));
        assert_eq!(static_route_for_url(&s, "https", "a.corp"), Some(UrlRoute::Direct));
        // One scheme without an entry connects directly.
        let only_https = settings("https=s:443", "");
        assert_eq!(static_route_for_url(&only_https, "http", "x.example"), Some(UrlRoute::Direct));
        // Nothing usable configured: the caller falls back to the environment.
        assert_eq!(static_route_for_url(&settings("", "<local>"), "http", "x"), None);
        assert_eq!(static_route_for_url(&settings("socks=k:1", ""), "http", "x"), None);
    }

    const DIFFERENT_ROUTES: &str ="HTTP and HTTPS are given different proxy routes";

    #[test]
    fn a_bare_host_and_port_is_one_http_proxy_for_both_schemes() {
        assert_eq!(
            endpoints_by_scheme("proxy.corp:3128"),
            (
                Some("http://proxy.corp:3128".into()),
                Some("http://proxy.corp:3128".into())
            )
        );
        assert_eq!(
            proxy_choice(&settings("proxy.corp:3128", "")),
            single("http://proxy.corp:3128", &[])
        );
    }

    #[test]
    fn each_scheme_gets_its_own_entry_and_never_the_other_one() {
        // Row by row: what an `http` request and an `https` request use.
        let rows: &[(&str, Option<&str>, Option<&str>)] = &[
            (
                "http=h:80;https=s:443",
                Some("http://h:80"),
                Some("http://s:443"),
            ),
            (
                "https=s:443;http=h:80",
                Some("http://h:80"),
                Some("http://s:443"),
            ),
            // Only one scheme has a proxy: the other connects directly.
            ("https=s:443", None, Some("http://s:443")),
            ("http=h:80", Some("http://h:80"), None),
            // A bare entry serves the schemes without an entry of their own.
            (
                "https=s:443;any:8080",
                Some("http://any:8080"),
                Some("http://s:443"),
            ),
            (
                "any:8080 http=h:80",
                Some("http://h:80"),
                Some("http://any:8080"),
            ),
            // Protocols this crate does not speak are ignored.
            ("ftp=f:21;socks=k:1080", None, None),
            ("ftp=f:21;socks=k:1080;http=h:80", Some("http://h:80"), None),
            // The first entry for a scheme wins.
            ("http=first:1;http=second:2", Some("http://first:1"), None),
            // An entry may carry its own scheme.
            (
                "https=https://tls.corp:8443",
                None,
                Some("https://tls.corp:8443"),
            ),
            // An unusable explicit entry does not borrow the bare one.
            (
                "https=socks5://k:1080;any:8080",
                Some("http://any:8080"),
                None,
            ),
        ];
        for (proxy, http, https) in rows {
            assert_eq!(
                endpoints_by_scheme(proxy),
                (http.map(str::to_string), https.map(str::to_string)),
                "{proxy}"
            );
        }
    }

    #[test]
    fn the_same_endpoint_for_both_schemes_is_one_proxy() {
        assert_eq!(
            proxy_choice(&settings("http=p.corp:8080;https=P.CORP:8080", "")),
            single("http://p.corp:8080", &[])
        );
    }

    #[test]
    fn different_endpoints_or_one_direct_scheme_are_reported_not_approximated() {
        // Plain HTTP must not be sent to the endpoint configured for HTTPS, or the reverse,
        // and a scheme configured as direct must not be sent to the other one's proxy.
        for proxy in [
            "http=web.corp:8080;https=secure.corp:8443",
            "https=secure.corp:8443",
            "http=web.corp:8080",
        ] {
            assert_eq!(
                proxy_choice(&settings(proxy, "")),
                unsupported(DIFFERENT_ROUTES),
                "{proxy}"
            );
        }
    }

    #[test]
    fn nothing_usable_is_no_proxy() {
        assert_eq!(
            proxy_choice(&SystemProxySettings::default()),
            SystemProxyChoice::None
        );
        assert_eq!(
            proxy_choice(&settings("  ", "localhost")),
            SystemProxyChoice::None
        );
        assert_eq!(
            proxy_choice(&settings("socks=k:1080", "")),
            SystemProxyChoice::None
        );
        assert_eq!(
            proxy_choice(&settings("socks5://k:1080", "")),
            SystemProxyChoice::None
        );
    }

    #[test]
    fn a_bypass_list_of_plain_hosts_is_kept_whole() {
        assert_eq!(
            bypass_hosts("localhost; *.corp.example ;intranet 10.0.0.1;localhost;.suffix.example"),
            Ok(vec![
                "localhost".to_string(),
                ".corp.example".to_string(),
                "intranet".to_string(),
                "10.0.0.1".to_string(),
                ".suffix.example".to_string(),
            ])
        );
        assert_eq!(bypass_hosts(""), Ok(vec![]));
        assert_eq!(
            proxy_choice(&settings("p:1", "localhost;*.corp.example")),
            single("http://p:1", &["localhost", ".corp.example"])
        );
    }

    #[test]
    fn a_bypass_entry_this_client_cannot_follow_is_reported_never_dropped() {
        // Fifth read, finding 11, and the ruling that followed: while the proxy is in use no
        // bypass entry is dropped (the traffic the user configured as direct would go through
        // the proxy) and none is widened (a scheme-limited entry kept as a host would send
        // the other scheme around the proxy). The first such entry is named.
        let rows = [
            ("https://only.example;both.example", "https://only.example"),
            ("both.example;http://plain.example", "http://plain.example"),
            ("HTTP://plain.example", "HTTP://plain.example"),
            ("localhost;<local>", "<local>"),
            ("<LOCAL>", "<LOCAL>"),
            ("10.*", "10.*"),
            ("*.corp.example;192.168.*.1", "192.168.*.1"),
            ("*", "*"),
            ("*corp.example", "*corp.example"),
            (".", "."),
        ];
        for (bypass, entry) in rows {
            assert_eq!(bypass_hosts(bypass), Err(entry.to_string()), "{bypass}");
            assert_eq!(
                proxy_choice(&settings("p:1", bypass)),
                unsupported(&format!(
                    "the bypass entry `{entry}` cannot be followed exactly"
                )),
                "{bypass}"
            );
        }
        // Without a proxy there is nothing to bypass, whatever the list says.
        assert_eq!(
            proxy_choice(&settings("", "https://only.example;<local>")),
            SystemProxyChoice::None
        );
        // Different routes are reported as that, whatever the bypass list says.
        assert_eq!(
            proxy_choice(&settings("http=h:80;https=s:443", "<local>")),
            unsupported(DIFFERENT_ROUTES)
        );
    }

    #[test]
    fn a_pac_script_is_reported_with_or_without_a_static_proxy() {
        // A PAC script chooses a proxy per URL. Falling back to a direct connection, or to
        // the static proxy, would not be what Windows itself does with these settings.
        for proxy in ["", "p:1"] {
            let pac = SystemProxySettings {
                auto_config: true,
                ..settings(proxy, "")
            };
            assert_eq!(
                proxy_choice(&pac),
                unsupported("a proxy auto-config script is set"),
                "{proxy:?}"
            );
            // A PAC script wins over the auto-detect flag.
            let both = SystemProxySettings {
                auto_detect: true,
                ..pac
            };
            assert_eq!(
                proxy_choice(&both),
                unsupported("a proxy auto-config script is set"),
                "{proxy:?}"
            );
        }
    }

    #[test]
    fn automatic_detection_alone_is_not_an_error_and_never_overrides_a_static_proxy() {
        // The Windows default: detection on, no static proxy, no PAC.
        for proxy in ["", "   "] {
            let detect = SystemProxySettings {
                auto_detect: true,
                ..settings(proxy, "<local>")
            };
            assert_eq!(proxy_choice(&detect), SystemProxyChoice::AutoDetectOnly);
        }
        // With a static proxy the flag is ignored: the proxy is followed exactly...
        let detect = SystemProxySettings {
            auto_detect: true,
            ..settings("p:1", "localhost")
        };
        assert_eq!(proxy_choice(&detect), single("http://p:1", &["localhost"]));
        // ...or refused for the same reasons as without the flag.
        let detect = SystemProxySettings {
            auto_detect: true,
            ..settings("p:1", "<local>")
        };
        assert_eq!(
            proxy_choice(&detect),
            unsupported("the bypass entry `<local>` cannot be followed exactly")
        );
        let detect = SystemProxySettings {
            auto_detect: true,
            ..settings("http=a:1;https=b:2", "")
        };
        assert_eq!(proxy_choice(&detect), unsupported(DIFFERENT_ROUTES));
    }
}
