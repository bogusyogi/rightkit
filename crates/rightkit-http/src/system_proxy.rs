//! The proxy the operating system is configured with, for [`ProxyMode::System`].
//!
//! Windows keeps a per-user proxy configuration (Settings > Network > Proxy, the old
//! "Internet Options"): a static proxy, a bypass list, a proxy auto-config (PAC) script URL
//! and an auto-detect flag. This module reads the static proxy and its bypass list through
//! `WinHttpGetIEProxyConfigForCurrentUser` and works out, per request scheme, which endpoint
//! Windows would use.
//!
//! The result is one of these ([`SystemProxyChoice`]): no proxy; one proxy that serves both
//! `http` and `https` with a bypass list of plain hosts (a client-wide proxy); a static
//! configuration that needs a decision per request ([`SystemProxyChoice::PerRequest`]:
//! endpoints that differ by scheme, a bypass entry limited to one scheme, `<local>` or a
//! wildcard a by-host list cannot express), which the clients answer per request URL with
//! [`static_route_for_url`]; or a PAC script, which this mode refuses
//! ([`SystemProxyChoice::Unsupported`]; `ProxyMode::SystemForUrl` evaluates one). Automatic
//! detection with no static proxy and no PAC script is [`SystemProxyChoice::AutoDetectOnly`],
//! not an error; with a static proxy the flag is ignored.
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
            // Drop a trailing `:port` (but not the colons of an IPv6 literal): `[v6]:port`
            // keeps what is inside the brackets, `host:port` keeps the host.
            let rest = if let Some(inner) = rest.strip_prefix('[') {
                inner.split_once(']').map_or(inner, |(addr, _)| addr)
            } else {
                match rest.rsplit_once(':') {
                    Some((head, port))
                        if !port.is_empty()
                            && port.chars().all(|c| c.is_ascii_digit())
                            && !head.contains(':') =>
                    {
                        head
                    }
                    _ => rest,
                }
            };
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
#[cfg_attr(not(windows), allow(dead_code))] // the WinHTTP caller is Windows-only; tests run everywhere
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
    /// A static proxy that cannot be one client-wide proxy with a by-host bypass list:
    /// endpoints that differ by scheme (or one scheme direct), or a bypass entry that is
    /// scheme-limited, `<local>` or has an inner wildcard. Each request URL is routed on its
    /// own with [`static_route_for_url`], so nothing is approximated.
    PerRequest,
    /// The configuration holds something this client cannot follow exactly (a PAC script,
    /// which chooses a proxy per URL). Carries that element, in words a person can match
    /// against the Windows proxy settings.
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
    // A PAC script chooses a proxy per URL and needs a script engine: refused, whatever else
    // is set. (`ProxyMode::SystemForUrl` hands it to WinHTTP.)
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
                Err(_) => SystemProxyChoice::PerRequest,
            }
        }
        _ => SystemProxyChoice::PerRequest,
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
    let auto_config_url = pac
        .map(|url| url.trim().to_string())
        .filter(|u| !u.is_empty());
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
pub(crate) fn winhttp_route_for_url(
    _url: &str,
    _settings: &SystemProxySettings,
) -> Option<UrlRoute> {
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
    let ok =
        unsafe { WinHttpGetProxyForUrl(session.0, wide_url.as_ptr(), &mut options, &mut info) };
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
