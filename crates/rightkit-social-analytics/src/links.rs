//! Tracked short links: destination checks, UTM tagging, deterministic link ids, and the pieces of
//! a public redirect route (link-id shape, redirect-target check, referrer host reduction).

use crate::Fail;

/// Longest slug accepted (campaign, variant and UTM values).
pub const SLUG_MAX: usize = 64;
/// `utm_medium` when the policy names none.
pub const DEFAULT_MEDIUM: &str = "social";

fn bad(msg: impl Into<String>) -> Fail {
    ("bad_input", msg.into())
}

/// A lower-case slug: 1 to `SLUG_MAX` characters from `a-z`, `0-9`, `_` and `-`. Uppercase is refused,
/// not folded, so the stored value is always the value the caller sent.
pub fn slug(value: &str, name: &str) -> Result<(), Fail> {
    let ok = !value.is_empty()
        && value.len() <= SLUG_MAX
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(bad(format!(
            "{name} must be a lower-case slug of 1 to {SLUG_MAX} characters from a-z, 0-9, _ and -, got {value}"
        )))
    }
}

/// The https link check for tracked-link destinations (`name` leads each message). Scheme,
/// credentials and the public-host test are `rightkit_http::validate_public_https_url`; on top of
/// it, whitespace/control characters and any port are refused.
pub fn public_https(url: &str, name: &str) -> Result<(), Fail> {
    use rightkit_http::UrlPolicyError;
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(bad(format!(
            "{name} must not contain spaces or control characters"
        )));
    }
    rightkit_http::validate_public_https_url(url).map_err(|why| match why {
        UrlPolicyError::NotHttps => ("url_not_https", format!("{name} must be an https URL")),
        other => bad(format!("{name} is not a plain public https host: {other}")),
    })?;
    let authority = url[8..].split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains(':') {
        return Err(bad(format!(
            "{name} must name a plain host, with no user, port or credentials"
        )));
    }
    Ok(())
}

/// Appends the UTM parameters to the destination. The query goes before any fragment, and the join
/// character follows whatever the destination already ends with. A destination that already names a
/// `utm_` key is `utm_conflict`: the policy never duplicates a key.
pub fn final_url(destination: &str, params: &[(&str, &str)]) -> Result<String, Fail> {
    let (base, fragment) = match destination.find('#') {
        Some(i) => (&destination[..i], &destination[i..]),
        None => (destination, ""),
    };
    if let Some(i) = base.find('?') {
        for part in base[i + 1..].split('&') {
            let key = part.split('=').next().unwrap_or("");
            if key.to_ascii_lowercase().starts_with("utm_") {
                return Err((
                    "utm_conflict",
                    format!(
                        "destination already has {key}; remove it from the destination or the utm policy cannot apply"
                    ),
                ));
            }
        }
    }
    let joiner = if !base.contains('?') {
        "?"
    } else if base.ends_with('?') || base.ends_with('&') {
        ""
    } else {
        "&"
    };
    let query: Vec<String> = params.iter().map(|(k, v)| format!("{k}={v}")).collect();
    Ok(format!("{base}{joiner}{}{fragment}", query.join("&")))
}

/// UTM overrides a link request may carry. Absent fields take their defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UtmPolicy {
    /// `utm_source`; default: the platform.
    #[serde(default)]
    pub source: Option<String>,
    /// `utm_medium`; default: [`DEFAULT_MEDIUM`].
    #[serde(default)]
    pub medium: Option<String>,
    /// `utm_campaign`; default: the campaign id.
    #[serde(default)]
    pub campaign: Option<String>,
    /// `utm_content`; default: the variant id.
    #[serde(default)]
    pub content: Option<String>,
}

/// The four UTM values of one link, after defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utm {
    /// `utm_source`.
    pub source: String,
    /// `utm_medium`.
    pub medium: String,
    /// `utm_campaign`.
    pub campaign: String,
    /// `utm_content`.
    pub content: String,
}

impl Utm {
    /// Apply `policy`: each given value (which must be a slug), else the default for that field.
    pub fn resolve(
        policy: Option<&UtmPolicy>,
        platform: &str,
        campaign: &str,
        variant_id: &str,
    ) -> Result<Utm, Fail> {
        let given = |value: Option<&String>, name: &str| -> Result<Option<String>, Fail> {
            match value {
                Some(v) => slug(v, name).map(|()| Some(v.clone())),
                None => Ok(None),
            }
        };
        let p = policy.cloned().unwrap_or_default();
        Ok(Utm {
            source: given(p.source.as_ref(), "utm_source")?.unwrap_or_else(|| platform.to_string()),
            medium: given(p.medium.as_ref(), "utm_medium")?
                .unwrap_or_else(|| DEFAULT_MEDIUM.to_string()),
            campaign: given(p.campaign.as_ref(), "utm_campaign")?
                .unwrap_or_else(|| campaign.to_string()),
            content: given(p.content.as_ref(), "utm_content")?
                .unwrap_or_else(|| variant_id.to_string()),
        })
    }

