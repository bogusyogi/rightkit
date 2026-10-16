// right-release Android lane: a signed Android App Bundle (AAB) built by Gradle, with the same release-tier refusals as the
// desktop lanes. Nothing here is specific to one app: the Gradle command, the keystore and the AAB path come from the
// app's release config (`targets.android`).
//
// Config (every key is checked by validateAndroidTarget; unknown signing keys are refused so a typo cannot smuggle a secret):
//   targets.android = {
//     signed: true,
//     signingContract: "android-aab-upload-key-v1",
//     gradle: { cmd: "./gradlew", args: [":app:bundleRelease"], cwd: "android", timeoutMs?, env? },   // must run bundle<Variant>Release
//     ndk?: { abis?: ["arm64-v8a"], jniLibs: "android/app/src/main/jniLibs", manifestPath?, package?, features?, apiLevel? },   // android-ndk.mjs: cargo-ndk before Gradle
//     aab: { file: "android/app/build/outputs/bundle/release/app-release.aab" },
//     artifacts: ["android/app/build/outputs/bundle/release/app-release.aab"],                          // seals the AAB
//     signing: {
//       keystore: { path: "~/keys/app-upload.jks", alias: "upload" },                                    // outside the repository
//       storePassword: { credential: "app-upload-store" },                                                // OS credential store, or
//       keyPassword: { file: "~/keys/app-upload-key.pass" },                                              // a 0600 file; optional (defaults to storePassword)
//       expectedCertSha256: "<64 hex>",                                                                   // SHA-256 of the upload certificate
//     },
//   }
//
// Secrets: a keystore or key password never appears in the environment, in an argument or in a log line.
//   - Any ambient environment variable that looks like a signing password is refused on every host (not only managed hosts).
//   - The passwords are read from the OS credential store (macOS Keychain `rightkit-android-<name>`, a protected token file
//     on Windows and Linux) or from a file outside the repository (mode 0600 on POSIX), then written to a transient
//     0600 `signing.properties` in a private temp directory. Gradle gets only the file's PATH, in
//     RIGHTKIT_ANDROID_SIGNING_PROPERTIES; the app's signingConfig reads storeFile/storePassword/keyAlias/keyPassword from it.
//   - The file is zeroed and removed when Gradle ends, on failure, and on process exit.
//
// After the build the AAB is read in-process (android-aab-scan.mjs): QA/debug markers in every .so, web-bundle mocks,
// one upload-key signature whose certificate matches expectedCertSha256 (never the Android debug certificate). A receipt
// with the AAB digest is written; androidSealEntry() turns it into the sealed-manifest `files` entry.
import { createHash } from "node:crypto";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { describeAabProblems, extractNativeLibs, scanAab } from "./android-aab-scan.mjs";
import { runAndroidNdk, validateAndroidNdk } from "./android-ndk.mjs";

export const ANDROID_SIGNING_CONTRACT = "android-aab-upload-key-v1";
export const ANDROID_SIGNING_PROPERTIES_ENV = "RIGHTKIT_ANDROID_SIGNING_PROPERTIES";
export const ANDROID_RECEIPT_NAME = "android-aab.json";
export const ANDROID_ENV_SECRET_REFUSED = "ANDROID_ENV_SECRET_REFUSED";
export const ANDROID_SECRET_MISSING = "ANDROID_SECRET_MISSING";

export class AndroidReleaseError extends Error {
  constructor(code, message) { super(message); this.name = "AndroidReleaseError"; this.code = code; }
}
const refuse = (code, message) => { throw new AndroidReleaseError(code, message); };

// ---- environment secrets -----------------------------------------------------------------------------------------
// Signing passwords are refused when found in the ambient environment, on every host. Names only, never values.
const SECRET_ENV_NAME = /^(?:ANDROID_|KEYSTORE|KEY_?STORE|STORE_|KEY_|RELEASE_|SIGNING_|UPLOAD_|ORG_GRADLE_PROJECT_)\w*(?:PASS|PWD)/i;
const OPTION_ENV_NAMES = ["GRADLE_OPTS", "JAVA_OPTS", "JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS"];
const SECRET_OPTION_VALUE = /pass(?:word|phrase)?\s*=|signing\.(?:store|key)|storepass|keypass/i;

