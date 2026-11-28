//! The routes a host option adds or removes, read from `data/option-gated-routes.json`.
//!
//! That file is the one list. The contract (`api_contract`) drops the routes of an option that is
//! off from it, and tools that cannot run this code (a client generator, a contract check)
//! read the same file. The router mounts each set under the same option getter; a unit test keeps
//! the file in step with the endpoint tables and with the getters named here.

use serde::Deserialize;
use std::sync::OnceLock;

/// The file, embedded so the published crate carries it (`data/**` is in the manifest's
/// `include`).
const DATA: &str = include_str!("../data/option-gated-routes.json");

/// An option name as the data file spells it, and the getter that answers for it.
type GatingOption = (&'static str, fn() -> bool);

/// Every option the file may name, with its getter. The file and this table name the same
/// options; a test fails when they differ.
const GATING_OPTIONS: &[GatingOption] = &[
    ("mcp_public_server", crate::host_options::mcp_public_server),
    (
        "plugin_marketplace",
        crate::host_options::plugin_marketplace,
    ),
    ("adapt_routes", crate::host_options::adapt_routes),
    ("peer_teams", crate::host_options::peer_teams),
    (
        "membrane_background_review",
        crate::host_options::membrane_background_review,
    ),
    ("cloud_workspaces", crate::host_options::cloud_workspaces),
    ("guest_shares", crate::host_options::guest_shares),
    (
        "browser_consent_routes",
        crate::host_options::browser_consent_routes,
    ),
    (
        "license_status_route",
        crate::host_options::license_status_route,
    ),
];

#[derive(Debug, Deserialize)]
struct GatedFile {
    options: std::collections::BTreeMap<String, GatedOption>,
}

#[derive(Debug, Deserialize)]
struct GatedOption {
    routes: Vec<GatedRoute>,
}

#[derive(Debug, Deserialize)]
struct GatedRoute {
    method: String,
    path: String,
    contract: bool,
}

/// The parsed file. The file is embedded and a test parses it, so a failure here is a build
/// defect, not a run-time condition.
fn gated() -> &'static GatedFile {
    static PARSED: OnceLock<GatedFile> = OnceLock::new();
    PARSED.get_or_init(|| {
        serde_json::from_str(DATA).expect("data/option-gated-routes.json is valid JSON")
    })
}

/// Whether the option of this name is on, or `None` when no getter has that name.
fn option_enabled(name: &str) -> Option<bool> {
    GATING_OPTIONS
        .iter()
        .find(|(option, _)| *option == name)
        .map(|(_, enabled)| enabled())
}

/// Whether an option that is off removes this endpoint from the contract: the endpoint is listed
/// in the file with `contract` true under an option whose value is `false`.
pub(crate) fn removed_by_options(method: &str, path: &str) -> bool {
    gated().options.iter().any(|(name, option)| {
        option_enabled(name) == Some(false)
            && option.routes.iter().any(|route| {
                route.contract && route.method.eq_ignore_ascii_case(method) && route.path == path
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// The whole file, with the fields only the checks read.
    #[derive(Deserialize)]
    struct FullFile {
        options: BTreeMap<String, FullOption>,
    }

    #[derive(Deserialize)]
    struct FullOption {
        default: bool,
        routes: Vec<FullRoute>,
    }

    #[derive(Deserialize)]
    struct FullRoute {
        method: String,
        path: String,
        #[serde(rename = "operationId")]
        operation_id: String,
        contract: bool,
    }

    fn full() -> FullFile {
        serde_json::from_str(DATA).expect("data/option-gated-routes.json matches its shape")
    }

    #[test]
    fn file_and_getter_table_name_the_same_options() {
        let in_file: BTreeSet<&str> = gated().options.keys().map(String::as_str).collect();
        let in_table: BTreeSet<&str> = GATING_OPTIONS.iter().map(|(name, _)| *name).collect();
        assert_eq!(in_file, in_table);
        for name in in_file {
            assert!(option_enabled(name).is_some(), "{name} has no getter");
        }
    }

    #[test]
    fn defaults_in_the_file_are_the_getters_defaults() {
        // No test sets an option, so every getter reads its default.
        for (name, option) in &full().options {
            assert_eq!(
                option_enabled(name),
                Some(option.default),
                "default of {name}"
            );
        }
    }

    #[test]
    fn contract_entries_match_the_endpoint_tables() {
        let declared = crate::api_contract::declared_endpoints();
        for (name, option) in &full().options {
            for route in &option.routes {
                let found = declared.iter().any(|endpoint| {
                    endpoint.method.eq_ignore_ascii_case(&route.method)
                        && endpoint.path == route.path
                });
                assert_eq!(
                    found,
                    route.contract,
                    "{name}: {} {} is {}declared in the endpoint tables but contract is {}",
                    route.method,
                    route.path,
                    if found { "" } else { "not " },
                    route.contract
                );
                assert_eq!(
                    route.operation_id,
                    crate::api_contract::operation_id(
                        &route.method.to_ascii_lowercase(),
                        &route.path
                    ),
                    "{name}: operation id of {} {}",
                    route.method,
                    route.path
                );
            }
        }
    }

    #[test]
    fn no_route_is_listed_twice() {
        let mut seen = BTreeSet::new();
        for (name, option) in &full().options {
            for route in &option.routes {
                let key = (route.method.to_ascii_uppercase(), route.path.clone());
                assert!(
                    seen.insert(key),
                    "{name}: {} {} twice",
                    route.method,
                    route.path
                );
            }
        }
    }
}
