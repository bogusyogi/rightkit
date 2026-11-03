//! Test seam for `TrustRoots::Bundled`.
//!
//! A TLS test needs a server whose certificate chains to a root the client trusts, and the
//! bundled Mozilla set cannot contain a throwaway CA. With the `testing` feature,
//! [`crate::testing::add_bundled_test_root`] registers extra DER roots that every client
//! built afterwards with `TrustRoots::Bundled` trusts, in addition to (async) or instead of
//! (blocking: ureq cannot extend its bundled set) the Mozilla roots. `TrustRoots::Platform`
//! never consults it. Without the feature this module has no state and returns nothing.

/// DER roots registered through the testing seam; always empty without `testing`.
pub(crate) fn test_roots() -> Vec<Vec<u8>> {
    {
        Vec::new()
    }
}