export function androidEnvSecretNames(env = process.env) {
  const names = [];
  for (const [name, value] of Object.entries(env)) {
    if (!String(value ?? "").trim()) continue;
    if (SECRET_ENV_NAME.test(name)) names.push(name);
    else if (OPTION_ENV_NAMES.includes(name.toUpperCase()) && SECRET_OPTION_VALUE.test(String(value))) names.push(name);
  }
  return names;
}

export function refuseAndroidEnvSecrets(env = process.env) {
  const [name] = androidEnvSecretNames(env);
  if (name) {
    refuse(ANDROID_ENV_SECRET_REFUSED, `${name} is refused: an Android signing secret must never be passed in the environment; `
      + "keep it in the OS credential store or a 0600 file named by targets.android.signing (see android-release.mjs)");
  }
}

// ---- config validation -------------------------------------------------------------------------------------------
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const nonEmpty = (value) => typeof value === "string" && value.trim() !== "";
const expandHome = (value) => value === "~" ? os.homedir() : /^~[\\/]/.test(value) ? path.join(os.homedir(), value.slice(2)) : value;
const slashed = (value) => String(value).replaceAll("\\", "/").replace(/^\.\//, "");
const insideRoot = (root, file) => { const rel = path.relative(path.resolve(root), path.resolve(file)); return rel === "" || (!rel.startsWith("..") && !path.isAbsolute(rel)); };

const GRADLE_RELEASE_TASK = /^(?::?[\w.-]+:)*bundle(?:[A-Z][A-Za-z0-9]*)?Release$/;
const GRADLE_SECRET_ARG = /pass(?:word|phrase)?\s*=|storepass|keypass|signing\.(?:store|key)\.|debuggable/i;
const GRADLE_SECRET_ENV = /pass|pwd|secret|token|keystore/i;
const SIGNING_KEYS = ["keystore", "storePassword", "keyPassword", "expectedCertSha256"];

export function validateSecretSource(value, label) {
  if (typeof value === "string") throw new Error(`${label} must be { credential } or { file }; an inline secret is refused`);
  if (!isObject(value)) throw new Error(`${label} must be { credential: "<name>" } or { file: "<path>" }`);
  const keys = Object.keys(value);
  const unknown = keys.filter((key) => key !== "credential" && key !== "file");
  if (unknown.length) throw new Error(`${label} has unsupported key(s) ${unknown.join(", ")}; a secret comes only from the OS credential store or a file`);
  if (keys.length !== 1) throw new Error(`${label} must name exactly one of credential or file`);
  if (keys[0] === "credential" && !/^[a-z0-9][a-z0-9-]{0,63}$/.test(String(value.credential))) throw new Error(`${label}.credential must be a lowercase name (letters, digits, hyphens)`);
  if (keys[0] === "file" && !nonEmpty(value.file)) throw new Error(`${label}.file must be a path`);
  return { ...value };
}

// Returns the normalised target, or throws an Error whose message names the problem. `root` is the app root.
export function validateAndroidTarget(target, { root }) {
  if (!isObject(target)) throw new Error("has no target");
  if (target.signed !== true) throw new Error("must declare signed: true");
  if (target.signingContract !== ANDROID_SIGNING_CONTRACT) throw new Error(`must declare signingContract: ${ANDROID_SIGNING_CONTRACT}`);

  const gradle = target.gradle;
  if (!isObject(gradle) || !nonEmpty(gradle.cmd) || !Array.isArray(gradle.args) || gradle.args.length === 0 || gradle.args.some((arg) => typeof arg !== "string")) {
    throw new Error("must declare gradle: { cmd, args[] } (the command that builds the release bundle)");
  }
  if (/\s/.test(gradle.cmd.trim())) throw new Error("gradle.cmd must be one executable; put its arguments in gradle.args");
  if (gradle.envFiles !== undefined) throw new Error("gradle.envFiles is refused: no secret may reach Gradle through the environment");
  for (const name of Object.keys(gradle.env ?? {})) {
    if (GRADLE_SECRET_ENV.test(name)) throw new Error(`gradle.env.${name} is refused: signing secrets are never passed through the environment`);
  }
  const bad = gradle.args.find((arg) => GRADLE_SECRET_ARG.test(arg));
  if (bad !== undefined) throw new Error("gradle.args carries a password, signing or debuggable property; the signing passwords come only from targets.android.signing");
  const tasks = gradle.args.filter((arg) => !arg.startsWith("-") && !/[=/\\]/.test(arg));
  const debugTask = tasks.find((task) => /debug/i.test(task));
  if (debugTask) throw new Error(`gradle task ${debugTask} is a debug build; a release builds bundle<Variant>Release`);
  const task = tasks.find((candidate) => GRADLE_RELEASE_TASK.test(candidate));
  if (!task) throw new Error("gradle.args must run a bundle<Variant>Release task (for example :app:bundleRelease)");

  if (!isObject(target.aab) || !nonEmpty(target.aab.file) || !/\.aab$/i.test(target.aab.file)) throw new Error("must declare aab: { file } naming the .aab Gradle produces");
  const declared = (target.artifacts ?? []).map((artifact) => slashed(typeof artifact === "string" ? artifact : artifact?.file));
  if (!declared.includes(slashed(target.aab.file))) throw new Error("artifacts must list aab.file so the bundle is checked and sealed");

  const signing = target.signing;
  if (!isObject(signing)) throw new Error("must declare signing: { keystore, storePassword, expectedCertSha256 }");
  const unknown = Object.keys(signing).filter((key) => !SIGNING_KEYS.includes(key));
  if (unknown.length) throw new Error(`signing has unsupported key(s) ${unknown.join(", ")}; secrets are never inline`);
  const keystore = signing.keystore;
  if (!isObject(keystore) || !nonEmpty(keystore.path) || !nonEmpty(keystore.alias) || /[\u0000-\u001f]/.test(keystore.alias)) throw new Error("signing.keystore must be { path, alias }");
  if (Object.keys(keystore).some((key) => key !== "path" && key !== "alias")) throw new Error("signing.keystore accepts only path and alias");
  const keystorePath = path.resolve(root, expandHome(keystore.path));
  if (insideRoot(root, keystorePath)) throw new Error("signing.keystore.path must be outside the repository: an upload key never lives in the app tree");
  if (/^debug\.keystore$/i.test(path.basename(keystorePath)) || /^androiddebugkey$/i.test(keystore.alias)) throw new Error("the Android debug keystore cannot sign a release");
  const storePassword = validateSecretSource(signing.storePassword, "signing.storePassword");
  const keyPassword = signing.keyPassword === undefined ? null : validateSecretSource(signing.keyPassword, "signing.keyPassword");
  const pin = String(signing.expectedCertSha256 ?? "").replace(/:/g, "").toLowerCase();
  if (!/^[0-9a-f]{64}$/.test(pin)) throw new Error("signing.expectedCertSha256 must be the SHA-256 (64 hex digits) of the upload certificate");

  const ndk = target.ndk === undefined ? null : validateAndroidNdk(target.ndk, { root, kind: "release" });

  return {
    gradle: { ...gradle, args: [...gradle.args] },
    ndk,
    task,
    aab: { file: target.aab.file },
    signing: { keystorePath, alias: keystore.alias, storePassword, keyPassword, expectedCertSha256: pin },
  };
}

// ---- secret sources ----------------------------------------------------------------------------------------------
function readKeychainSecret({ service, account }) {
  const result = spawnSync("security", ["find-generic-password", "-a", account, "-s", service, "-w"], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
  if (result.error || result.status !== 0) return undefined;
  return result.stdout;
}

// A secret file: regular file, and on POSIX not readable by group or others. A missing file is undefined.
export function readSecretFile(file, { platform = process.platform } = {}) {
  let stat;
  try { stat = statSync(file); } catch (error) {
    if (error.code === "ENOENT") return undefined;
    throw new Error("secret file is unreadable");
  }
  if (!stat.isFile()) throw new Error("secret path is not a regular file");
  if (platform !== "win32" && (stat.mode & 0o077) !== 0) throw new Error("secret file must not be readable by group or others (chmod 600)");
  try { return readFileSync(file, "utf8"); } catch { throw new Error("secret file is unreadable"); }
}

export function androidSecretStoreCommand(name, platform = process.platform) {
  if (platform === "darwin") return `security add-generic-password -a "$USER" -s rightkit-android-${name} -U -w`;
  if (platform === "win32") {
    return `powershell -NoProfile -Command "$s = Read-Host -AsSecureString 'Android ${name}'; $d = Join-Path $env:APPDATA 'RightKit'; New-Item -ItemType Directory -Force -Path $d | Out-Null; [IO.File]::WriteAllText((Join-Path $d 'android-${name}.token'), [Runtime.InteropServices.Marshal]::PtrToStringBSTR([Runtime.InteropServices.Marshal]::SecureStringToBSTR($s)))"`;
  }
  return `umask 077; mkdir -p ~/.config/RightKit; read -rs p && printf %s "$p" > ~/.config/RightKit/android-${name}.token`;
}

// Same storage pattern as protected-credentials.mjs: macOS Keychain generic password (account $USER), otherwise a
// protected token file. Injectable so tests never touch a real Keychain or profile directory.
export function createAndroidCredentialStore({ env = process.env, platform = process.platform, readKeychain = readKeychainSecret, readFile = readSecretFile } = {}) {
  return {
    platform,
    read(name) {
      let value;
      if (platform === "darwin") value = readKeychain({ service: `rightkit-android-${name}`, account: env.USER || os.userInfo().username });
      else if (platform === "win32") value = readFile(path.win32.join(env.APPDATA || path.join(os.homedir(), "AppData", "Roaming"), "RightKit", `android-${name}.token`));
      else value = readFile(path.join(env.XDG_CONFIG_HOME || path.join(env.HOME || os.homedir(), ".config"), "RightKit", `android-${name}.token`));
      const trimmed = typeof value === "string" ? value.trim() : "";
      if (!trimmed) refuse(ANDROID_SECRET_MISSING, `Android signing secret "${name}" is not in the protected credential store; store it once with: ${androidSecretStoreCommand(name, platform)}`);
      return trimmed;
    },
  };
}

export function resolveAndroidSecret(source, { root, store, label, readFile = readSecretFile }) {
  if (source.credential) return store.read(source.credential);
  const file = path.resolve(root, expandHome(source.file));
  if (insideRoot(root, file)) refuse("ANDROID_SECRET_IN_REPOSITORY", `${label} file must be outside the repository`);
  let value;
  try { value = readFile(file); } catch (error) { refuse("ANDROID_SECRET_UNREADABLE", `${label}: ${error.message}`); }
  const trimmed = typeof value === "string" ? value.trim() : "";
  if (!trimmed) refuse(ANDROID_SECRET_MISSING, `${label} file is missing or empty`);
  return trimmed;
}

// ---- transient signing properties --------------------------------------------------------------------------------
// java.util.Properties.load(InputStream) reads ISO-8859-1 and treats these characters specially.
export function escapeJavaProperty(value) {
  const text = String(value);
  let out = "";
  for (let i = 0; i < text.length; i += 1) {
    const code = text.charCodeAt(i); const ch = text[i];
    if (ch === "\\") out += "\\\\";
    else if (ch === "\n") out += "\\n";
    else if (ch === "\r") out += "\\r";
    else if (ch === "\t") out += "\\t";
    else if (ch === "\f") out += "\\f";
    else if (code < 0x20 || code > 0x7e) out += `\\u${code.toString(16).padStart(4, "0")}`;
    else if ("=: #!".includes(ch)) out += `\\${ch}`;
    else out += ch;
  }
  return out;
}

export function createSigningProperties({ keystore, alias, storePassword, keyPassword, tmpRoot = os.tmpdir() }) {
  const dir = mkdtempSync(path.join(tmpRoot, "rightkit-android-sign-"));
  const file = path.join(dir, "signing.properties");
  const content = [`storeFile=${escapeJavaProperty(keystore)}`, `storePassword=${escapeJavaProperty(storePassword)}`, `keyAlias=${escapeJavaProperty(alias)}`, `keyPassword=${escapeJavaProperty(keyPassword)}`, ""].join("\n");
  try {
    writeFileSync(file, content, { flag: "wx", mode: 0o600 });
    try { chmodSync(dir, 0o700); } catch { /* best effort on hosts without POSIX modes */ }
  } catch {
    rmSync(dir, { recursive: true, force: true });
    throw new AndroidReleaseError("ANDROID_SIGNING_PROPERTIES_FAILED", "cannot write the transient signing properties file");
  }
  const length = Buffer.byteLength(content);
  let done = false;
  const cleanup = () => {
    if (done) return;
    done = true;
    try { writeFileSync(file, Buffer.alloc(length)); } catch { /* already gone */ }
    try { rmSync(dir, { recursive: true, force: true }); } catch { /* best effort */ }
  };
  return { file, dir, cleanup };
}

// ---- verification, receipt and seal entry ------------------------------------------------------------------------
const sha256File = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");

export function verifyAabSigner(signature, signing, now = new Date()) {
  if (!signature) refuse("ANDROID_AAB_UNSIGNED", "the AAB has no readable upload-key signature");
  if (signature.debugCertificate) refuse("ANDROID_AAB_DEBUG_SIGNED", "the AAB is signed with the Android debug certificate");
  if (signature.certSha256 !== signing.expectedCertSha256) {
    refuse("ANDROID_AAB_SIGNER_MISMATCH", `the AAB signer certificate ${signature.certSha256} is not signing.expectedCertSha256 ${signing.expectedCertSha256}`);
  }
  if (signature.notAfter != null && signature.notAfter <= now.getTime()) refuse("ANDROID_AAB_CERT_EXPIRED", "the AAB signer certificate has expired");
}

export function androidSealEntry(receipt) {
  return { role: "aab", name: receipt.file.name, sha256: receipt.file.sha256, sizeBytes: receipt.file.sizeBytes };
}

export function writeAndroidReceipt(dir, receipt) {
  mkdirSync(dir, { recursive: true });
  const file = path.join(dir, ANDROID_RECEIPT_NAME);
  writeFileSync(file, `${JSON.stringify(receipt, null, 2)}\n`);
  return file;
}

// For the sealing step: the receipt must describe exactly the AAB that is about to be sealed.
export function verifyAndroidReceipt(receiptPath, aabFile) {
  const receipt = JSON.parse(readFileSync(receiptPath, "utf8"));
  if (receipt.schema !== 1 || receipt.contract !== ANDROID_SIGNING_CONTRACT) throw new Error("not an Android AAB receipt");
  if (path.basename(aabFile) !== receipt.file?.name) throw new Error("AAB receipt names a different file");
  if (statSync(aabFile).size !== receipt.file.sizeBytes || sha256File(aabFile) !== receipt.file.sha256) throw new Error("AAB bytes differ from the build receipt");
  return receipt;
}

// ---- the lane ----------------------------------------------------------------------------------------------------
// runGradle(command, extraEnv): runs the configured Gradle command with extraEnv added (release.mjs: runCommand).
// scanFiles(label, files, { requireBinary, prePack }): release.mjs's assertNoQaControlBinaries (QA/debug rules plus the
//   release-switch binary layer); it refuses the release itself.
export async function runAndroidRelease({
  root, app = "app", target, dryRun = false, env = process.env, platform = process.platform,
  runGradle, runNdk = runGradle, resolveArtifact = (file) => path.resolve(root, file), scanFiles, receiptDir,
  allowContentOnlyDevtools = false, credentialStore, tmpRoot = os.tmpdir(), now = () => new Date(), log = console.log,
}) {
  refuseAndroidEnvSecrets(env);
  const aab = resolveArtifact(target.aab.file);
  const { signing } = target;
  if (dryRun) {
    log(`right-release: dry-run: android ${target.task} -> ${aab}; signing passwords would come from ${signing.storePassword.credential ? "the credential store" : "a file"}, never the environment`);
    if (target.ndk) await runAndroidNdk({ ndk: target.ndk, profile: "release", dryRun: true, run: (command) => runNdk(command, {}), log });
    await runGradle(target.gradle, {});
    return { dryRun: true, aab };
  }
  if (!existsSync(signing.keystorePath) || !statSync(signing.keystorePath).isFile()) refuse("ANDROID_KEYSTORE_MISSING", "the configured upload keystore file is missing");
  const store = credentialStore ?? createAndroidCredentialStore({ env, platform });
  const storePassword = resolveAndroidSecret(signing.storePassword, { root, store, label: "signing.storePassword" });
  const keyPassword = signing.keyPassword ? resolveAndroidSecret(signing.keyPassword, { root, store, label: "signing.keyPassword" }) : storePassword;

  // A stale bundle from an earlier build must never be mistaken for this build's output.
  rmSync(aab, { force: true });
  // Native libraries first (no secret involved): cargo-ndk --release per ABI into the generated jniLibs directory.
  if (target.ndk) await runAndroidNdk({ ndk: target.ndk, profile: "release", run: (command) => runNdk(command, {}), log });
  const props = createSigningProperties({ keystore: signing.keystorePath, alias: signing.alias, storePassword, keyPassword, tmpRoot });
  const onExit = () => props.cleanup();
  process.on("exit", onExit); // release.mjs ends a failed step with process.exit(); the secret file must not outlive it
  try {
    log(`right-release: android ${target.task} (signing properties passed by path only)`);
    await runGradle(target.gradle, { [ANDROID_SIGNING_PROPERTIES_ENV]: props.file });
  } finally {
    props.cleanup();
    process.removeListener("exit", onExit);
  }
  if (!existsSync(aab) || !statSync(aab).isFile()) refuse("ANDROID_AAB_MISSING", `Gradle finished but ${target.aab.file} was not produced`);

  const scan = scanAab(aab, { allowContentOnlyDevtools });
  const problems = describeAabProblems(scan);
  if (problems.length) {
    throw new AndroidReleaseError("ANDROID_AAB_REFUSED", `${app} android AAB ${path.basename(aab)} refused: it must be a signed bundle with no QA control server, devtools, remote debugging or browser-QA mock; ${problems.length} finding(s):\n${problems.join("\n")}`);
  }
  verifyAabSigner(scan.signature, signing, now());
  // Same shared rules as the desktop lanes, over the finished package and over the native libraries inside it.
  scanFiles?.("android AAB", [aab], { requireBinary: true, prePack: true });
  const libDir = mkdtempSync(path.join(tmpRoot, "rightkit-android-libs-"));
  try { scanFiles?.("android AAB native libraries", extractNativeLibs(aab, libDir), { requireBinary: true, prePack: true }); }
  finally { rmSync(libDir, { recursive: true, force: true }); }

  const receipt = {
    schema: 1,
    contract: ANDROID_SIGNING_CONTRACT,
    app,
    builtAt: now().toISOString(),
    gradleTask: target.task,
    file: { name: path.basename(aab), sha256: sha256File(aab), sizeBytes: statSync(aab).size },
    signer: { alias: signing.alias, block: scan.signature.block, certSha256: scan.signature.certSha256, subject: scan.signature.subject },
    scan: { entries: scan.entries, webFiles: scan.webFiles, nativeLibs: scan.nativeLibs.map(({ name, sha256 }) => ({ name, sha256 })) },
  };
  const receiptPath = writeAndroidReceipt(receiptDir ?? path.join(root, ".right-release", "receipts"), receipt);
  log(`right-release: android AAB ${receipt.file.name} sha256 ${receipt.file.sha256} signed by ${receipt.signer.certSha256}; receipt ${receiptPath}`);
  return { dryRun: false, aab, receipt, receiptPath, sealEntry: androidSealEntry(receipt) };
}
