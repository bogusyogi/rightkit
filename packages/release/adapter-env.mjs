// The request environment of a carried adapter call (rustright#190; rustright-cli shims.rs adapter_environment, rustright-policy adapter.rs
// check_environment). A `rustright run <adapter> -- <args>` request carries exactly the variables named by RUSTRIGHT_ADAPTER_ENV (a comma
// list), each set in the client's own environment. The door (build-launch.mjs) sets RUSTRIGHT_ADAPTER_ENV for a step from the env the step itself declares
// (`adapterEnvLaunch`); the seam sends the step's environment to the client unchanged (stage-seam.mjs runStageAdapter).
//
// What the gate no longer judges (plan section 4, "no adapter grammar"): whether a name is reserved or registered. The client refuses a reserved name before it
// sends (shims.rs adapter_environment, `is_reserved_env_key`), and the broker refuses a reserved or unregistered key, a signing-material key and a handoff path
// that is not absolute (RK_ADAPTER_ENV_DENIED, admission_impl.rs `admit_adapter` -> adapter.rs `check_environment`). What stays here is what no one else refuses:
// signing material and the signing-file path never ride the request, and the comma list the gate builds itself must be able to carry what it names.
export const ADAPTER_ENV_VAR = "RUSTRIGHT_ADAPTER_ENV";

/**
 * RA-08: the path of the transient Android signing-properties file (android-release.mjs ANDROID_SIGNING_PROPERTIES_ENV). It is a typed FILE handoff of the provisioned signer session (native-adapters.mjs
 * `signingFileProblem`, `nativeSigningFileRefusal`), never an adapter request environment: the door refuses a step that declares it, the carrier's client never receives it, and
 * `RUSTRIGHT_ADAPTER_ENV` never names it. Only the path exists in the environment of a direct host's own Gradle; the file holds the secrets.
 */
export const SIGNING_FILE_ENV = "RIGHTKIT_ANDROID_SIGNING_PROPERTIES";
export const ADAPTER_ENV_SIGNING_FILE = "RK_ADAPTER_ENV_SIGNING_FILE";
/** Whether `key` names the signing-properties file handoff (case-insensitive, as RustRight compares). */
export const isSigningFileEnvKey = (key) => String(key).toUpperCase() === SIGNING_FILE_ENV;

export const ADAPTER_ENV_UNSET = "RK_ADAPTER_ENV_UNSET";
export const ADAPTER_ENV_INVALID = "RK_ADAPTER_ENV_INVALID";

/**
 * GB-06 (BROKER-CONTRACT section 16): signing material never rides an adapter request. RustRight reserves `TAURI_SIGNING_*` (the empty value that suppresses signing included), the legacy
 * `TAURI_PRIVATE_KEY` and `TAURI_KEY_PASSWORD` (adapter.rs RESERVED_ENV_PREFIXES), so the client and the broker refuse a request that names one. The gate refuses a step that declares one WITH a value
 * before anything is launched, so the secret is never in a process environment the gate hands to the client, and a name the recipe sets EMPTY (it only suppresses signing in a direct session) is not
 * forwarded rather than refused. Signing, the updater seal and their read-back stay in the provisioned signer session, so a carried Tauri build runs unsigned and RightKit seals outside the broker.
 */
export const SIGNING_ENV_PREFIXES = Object.freeze(["TAURI_SIGNING_", "TAURI_PRIVATE_KEY", "TAURI_KEY_PASSWORD"]);
export const ADAPTER_ENV_SIGNING = "RK_ADAPTER_ENV_SIGNING";
/** Whether `key` is signing material (case-insensitive, as RustRight compares). */
export const isSigningEnvKey = (key) => { const upper = String(key).toUpperCase(); return SIGNING_ENV_PREFIXES.some((prefix) => upper.startsWith(prefix)); };

/**
 * The signed-artifact handoff (GB-05; adapter.rs QA_HANDOFF_ENV): the sign step's two outputs reach a consumer under exactly these two names. They are
 * not secrets and not broker-owned; the broker exempts exactly these two from the reserved RIGHTKIT_ prefix, requires them registered in the adapter's `env`
 * and requires an absolute path with no `..` (adapter.rs check_environment).
 */
export const QA_HANDOFF_ENV = Object.freeze(["RIGHTKIT_QA_APP_BINARY", "RIGHTKIT_QA_SIGNING_EVIDENCE"]);

function fail(code, message) {
  const error = new Error(`${code}: ${message}`);
  error.code = code;
  throw error;
}

const validName = (name) => typeof name === "string" && name !== "" && !/[,=\0\s]/.test(name);
const validValue = (value) => typeof value === "string" && !value.includes("\0");

/**
 * What the door adds to a carried adapter step's launch environment for the env the step declares: `{}` for none (no RUSTRIGHT_ADAPTER_ENV at
 * all), else RUSTRIGHT_ADAPTER_ENV naming exactly the step's variables, plus those variables with their values. A signing-material name with a value, the signing-file handoff, a name that
 * cannot ride a comma list, a declared name with no value, or a value that is not a string is refused with a coded error. Reserved and unregistered names are the client's and the broker's to refuse.
 */
export function adapterEnvLaunch(stepEnv) {
  // GB-06: signing material is never forwarded. A signing variable the step declares with a value is material and refuses the step at planning (the name, never the value, is reported); one the
  // recipe sets EMPTY only suppresses signing in a direct session, carries nothing, and is not forwarded (the carried build runs unsigned; the seal is made outside the broker).
  const declared = Object.keys(stepEnv ?? {});
  const handoff = declared.find((name) => isSigningFileEnvKey(name));
  if (handoff !== undefined) fail(ADAPTER_ENV_SIGNING_FILE, `step env ${JSON.stringify(handoff)} is the signing-properties file handoff: its path travels only as the typed signing-file handoff of a native job (native-adapters.mjs nativeSigningFileRefusal), never as an adapter request environment, and no adapter registers it`);
  const material = declared.filter((name) => isSigningEnvKey(name) && String(stepEnv[name] ?? "") !== "");
  if (material.length) fail(ADAPTER_ENV_SIGNING, `step env ${material.map((n) => JSON.stringify(n)).join(", ")} is signing material: it never rides an adapter request (RustRight reserves TAURI_SIGNING_*, TAURI_PRIVATE_KEY and TAURI_KEY_PASSWORD, RK_ADAPTER_ENV_DENIED); signing and the updater seal stay in the provisioned signer session, and a carried Tauri build runs unsigned`);
  const droppedSigning = declared.filter((name) => isSigningEnvKey(name)).sort();
  const names = declared.filter((name) => !isSigningEnvKey(name)).sort();
  const launch = (value) => { if (droppedSigning.length) Object.defineProperty(value, "signingEnvDropped", { value: droppedSigning, enumerable: false }); return value; };
  if (names.length === 0) return launch({});
  for (const name of names) {
    if (!validName(name)) fail(ADAPTER_ENV_INVALID, `step env name ${JSON.stringify(name)} cannot be forwarded to an adapter`);
    const value = stepEnv[name];
    if (value === undefined || value === null) fail(ADAPTER_ENV_UNSET, `step env ${name} is declared but has no value`);
    if (!validValue(String(value))) fail(ADAPTER_ENV_INVALID, `step env ${name} has a value that cannot be forwarded`);
  }
  return launch({ [ADAPTER_ENV_VAR]: names.join(","), ...Object.fromEntries(names.map((name) => [name, String(stepEnv[name])])) });
}
