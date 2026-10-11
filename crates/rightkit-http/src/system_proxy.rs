//! The proxy the operating system is configured with, for [`ProxyMode::System`].
//!
//! Windows keeps a per-user proxy configuration (Settings > Network > Proxy, the old
//! "Internet Options"): a static proxy, a bypass list, a proxy auto-config (PAC) script URL
//! and an auto-detect flag. This module reads the static proxy and its bypass list through
//! `WinHttpGetIEProxyConfigForCurrentUser` and turns them into the same form an environment
//! proxy has: one proxy URL and a `NO_PROXY`-style list.
//!
//! Not evaluated: a PAC script and auto-detection. Both choose a proxy per URL, and a client
//! here makes one proxy decision when it is built. A configuration that has only those reads
//! as "no static proxy", and the caller falls back to the environment.
//!
//! [`ProxyMode::System`]: crate::ProxyMode::System

/// The static part of the operating system's proxy configuration, as the OS stores it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SystemProxySettings {
    /// `host:port`, or a per-protocol list such as `http=a:80;https=b:443`.
    pub(crate) proxy: Option<String>,
    /// Hosts that connect directly, separated by `;` or whitespace. `<local>` and `*`
    /// wildcards are allowed by the OS.
    pub(crate) bypass: Option<String>,
}

/// The proxy URL and `NO_PROXY`-style entries that `settings` describe, or `None` when they
/// name no usable `http`/`https` proxy.
///
/// A per-protocol list prefers the `https` entry, then `http`; a bare `host:port` serves
/// every protocol. An entry without a scheme is an `http` proxy, as Windows treats it.
/// `socks` and `ftp` entries are ignored: this crate speaks to `http` and `https` proxies.
///
/// Bypass entries become `NO_PROXY` entries where they can be expressed: a host name as it
/// is, and `*.example.com` as `.example.com`. `<local>` (names without a dot) and patterns
/// with a wildcard elsewhere (`10.*`) are dropped, so those hosts go through the proxy
/// instead of connecting directly.
pub(crate) fn proxy_from_settings(settings: &SystemProxySettings) -> Option<(String, Vec<String>)> {
    let raw = settings.proxy.as_deref()?.trim();
    if raw.is_empty() {
        return None;
    }
    let mut any = None;
    let mut http = None;
    let mut https = None;
    for entry in raw
        .split([';', ' '])
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
    let chosen = https.or(http).or(any).filter(|v| !v.is_empty())?;
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
    if scheme != "http" && scheme != "https" {
        return None;
    }

    let mut no_proxy = Vec::new();
    for entry in settings
        .bypass
        .as_deref()
        .unwrap_or_default()
        .split(|c: char| c == ';' || c.is_whitespace())
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        if entry.eq_ignore_ascii_case("<local>") {
            continue;
        }
        // A scheme prefix (`https://host`) narrows the entry to one protocol; keep the host.
        let host = entry.split_once("://").map_or(entry, |(_, rest)| rest);
        let host = host.strip_prefix('*').unwrap_or(host);
        if host.is_empty() || host.contains('*') || host == "." {
            continue;
        }
        if !no_proxy.iter().any(|known: &String| known == host) {
            no_proxy.push(host.to_string());
        }
    }
    Some((url, no_proxy))
}

/// The current user's static proxy configuration, or the default (nothing configured) on a
/// system without one.
#[cfg(not(windows))]
pub(crate) fn system_proxy_settings() -> SystemProxySettings {
    SystemProxySettings::default()
}

/// The current user's static proxy configuration from WinHTTP's view of the Internet
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
    let (_pac, proxy, bypass) = unsafe {
        (
            take(config.auto_config_url),
            take(config.proxy),
            take(config.proxy_bypass),
        )
    };
    SystemProxySettings { proxy, bypass }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(proxy: &str, bypass: &str) -> SystemProxySettings {
        SystemProxySettings {
            proxy: Some(proxy.into()).filter(|p: &String| !p.is_empty()),
            bypass: Some(bypass.into()).filter(|b: &String| !b.is_empty()),
        }
    }

    #[test]
    fn a_bare_host_and_port_is_an_http_proxy_for_everything() {
        assert_eq!(
            proxy_from_settings(&settings("proxy.corp:3128", "")),
            Some(("http://proxy.corp:3128".into(), vec![]))
        );
    }

    #[test]
    fn a_per_protocol_list_prefers_https_then_http_and_ignores_the_rest() {
        assert_eq!(
            proxy_from_settings(&settings("ftp=f:21;http=h:80;https=s:443;socks=k:1080", "")),
            Some(("http://s:443".into(), vec![]))
        );
        assert_eq!(
            proxy_from_settings(&settings("socks=k:1080;http=h:80", "")),
            Some(("http://h:80".into(), vec![]))
        );
        assert_eq!(proxy_from_settings(&settings("socks=k:1080", "")), None);
    }

    #[test]
    fn an_entry_with_a_scheme_keeps_it_and_other_schemes_are_refused() {
        assert_eq!(
            proxy_from_settings(&settings("https://secure.corp:8443", "")),
            Some(("https://secure.corp:8443".into(), vec![]))
        );
        assert_eq!(proxy_from_settings(&settings("socks5://k:1080", "")), None);
    }

    #[test]
    fn nothing_configured_is_no_proxy() {
        assert_eq!(proxy_from_settings(&SystemProxySettings::default()), None);
        assert_eq!(proxy_from_settings(&settings("  ", "localhost")), None);
    }

    #[test]
    fn the_bypass_list_becomes_no_proxy_entries_where_it_can() {
        let (_, no_proxy) = proxy_from_settings(&settings(
            "p:1",
            "localhost; *.corp.example ;intranet <local>;10.*;https://only.example;localhost",
        ))
        .unwrap();
        assert_eq!(
            no_proxy,
            vec!["localhost", ".corp.example", "intranet", "only.example"]
        );
    }
}
