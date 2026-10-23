//! The Right Suite API host contract (`contracts/right-suite-api.v1.json`), owned by RightKit.
//!
//! Always compiled, no dependencies. The constants are generated (`scripts/gen-suite-api.mjs`)
//! and re-exported here; the functions build every URL and host list an app needs from them, so
//! an app never carries the API host or an artifact host of its own.
//!
//! * The update-check host is the current host only. A legacy host is exported for refusal tests
//!   and gates; it is never in an allowlist.
//! * Artifact hosts are per app (`APP_ARTIFACT_HOSTS`); an app not in the contract has none.
//! * App keys match `^[a-z][a-z0-9-]*$`, so a key can never add a path segment or a query.

#[path = "suite_api_generated.rs"]
mod suite_api_generated;

pub use suite_api_generated::{
    ADMIN_PATCHES_PATH, ADMIN_RELEASES_PATH, ADMIN_RUNTIME_ARTIFACTS_PATH, API_HOST, API_ORIGIN,
    APP_ARTIFACT_HOSTS, ARTIFACT_PATH_PREFIX, GATED_ARTIFACT_HOST, LEGACY_HOSTS,
    PUBLIC_ARTIFACT_HOST, PUBLIC_INSTALLER_BASE, RELEASE_STATUS_PATH, UPDATER_MANIFEST_PATH,
};

use std::fmt;

/// Why a Right Suite API value could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuiteApiError {
    /// The app key is not `^[a-z][a-z0-9-]*$`.
    BadAppKey,
    /// The app key is well formed but the contract does not list the app.
    UnknownApp,
}

impl fmt::Display for SuiteApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BadAppKey => "the app key is not a valid Right Suite app key",
            Self::UnknownApp => "the app is not in the Right Suite API contract",
        })
    }
}

impl std::error::Error for SuiteApiError {}

/// The one host that serves update metadata (a check), as a list for allowlist constructors.
const CHECK_HOSTS: &[&str] = &[API_HOST];

/// Whether `app` is `^[a-z][a-z0-9-]*$`.
fn valid_app_key(app: &str) -> bool {
    let mut bytes = app.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn checked_key(app: &str) -> Result<&str, SuiteApiError> {
    if valid_app_key(app) {
        Ok(app)
    } else {
        Err(SuiteApiError::BadAppKey)
    }
}

/// The updater feed for `app`: the API origin plus the contract's manifest path with `{app}`
/// filled in. Tauri's `{{target}}` placeholder stays literal; the updater fills it at runtime.
pub fn updater_endpoint(app: &str) -> Result<String, SuiteApiError> {
    let app = checked_key(app)?;
    Ok(format!(
        "{API_ORIGIN}{}",
        UPDATER_MANIFEST_PATH.replace("{app}", app)
    ))
}

/// The release-status URL for `app`. The contract's path has no platform placeholder, so none is
/// added: the answer names the platform itself.
pub fn release_status_url(app: &str) -> Result<String, SuiteApiError> {
    let app = checked_key(app)?;
    Ok(format!(
        "{API_ORIGIN}{}",
        RELEASE_STATUS_PATH.replace("{app}", app)
    ))
}

/// The app's path on its artifact hosts, such as `/mailright`.
pub fn artifact_path_prefix(app: &str) -> Result<String, SuiteApiError> {
    let app = checked_key(app)?;
    Ok(ARTIFACT_PATH_PREFIX.replace("{app}", app))
}

/// The hosts that serve update metadata: the current API host only, never a legacy host.
pub fn update_check_hosts() -> &'static [&'static str] {
    CHECK_HOSTS
}

