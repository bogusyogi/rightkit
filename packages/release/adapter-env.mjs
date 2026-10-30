// The request environment of a carried adapter call (rustright#190; rustright-cli shims.rs adapter_environment, rustright-policy adapter.rs
// check_environment). A `rustright run <adapter> -- <args>` request carries exactly the variables named by RUSTRIGHT_ADAPTER_ENV (a comma
// list), each set in the client's own environment; the broker accepts a name only when the adapter's registration lists it in `env` and it is
// not reserved. The forwarded map is the request core's `env` (rustright-protocol attestation.rs request_core), so its values are bound into the
// attested request digest the gate recomputes (snapshot-request.mjs snapshotRequestCore).
//
// The door (build-launch.mjs) sets RUSTRIGHT_ADAPTER_ENV for a step from the env the step itself declares (`adapterEnvLaunch`); the carrier
// (snapshot-broker.mjs carryBrokerAdapter) reads back what the client will forward (`forwardedAdapterEnv`), with the client's own refusals.
import path from "node:path";

export const ADAPTER_ENV_VAR = "RUSTRIGHT_ADAPTER_ENV";
const LEGACY_ADAPTER_ENV_VAR = "RIGHTKIT_ADAPTER_ENV";

export const ADAPTER_ENV_RESERVED = "RK_ADAPTER_ENV_RESERVED";
export const ADAPTER_ENV_UNSET = "RK_ADAPTER_ENV_UNSET";
export const ADAPTER_ENV_INVALID = "RK_ADAPTER_ENV_INVALID";

// rustright-policy adapter.rs RESERVED_ENV_KEYS and RESERVED_ENV_PREFIXES at rustright 7b24288, compared upper-case. The prefix list still holds
// RIGHTKIT_ (marked legacy, "removed by task L-1"): until RustRight drops it, a RIGHTKIT_* name is refused by the broker and cannot be registered.
const RESERVED_EXACT = new Set([
  "PATH", "PATHEXT", "HOME", "USERPROFILE", "TMPDIR", "TMP", "TEMP", "SYSTEMROOT", "COMSPEC", "SHELL", "BASH_ENV", "ENV", "PYTHONPATH", "PYTHONHOME",
  "PYTHONSTARTUP", "PYTHONUSERBASE", "PYTHONEXECUTABLE", "PYTHONINSPECT", "PYTHONSAFEPATH", "NODE_OPTIONS", "NODE_PATH", "RUBYOPT", "RUBYLIB", "GEM_HOME",
  "GEM_PATH", "PERL5LIB", "PERL5OPT", "PERLLIB", "JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS", "JDK_JAVA_OPTIONS", "CLASSPATH", "GOFLAGS", "GOROOT", "DOTNET_ROOT",
  "DOTNET_STARTUP_HOOKS", "DOTNET_ADDITIONAL_DEPS", "ERL_FLAGS", "ERL_AFLAGS", "ERL_ZFLAGS", "ELIXIR_ERL_OPTIONS",
]);
const RESERVED_PREFIXES = ["RUSTRIGHT_", "RIGHTKIT_", "LD_", "DYLD_", "CARGO", "RUST", "SCCACHE", "COR_", "CORECLR_", "DOTNET_HOST"];

/**
 * The signed-artifact handoff (GB-05; adapter.rs QA_HANDOFF_ENV): the sign step's two outputs reach a consumer under exactly these two names. They are
 * not secrets and not broker-owned, so the broker exempts exactly these two from the reserved RIGHTKIT_ prefix; they must be registered in the adapter's
 * `env` and carry an absolute path with no `..`.
 */
export const QA_HANDOFF_ENV = Object.freeze(["RIGHTKIT_QA_APP_BINARY", "RIGHTKIT_QA_SIGNING_EVIDENCE"]);
export const isQaHandoffKey = (key) => QA_HANDOFF_ENV.includes(String(key).toUpperCase());
export const ADAPTER_ENV_UNREGISTERED = "RK_ADAPTER_ENV_UNREGISTERED";
export const ADAPTER_ENV_HANDOFF_PATH = "RK_ADAPTER_ENV_HANDOFF_PATH";

/** Whether the broker refuses `key` in a request environment (adapter.rs is_reserved_env_key, case-insensitive; the QA handoff names are exempt). */
export function isReservedAdapterEnvKey(key) {
  const upper = String(key).toUpperCase();
  if (isQaHandoffKey(upper)) return false;
  return upper === "" || upper.includes("=") || RESERVED_EXACT.has(upper) || RESERVED_PREFIXES.some((prefix) => upper.startsWith(prefix));
}

/**
 * The candidate registration manifest of the request-environment keys, per adapter (GB-04): the inventory of every non-reserved key the gate's planners
 * send, by step. Each adapter's registration must list exactly these in its `env`; the installed registration must equal this manifest, and a key outside
 * it refuses at planning, before any process. `pnpm` is the reference registration RustRight published (docs/contracts/adapter-argv.fixtures.json
 * `adapter.env`); `node` takes the same keys (a step script reads the test knob and the signed handoff); `swift` takes none (the gate sends no environment
 * to a SwiftPM job).
 *   RK_ISOLATED_CHECKOUT   test knob of a broker-admitted test row (right-suite-contract.test.mjs, release-exports.test.mjs): "1" runs it from an isolated checkout
 *   RIGHTKIT_QA_APP_BINARY / RIGHTKIT_QA_SIGNING_EVIDENCE   the signed-artifact handoff (GB-05), set by the sign step only
 */
