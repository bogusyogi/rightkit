//! The gate for tests that use a real per-user store (AppContainer profiles).
//!
//! Such a store cannot be pointed at a temporary copy, so these tests run only where that is
//! intended: on CI (`GITHUB_ACTIONS=true`) or with `RIGHTKIT_TEST_REAL_USER_STORES=1`. Anywhere
//! else, for example `cargo test` on someone's own machine, they print why and pass without
//! touching anything.
#![allow(dead_code)]

/// The opt-in variable, one name for the whole repository.
pub const OPT_IN: &str = "RIGHTKIT_TEST_REAL_USER_STORES";

/// Whether `test` may use the real user's stores in this run. Prints the reason when it may not.
pub fn real_user_stores_allowed(test: &str) -> bool {
    let is = |name: &str, value: &str| std::env::var(name).is_ok_and(|v| v == value);
    if is("GITHUB_ACTIONS", "true") || is(OPT_IN, "1") {
        return true;
    }
    println!(
        "skipped {test}: it writes to the real user's AppContainer profiles; it runs only on CI \
         (GITHUB_ACTIONS=true) or with {OPT_IN}=1"
    );
    false
}

/// A test names what it writes after itself: refuse any name that does not start with `prefix`.
pub fn assert_test_name(name: &str, prefix: &str) {
    assert!(
        name.starts_with(prefix),
        "a test may only write names starting with `{prefix}`, not `{name}`"
    );
}

/// Deletes the AppContainer profile a spawn with one policy registered, however the test ends.
/// The name is taken while the granted folders still exist.
pub struct ProfileCleanup(String);

impl ProfileCleanup {
    pub fn of(policy: &rightkit_sandbox::SandboxPolicy) -> Self {
        let name = rightkit_sandbox::appcontainer_profile_name_for_test(policy)
            .expect("the profile name of the test's policy");
        assert_test_name(&name, "rightkit.sandbox.");
        Self(name)
    }
}

impl Drop for ProfileCleanup {
    fn drop(&mut self) {
        let _ = rightkit_sandbox::delete_appcontainer_profile_for_test(&self.0);
    }
}
