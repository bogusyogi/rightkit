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
 * The environment part of the key a passed test program is remembered under (gate/tests.mjs): every variable the program will really
 * get, name and value, sorted. Nothing is left out. A test may read any of them (TEMP decides where rightkit-sandbox's tests work), and a
 * shorter list was a guess about which ones matter. The cost is known: a rerun of the same commit from another terminal or session differs
 * in some variable and runs the test programs again. That reruns programs, not Cargo: the remembered broker calls have their own key.
 */
// One variable is left out: RUST_TEST_THREADS. The gate sets it itself from the size of the machine and of the list (gate/tests.mjs testWidth), so it
// would change the key whenever one more crate is on the list and every remembered pass would be lost for no reason. It is a share of the
// machine, not an input of the test. A run that needs one thread says so in its arguments, and those are in the key.
export const testKeyEnv = (programEnv) => Object.entries(programEnv ?? {}).filter(([name, value]) => typeof value === "string" && name !== "RUST_TEST_THREADS").sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
