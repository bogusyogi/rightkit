// The request environment of a carried adapter call (rustright#190; rustright-cli shims.rs adapter_environment, rustright-policy adapter.rs
// check_environment). A `rustright run <adapter> -- <args>` request carries exactly the variables named by RUSTRIGHT_ADAPTER_ENV (a comma
// list), each set in the client's own environment; the broker accepts a name only when the adapter's registration lists it in `env` and it is
// not reserved. The forwarded map is the request core's `env` (rustright-protocol attestation.rs request_core), so its values are bound into the
// attested request digest the gate recomputes (snapshot-request.mjs snapshotRequestCore).
//
// The door (build-launch.mjs) sets RUSTRIGHT_ADAPTER_ENV for a step from the env the step itself declares (`adapterEnvLaunch`); the carrier
// (snapshot-broker.mjs carryBrokerAdapter) reads back what the client will forward (`forwardedAdapterEnv`), with the client's own refusals.
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

/** Whether the broker refuses `key` in a request environment (adapter.rs is_reserved_env_key, case-insensitive). */
export function isReservedAdapterEnvKey(key) {
  const upper = String(key).toUpperCase();
  return upper === "" || upper.includes("=") || RESERVED_EXACT.has(upper) || RESERVED_PREFIXES.some((prefix) => upper.startsWith(prefix));
}

function fail(code, message) {
  const error = new Error(`${code}: ${message}`);
  error.code = code;
  throw error;
}

const validName = (name) => typeof name === "string" && name !== "" && !/[,=\0\s]/.test(name);
const validValue = (value) => typeof value === "string" && !value.includes("\0");

/**
 * What the door adds to a carried adapter step's launch environment for the env the step declares: `{}` for none (no RUSTRIGHT_ADAPTER_ENV at
 * all), else RUSTRIGHT_ADAPTER_ENV naming exactly the step's variables, plus those variables with their values. A reserved name, a name that
 * cannot ride a comma list, a declared name with no value, or a value that is not a string is refused with a coded error.
 */
export function adapterEnvLaunch(stepEnv) {
  const names = Object.keys(stepEnv ?? {}).sort();
  if (names.length === 0) return {};
  for (const name of names) {
    if (isReservedAdapterEnvKey(name)) fail(ADAPTER_ENV_RESERVED, `step env ${JSON.stringify(name)} is broker-controlled or redirects code loading, so no adapter request may carry it (reserved: RUSTRIGHT_*, RIGHTKIT_*, CARGO*, RUST*, loader and interpreter-path variables)`);
    if (!validName(name)) fail(ADAPTER_ENV_INVALID, `step env name ${JSON.stringify(name)} cannot be forwarded to an adapter`);
    const value = stepEnv[name];
    if (value === undefined || value === null) fail(ADAPTER_ENV_UNSET, `step env ${name} is declared but has no value`);
    if (!validValue(String(value))) fail(ADAPTER_ENV_INVALID, `step env ${name} has a value that cannot be forwarded`);
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