export const ADAPTER_ENV_MANIFEST = Object.freeze({
  "rightkit-pnpm": Object.freeze(["RK_ISOLATED_CHECKOUT", ...QA_HANDOFF_ENV]),
  "rightkit-node": Object.freeze(["RK_ISOLATED_CHECKOUT", ...QA_HANDOFF_ENV]),
  "rightkit-swift": Object.freeze([]),
});

/**
 * Why the broker would refuse this request environment for an adapter whose registration lists `registered` (adapter.rs `check_environment`), or null: a
 * reserved key (broker-controlled, loader or interpreter path), a signed-artifact handoff key whose value is not an absolute path with no `..`, and any key the
 * registration does not list. One refusal names every unregistered key and the registered set; nothing is dropped silently.
 */
export function adapterEnvRefusal(env, registered) {
  const keys = Object.keys(env ?? {});
  const reserved = keys.find((key) => isReservedAdapterEnvKey(key));
  if (reserved !== undefined) return { code: ADAPTER_ENV_RESERVED, message: `environment key ${JSON.stringify(reserved)} is broker-controlled or redirects code loading, so no adapter request may carry it (reserved: RUSTRIGHT_*, RIGHTKIT_* except the two signed-artifact handoff names, CARGO*, RUST*, PATH, loader and interpreter-path variables)` };
  const listed = new Set((registered ?? []).map(String));
  for (const key of keys) {
    const value = String(env[key]);
    if (isQaHandoffKey(key) && listed.has(key) && !(isAbsolutePath(value) && !value.split(/[\\/]/).includes(".."))) return { code: ADAPTER_ENV_HANDOFF_PATH, message: `environment key ${JSON.stringify(key)} must be an absolute path with no '..' (the signed artifact's exact path)` };
  }
  const unregistered = keys.filter((key) => !listed.has(key));
  if (unregistered.length === 0) return null;
  return { code: ADAPTER_ENV_UNREGISTERED, message: `environment key(s) ${unregistered.map((k) => JSON.stringify(k)).join(", ")} are not registered for this adapter (registered: ${[...listed].sort().join(", ") || "none"}); register the key in the adapter's \`env\` (the candidate manifest is ADAPTER_ENV_MANIFEST) or remove it from the step; nothing is dropped to make the call pass` };
}

function fail(code, message) {
  const error = new Error(`${code}: ${message}`);
  error.code = code;
  throw error;
}

const isAbsolutePath = (value) => path.isAbsolute(value) || path.posix.isAbsolute(value) || path.win32.isAbsolute(value);
const validName = (name) => typeof name === "string" && name !== "" && !/[,=\0\s]/.test(name);
const validValue = (value) => typeof value === "string" && !value.includes("\0");

/**
 * What the door adds to a carried adapter step's launch environment for the env the step declares: `{}` for none (no RUSTRIGHT_ADAPTER_ENV at
 * all), else RUSTRIGHT_ADAPTER_ENV naming exactly the step's variables, plus those variables with their values. A reserved name, a name that
 * cannot ride a comma list, a declared name with no value, or a value that is not a string is refused with a coded error.
 */
export function adapterEnvLaunch(stepEnv, { registered = null } = {}) {
  const names = Object.keys(stepEnv ?? {}).sort();
  if (names.length === 0) return {};
  for (const name of names) {
    if (isReservedAdapterEnvKey(name)) fail(ADAPTER_ENV_RESERVED, `step env ${JSON.stringify(name)} is broker-controlled or redirects code loading, so no adapter request may carry it (reserved: RUSTRIGHT_*, RIGHTKIT_* except the two signed-artifact handoff names, CARGO*, RUST*, loader and interpreter-path variables)`);
    if (!validName(name)) fail(ADAPTER_ENV_INVALID, `step env name ${JSON.stringify(name)} cannot be forwarded to an adapter`);
    const value = stepEnv[name];
    if (value === undefined || value === null) fail(ADAPTER_ENV_UNSET, `step env ${name} is declared but has no value`);
    if (!validValue(String(value))) fail(ADAPTER_ENV_INVALID, `step env ${name} has a value that cannot be forwarded`);
  }
  // GB-04: judged against the adapter's registration (its `env`) before anything is launched; one refusal names every unregistered key.
  if (registered !== null) {
    const refusal = adapterEnvRefusal(Object.fromEntries(names.map((name) => [name, String(stepEnv[name])])), registered);
    if (refusal) fail(refusal.code, refusal.message);
  }
  return { [ADAPTER_ENV_VAR]: names.join(","), ...Object.fromEntries(names.map((name) => [name, String(stepEnv[name])])) };
}

/**
 * The map the RustRight client forwards for a call run with `env` (shims.rs adapter_environment): each name in RUSTRIGHT_ADAPTER_ENV, its value
 * from `env`. A reserved name, or a named variable that is not set, is refused (the client refuses the same two ways).
 */
export function forwardedAdapterEnv(env) {
  const raw = env?.[ADAPTER_ENV_VAR] ?? env?.[LEGACY_ADAPTER_ENV_VAR] ?? "";
  const forwarded = {};
  for (const name of String(raw).split(",").map((part) => part.trim()).filter(Boolean)) {
    if (isReservedAdapterEnvKey(name)) fail(ADAPTER_ENV_RESERVED, `${ADAPTER_ENV_VAR} names ${name}, which is broker-controlled and cannot be forwarded to an adapter`);
    const value = env[name];
    if (value === undefined) fail(ADAPTER_ENV_UNSET, `${ADAPTER_ENV_VAR} names ${name}, which is not set`);
    forwarded[name] = String(value);
  }
  return forwarded;
}