/// The hosts that may serve `app`'s artifacts. An app the contract does not list is an error, so
/// no app inherits another app's hosts.
pub fn artifact_hosts(app: &str) -> Result<Vec<&'static str>, SuiteApiError> {
    let app = checked_key(app)?;
    APP_ARTIFACT_HOSTS
        .iter()
        .find(|(key, _)| *key == app)
        .map(|(_, hosts)| hosts.to_vec())
        .ok_or(SuiteApiError::UnknownApp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const VECTOR: &str = include_str!("../test-vectors/right-suite-api.v1.json");

    fn contract() -> Value {
        serde_json::from_str(VECTOR).expect("the vendored contract is JSON")
    }

    fn text<'a>(value: &'a Value, path: &[&str]) -> &'a str {
        let mut at = value;
        for key in path {
            at = &at[*key];
        }
        at.as_str()
            .unwrap_or_else(|| panic!("{path:?} is a string"))
    }

    #[test]
    fn the_constants_equal_the_vendored_contract() {
        let c = contract();
        assert_eq!(text(&c, &["schema"]), "right-suite-api.v1");
        assert_eq!(API_HOST, text(&c, &["host"]));
        assert_eq!(API_ORIGIN, text(&c, &["origin"]));
        assert_eq!(API_ORIGIN, format!("https://{API_HOST}"));
        let legacy: Vec<&str> = c["legacyHosts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(LEGACY_HOSTS, legacy.as_slice());
        assert_eq!(
            UPDATER_MANIFEST_PATH,
            text(&c, &["paths", "updaterManifest"])
        );
        assert_eq!(RELEASE_STATUS_PATH, text(&c, &["paths", "releaseStatus"]));
        assert_eq!(ADMIN_PATCHES_PATH, text(&c, &["paths", "adminPatches"]));
        assert_eq!(ADMIN_RELEASES_PATH, text(&c, &["paths", "adminReleases"]));
        assert_eq!(
            ADMIN_RUNTIME_ARTIFACTS_PATH,
            text(&c, &["paths", "adminRuntimeArtifacts"])
        );
        assert_eq!(
            GATED_ARTIFACT_HOST,
            text(&c, &["artifactHostSets", "gated"])
        );
        assert_eq!(
            PUBLIC_ARTIFACT_HOST,
            text(&c, &["artifactHostSets", "public"])
        );
        assert_eq!(PUBLIC_INSTALLER_BASE, text(&c, &["publicInstallerBase"]));
        assert_eq!(ARTIFACT_PATH_PREFIX, text(&c, &["artifactPathPrefix"]));

        let apps = c["apps"].as_object().unwrap();
        assert_eq!(APP_ARTIFACT_HOSTS.len(), apps.len());
        for (key, hosts) in APP_ARTIFACT_HOSTS {
            let expected: Vec<&str> = apps[*key]["artifactHosts"]
                .as_array()
                .unwrap_or_else(|| panic!("{key} is in the contract"))
                .iter()
                .map(|set| text(&c, &["artifactHostSets", set.as_str().unwrap()]))
                .collect();
            assert_eq!(*hosts, expected.as_slice(), "{key}");
        }
    }

    #[test]
    fn the_updater_endpoint_is_exact_and_keeps_the_runtime_placeholder() {
        assert_eq!(
            updater_endpoint("mailright").unwrap(),
            "https://api.orthiclabs.com/v1/apps/mailright/releases/latest.json?platform={{target}}"
        );
        assert_eq!(
            release_status_url("viewright").unwrap(),
            "https://api.orthiclabs.com/v1/apps/viewright/releases/status.json"
        );
        assert_eq!(artifact_path_prefix("coderight").unwrap(), "/coderight");
    }

    #[test]
    fn a_malformed_app_key_builds_nothing() {
        for bad in [
            "",
            "Mail",
            "a/b",
            "../x",
            "1app",
            "-app",
            "app_x",
            "a b",
            "a?b",
            "a#b",
            "app\n",
            "é",
            "a.b",
            "mailright/",
        ] {
            assert_eq!(
                updater_endpoint(bad),
                Err(SuiteApiError::BadAppKey),
                "{bad:?}"
            );
            assert_eq!(
                release_status_url(bad),
                Err(SuiteApiError::BadAppKey),
                "{bad:?}"
            );
            assert_eq!(
                artifact_path_prefix(bad),
                Err(SuiteApiError::BadAppKey),
                "{bad:?}"
            );
            assert_eq!(
                artifact_hosts(bad),
                Err(SuiteApiError::BadAppKey),
                "{bad:?}"
            );
        }
        for ok in ["a", "mailright", "app-2", "a-b-c"] {
            assert!(updater_endpoint(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn the_check_host_is_the_current_host_and_never_a_legacy_one() {
        assert_eq!(update_check_hosts(), &[API_HOST]);
        for legacy in LEGACY_HOSTS {
            assert!(!update_check_hosts().contains(legacy));
            for (app, hosts) in APP_ARTIFACT_HOSTS {
                assert!(!hosts.contains(legacy), "{app}");
            }
        }
    }

    #[test]
    fn artifact_hosts_are_per_app_and_an_unlisted_app_has_none() {
        let viewright = artifact_hosts("viewright").unwrap();
        assert_eq!(viewright, vec![PUBLIC_ARTIFACT_HOST]);
        assert!(!viewright.contains(&GATED_ARTIFACT_HOST));
        assert_eq!(
            artifact_hosts("mailright").unwrap(),
            vec![GATED_ARTIFACT_HOST, PUBLIC_ARTIFACT_HOST]
        );
        assert_eq!(artifact_hosts("nosuchapp"), Err(SuiteApiError::UnknownApp));
        for (app, hosts) in APP_ARTIFACT_HOSTS {
            assert!(
                !hosts.contains(&API_HOST),
                "{app}: the API host is not an artifact host"
            );
        }
    }
}
