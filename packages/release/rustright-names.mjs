// GB-09: the names the gate emits at the RustRight interface. The client reads the current `RUSTRIGHT_*` spellings and, for migration only, the legacy
// `RIGHTKIT_*` ones (rustright-cli shims.rs LEGACY_ENV_SPELLINGS, removed by RustRight's task L-1; `RUSTRIGHT_LEGACY_ALIASES=off` makes it read none).
// Every outbound production call carries the current spelling only: a legacy interface name found in a call's environment is translated to its current
// spelling (the current one wins when both are set) and the legacy name is dropped, so no route depends on an alias the broker may stop reading. Gate-internal
// RIGHTKIT_* names that are not part of this interface (RIGHTKIT_MANAGED_ROOT, RIGHTKIT_QA_*, ...) are never touched.
export const LEGACY_INTERFACE_ENV = Object.freeze({
  RIGHTKIT_BUILD_PRIORITY: "RUSTRIGHT_BUILD_PRIORITY", RIGHTKIT_PARENT_REQUEST_ID: "RUSTRIGHT_PARENT_REQUEST_ID", RIGHTKIT_EXCLUSIVE: "RUSTRIGHT_EXCLUSIVE",
  RIGHTKIT_CANCEL_ON_DISCONNECT: "RUSTRIGHT_CANCEL_ON_DISCONNECT", RIGHTKIT_CALLING_GATE: "RUSTRIGHT_CALLING_GATE", RIGHTKIT_SNAPSHOT_REQUEST: "RUSTRIGHT_SNAPSHOT_REQUEST",
  RIGHTKIT_SNAPSHOT_ADMISSION: "RUSTRIGHT_SNAPSHOT_ADMISSION", RIGHTKIT_CHECK_ADMISSION: "RUSTRIGHT_CHECK_ADMISSION", RIGHTKIT_GATE_RUN: "RUSTRIGHT_GATE_RUN",
  RIGHTKIT_ATTEST_NONCE: "RUSTRIGHT_ATTEST_NONCE", RIGHTKIT_REQUEST_ID_FILE: "RUSTRIGHT_REQUEST_ID_FILE", RIGHTKIT_INPUT_MANIFEST: "RUSTRIGHT_INPUT_MANIFEST",
  RIGHTKIT_OUTPUT_ROOTS: "RUSTRIGHT_OUTPUT_ROOTS", RIGHTKIT_ADAPTER_ENV: "RUSTRIGHT_ADAPTER_ENV", RIGHTKIT_FORWARD_ENV: "RUSTRIGHT_FORWARD_ENV",
  RIGHTKIT_PARENT_CAPABILITY_TOKEN: "RUSTRIGHT_PARENT_CAPABILITY_TOKEN", RIGHTKIT_CUTOVER_WAIT_MS: "RUSTRIGHT_CUTOVER_WAIT_MS", RIGHTKIT_RECONNECT_WAIT_MS: "RUSTRIGHT_RECONNECT_WAIT_MS",
  RIGHTKIT_RUN_ROOT: "RUSTRIGHT_RUN_ROOT", RIGHTKIT_MANAGED_AGENT_SHIM: "RUSTRIGHT_MANAGED_AGENT_SHIM",
  RIGHTKIT_BUILD_BROKER_SOCKET: "RUSTRIGHT_BUILD_BROKER_SOCKET",
});

/** The legacy interface names set in `env` (case-insensitive, as Windows environments are). */
export const legacyInterfaceNamesIn = (env) => Object.keys(env ?? {}).filter((key) => Object.hasOwn(LEGACY_INTERFACE_ENV, key.toUpperCase()));

/** `env` with every legacy interface name translated to the current spelling (the current value wins) and removed; the same object when there is none. */
export function currentInterfaceEnv(env) {
  const legacy = legacyInterfaceNamesIn(env);
  if (legacy.length === 0) return env;
  const out = { ...env };
  const present = new Set(Object.keys(out).map((key) => key.toUpperCase()));
  for (const key of legacy) {
    const current = LEGACY_INTERFACE_ENV[key.toUpperCase()];
    if (!present.has(current)) { out[current] = out[key]; present.add(current); }
    delete out[key];
  }
  return out;
}