    /// The `(key, value)` query pairs, in the order they are appended.
    pub fn params(&self) -> [(&'static str, &str); 4] {
        [
            ("utm_source", self.source.as_str()),
            ("utm_medium", self.medium.as_str()),
            ("utm_campaign", self.campaign.as_str()),
            ("utm_content", self.content.as_str()),
        ]
    }
}

/// `lnk_` plus 32 hex digits (SHA-256, first 128 bits) over the seven inputs joined with `0x1f`.
pub fn link_id_of(
    venture: &str,
    channel: &str,
    account: &str,
    campaign_id: &str,
    variant_id: &str,
    platform: &str,
    destination: &str,
) -> String {
    let parts = [
        venture,
        channel,
        account,
        campaign_id,
        variant_id,
        platform,
        destination,
    ];
    let mut bytes = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            bytes.push(0x1f);
        }
        bytes.extend_from_slice(part.as_bytes());
    }
    format!("lnk_{}", &rightkit_fs::digest::sha256_bytes(&bytes)[..32])
}

/// `lnk_` plus 32 hex digits, the only shape `link.create` makes. Anything else is refused before the store.
pub fn is_link_id(text: &str) -> bool {
    text.strip_prefix("lnk_").is_some_and(|hex| {
        hex.len() == 32
            && hex
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}

/// The host of a `Referer` header value, lower-case, with no scheme, user info, port, path or query; `None` when
/// it is not a plain host name. Only this much of a referrer is ever stored.
pub fn referrer_host(referer: &str) -> Option<String> {
    let rest = referer.trim().split_once("://").map(|(_, r)| r)?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    let host = host.rsplit_once(':').map_or(host, |(h, port)| {
        if port.bytes().all(|c| c.is_ascii_digit()) {
            h
        } else {
            host
        }
    });
    let host = host.to_ascii_lowercase();
    let plain = !host.is_empty()
        && host.len() <= 100
        && host
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-');
    plain.then_some(host)
}

/// Only an `http` or `https` URL with no space or control character may be a redirect target.
pub fn redirect_target_ok(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && lower.len() > 8
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn referrer_hosts_keep_only_a_plain_host() {
        assert_eq!(referrer_host("https://a.b/c?d").as_deref(), Some("a.b"));
        assert_eq!(
            referrer_host("http://u:p@Host.test:80/").as_deref(),
            Some("host.test")
        );
        assert_eq!(referrer_host("nonsense"), None);
        assert_eq!(referrer_host("https://exa mple.test/"), None);
    }

    #[test]
    fn link_ids_are_deterministic_and_well_formed() {
        let a = link_id_of("v", "c", "a", "camp", "var", "x", "https://example.com/");
        assert_eq!(
            a,
            link_id_of("v", "c", "a", "camp", "var", "x", "https://example.com/")
        );
        assert_ne!(
            a,
            link_id_of("v", "c", "a", "camp", "var", "x", "https://example.com/b")
        );
        assert!(is_link_id(&a) && !is_link_id("lnk_0123"));
    }

    #[test]
    fn utm_defaults_and_overrides_are_appended_before_the_fragment() {
        let utm = Utm::resolve(None, "x", "spring", "v1").unwrap();
        assert_eq!(
            final_url("https://example.com/p?a=1#top", &utm.params()).unwrap(),
            "https://example.com/p?a=1&utm_source=x&utm_medium=social&utm_campaign=spring&utm_content=v1#top"
        );
        let bad_policy = UtmPolicy {
            source: Some("Not A Slug".into()),
            ..UtmPolicy::default()
        };
        assert!(Utm::resolve(Some(&bad_policy), "x", "c", "v").is_err());
        assert_eq!(
            final_url("https://example.com/?utm_source=y", &utm.params())
                .unwrap_err()
                .0,
            "utm_conflict"
        );
    }

    #[test]
    fn destinations_must_be_plain_public_https() {
        assert!(public_https("https://example.com/a", "destination").is_ok());
        assert!(public_https("http://example.com/a", "destination").is_err());
        assert!(public_https("https://example.com:8443/", "destination").is_err());
        assert!(public_https("https://exa mple.com/", "destination").is_err());
        assert!(redirect_target_ok("https://example.com/x") && !redirect_target_ok("javascript:x"));
    }
}
