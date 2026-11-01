include!("config.rs");
include!("openapi.rs");
include!("discovery_middleware.rs");
include!("install_local.rs");
include!("marketplace.rs");
include!("package.rs");
include!("signature_provenance.rs");
include!("routes_handlers.rs");

#[cfg(test)]
include!("tests.rs");

#[cfg(test)]
include!("live_catalog_tests.rs");

#[cfg(all(test, feature = "qualification"))]
include!("eff001_subagent_route_tests.rs");
