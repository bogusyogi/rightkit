// The environment a unit's test programs run in. One definition for both compile paths (loop review 11, D1): the per-crate rows
// (local-publish.mjs) and the build-together path (gate/tests.mjs). Before this module the second ran test programs with the gate's
// whole environment, credentials included.

/** Names that carry a credential: a test program never inherits them. */
export const CREDENTIAL_NAME = /(TOKEN|SECRET|PASSWORD|API_KEY|PRIVATE_KEY)/i;

/**
 * `env` without credential-named variables, plus the three settings every unit test runs under: assets are required unless the unit is
 * listed in ci/asset-gated.json (`assetGated`), the checkout is isolated, Cargo prints no colour.
 */
export function testProgramEnv(env = process.env, assetGated = false) {
  const inherited = Object.fromEntries(Object.entries(env ?? {}).filter(([key]) => !CREDENTIAL_NAME.test(key)));
  return { ...inherited, RIGHTKIT_REQUIRE_ASSETS: assetGated ? "0" : "1", RIGHTKIT_ISOLATED_CHECKOUT: "1", CARGO_TERM_COLOR: "never" };
}

/**
 * The variables of a test program's environment that go into the key a passed program is remembered under (gate/tests.mjs).
 * A fixed list, on purpose: the whole environment in a key was the cache-miss loop (RUST_LOG, TERM and CARGO differ by launcher).
 * Anything not named here can change without voiding a remembered pass; within one commit only, because the key also carries the commit.
 */
export const TEST_KEY_ENV = ["CI", "LANG", "LC_ALL", "RIGHTKIT_ISOLATED_CHECKOUT", "RIGHTKIT_REQUIRE_ASSETS", "RUST_TEST_THREADS", "TZ"];
/** [[name, value-or-null]] for TEST_KEY_ENV, read from the environment the program will really get. */
export const testKeyEnv = (programEnv) => TEST_KEY_ENV.map((name) => [name, typeof programEnv?.[name] === "string" ? programEnv[name] : null]);
