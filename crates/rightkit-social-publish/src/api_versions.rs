//! Pinned platform API versions and their sunset dates. `doctor` reads this table and warns before a
//! pinned version sunsets. Every entry carries its source and the date that source was fetched. Only
//! dated, sourced entries belong here; nothing is guessed.
use std::sync::LazyLock;

use rightkit_platform_rules::{Catalog, Status as RuleStatus};
use serde_json::Value;

use crate::Fail;

/// Sourced platform records promoted on top of the bundled `rightkit-platform-rules` seed.
const CATALOG_OVERRIDES: &str = include_str!("../data/platform-rules.json");

/// The sourced platform-rule catalog: the bundled `rightkit-platform-rules` seed plus the sourced
/// records shipped in this crate (`data/platform-rules.json`). A platform limit is data in that
/// catalog; no limit lives in Rust.
pub fn catalog() -> &'static Catalog {
    static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
        Catalog::bundled()
            .merge_json(CATALOG_OVERRIDES)
            .expect("data/platform-rules.json merges onto the bundled platform rules")
    });
    &CATALOG
}

/// Parse `YYYY-MM-DDTHH:MM:SSZ` (UTC) into seconds since the epoch.
fn parse_instant(raw: &str, what: &str) -> Result<i64, Fail> {
    rightkit_time::DateTime::parse_rfc3339(raw)
        .filter(|t| raw.len() == 20 && raw.ends_with('Z') && t.second <= 59)
        .map(|t| t.unix_seconds())
        .ok_or_else(|| {
            (
                "bad_input",
                format!("{what} must be a UTC instant YYYY-MM-DDTHH:MM:SSZ, got {raw}"),
            )
        })
}

/// Seconds in a day. Sunset arithmetic is in whole UTC days.
const DAY_SECS: i64 = 86_400;

/// A pinned version is `sunset_soon` when its sunset is at most this many days after `now`.
pub const WARN_DAYS: i64 = 60;

/// One pinned platform API version, read from the sourced `api_version` rules of the platform-rule catalog.
pub struct PinnedApiVersion {
    pub platform: &'static str,
    pub version: &'static str,
    /// The sunset instant, UTC, `YYYY-MM-DDTHH:MM:SSZ`: the start of the sunset day.
    pub sunset: &'static str,
    /// The page the sunset date was read from.
    pub source: &'static str,
    /// The day the source was fetched, `YYYY-MM-DD`.
    pub fetched: &'static str,
}

/// The pinned versions, one per platform with a sourced, dated `api_version` rule in the catalog
/// (`data/platform-rules.json`: LinkedIn Marketing API 202609, Meta Graph API v25.0, the newest version whose
/// expiration date the versions page states). The values live in that data, not here; a platform with no
/// sourced and dated rule has no entry. Strings are leaked once: the table lives for the process.
pub static PINNED: LazyLock<Vec<PinnedApiVersion>> = LazyLock::new(|| {
    let catalog = catalog();
    let leak = |text: String| -> &'static str { Box::leak(text.into_boxed_str()) };
    let mut pins: Vec<PinnedApiVersion> = Vec::new();
    for rule in catalog.rules() {
        if rule.capability != "api_version" || pins.iter().any(|p| p.platform == rule.platform) {
            continue;
        }
        let Some(latest) = catalog.get(&rule.platform, "all", "api_version", "any") else {
            continue;
        };
        let (Some(sunset), Some(source), Some(Value::String(version))) = (
            latest.sunset_date.as_deref(),
            latest.source_url.as_deref(),
            latest.value.as_ref(),
        ) else {
            continue;
        };
        if latest.status != RuleStatus::Sourced {
            continue;
        }
        pins.push(PinnedApiVersion {
            platform: leak(latest.platform.clone()),
            version: leak(version.clone()),
            sunset: leak(format!("{sunset}T00:00:00Z")),
            source: leak(source.to_owned()),
            fetched: leak(latest.fetched().to_owned()),
        });
    }
    pins
});

/// The pinned entry for a platform, if the registry has one. This is the one place a request builder
/// reads the version it sends (the LinkedIn publish client reads it for its `LinkedIn-Version` header),
/// so `doctor` and the client can never disagree about which version is pinned.
pub fn pinned(platform: &str) -> Option<&'static PinnedApiVersion> {
    PINNED.iter().find(|p| p.platform == platform)
}

/// The doctor status of one pinned version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The sunset is more than `WARN_DAYS` days after `now`.
    Current,
    /// The sunset is still ahead and within `WARN_DAYS` days of `now`.
    SunsetSoon,
    /// The sunset instant is at or before `now`.
    SunsetPassed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Current => "current",
            Status::SunsetSoon => "sunset_soon",
            Status::SunsetPassed => "sunset_passed",
        }
    }
}

/// Status of `version` at `now` (UTC seconds), with the whole days left until the sunset (truncated
/// toward zero). `days_left` is meaningful for `current` and `sunset_soon` only.
pub fn status_at(version: &PinnedApiVersion, now: i64) -> Result<(Status, i64), Fail> {
    let sunset = parse_instant(version.sunset, "sunset")?;
    let left = sunset - now;
    let status = if left <= 0 {
        Status::SunsetPassed
    } else if left <= WARN_DAYS * DAY_SECS {
        Status::SunsetSoon
    } else {
        Status::Current
    };
    Ok((status, left / DAY_SECS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_follows_the_sunset_date() {
        let v = PinnedApiVersion {
            platform: "example",
            version: "1",
            sunset: "2027-01-01T00:00:00Z",
            source: "https://example.com/versions",
            fetched: "2026-10-01",
        };
        let sunset = parse_instant(v.sunset, "sunset").unwrap();
        assert_eq!(status_at(&v, sunset).unwrap().0, Status::SunsetPassed);
        assert_eq!(
            status_at(&v, sunset - DAY_SECS).unwrap().0,
            Status::SunsetSoon
        );
        assert_eq!(
            status_at(&v, sunset - (WARN_DAYS + 5) * DAY_SECS)
                .unwrap()
                .0,
            Status::Current
        );
        assert!(parse_instant("2027-01-01", "x").is_err());
    }

    #[test]
    fn every_pin_names_a_source_and_a_parseable_sunset() {
        for pin in PINNED.iter() {
            assert!(!pin.source.is_empty(), "{}", pin.platform);
            assert!(
                parse_instant(pin.sunset, "sunset").is_ok(),
                "{}",
                pin.sunset
            );
        }
    }
}
