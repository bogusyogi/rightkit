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
//! limited to one scheme, `<local>`, a wildcard this client cannot express, a PAC script and
//! automatic detection (both of those choose a proxy per URL).
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
    /// "Automatically detect settings" is on.
    pub(crate) auto_detect: bool,
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
    if settings.auto_detect {
        return SystemProxyChoice::Unsupported(
            "\"Automatically detect settings\" (proxy auto-detect) is on".into(),
        );
    }
    if settings.auto_config {
        return SystemProxyChoice::Unsupported("a proxy auto-config script is set".into());
    }
    let raw = settings.proxy.as_deref().unwrap_or_default().trim();
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
    use std::ffi::c_void;

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
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalFree(memory: *mut c_void) -> *mut c_void;
    }

    /// Copy a NUL-terminated UTF-16 string the OS allocated, then free it.
    ///
    /// # Safety
    /// `text` is null or a NUL-terminated string allocated with `GlobalAlloc` that nothing
    /// else frees.
    unsafe fn take(text: *mut u16) -> Option<String> {
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
    SystemProxySettings {
        proxy,
        bypass,
        auto_config: pac.is_some_and(|url| !url.trim().is_empty()),
        auto_detect: config.auto_detect != 0,
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

    const DIFFERENT_ROUTES: &str = "HTTP and HTTPS are given different proxy routes";

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
    fn a_pac_script_or_automatic_detection_is_reported_with_or_without_a_static_proxy() {
        // Both choose a proxy per URL. Falling back to a direct connection, or to the static
        // proxy, would not be what Windows itself does with these settings.
        for proxy in ["", "p:1"] {
            let detect = SystemProxySettings {
                auto_detect: true,
                ..settings(proxy, "")
            };
            assert_eq!(
                proxy_choice(&detect),
                unsupported("\"Automatically detect settings\" (proxy auto-detect) is on"),
                "{proxy:?}"
            );
            let pac = SystemProxySettings {
                auto_config: true,
                ..settings(proxy, "")
            };
            assert_eq!(
                proxy_choice(&pac),
                unsupported("a proxy auto-config script is set"),
                "{proxy:?}"
            );
        }
    }
}
