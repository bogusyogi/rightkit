// The `sign` step of `tests.suites` (rightkit#1303): a gate-run test build is signed with the product's one signing identity before anything runs it,
// so a QA run exercises the same code identity the release carries (macOS keys privacy grants to the designated requirement).
//
//   { name: "sign-app", sign: { files: [{ from: "build-app", path: "aarch64-apple-darwin/debug/heardright" }, ...] } }
//
// It runs IN-PROCESS inside `right-release test`, in the gate's own session and outside the broker (like QA: the signing agent and the Keychain are
// the session's, a broker job has neither), and it is never a carried step (build-launch.mjs refuses it). It reuses the release signers and adds no
// ceremony of its own: macOS signs through signMacosEntries, which signs through runMacCodesignCommands (the signing agent) with the identity
// resolveMacosDevelopmentSigning picks; Windows signs and verifies through sign-windows.mjs (signWindowsFiles, verifyWindowsFiles).
//
// What it does, in this order, and refuses at the first thing that is not so:
//   1. the identity is derived: the product's tauri.conf.json `identifier` (mac install.bundleId must equal it); the team is SIGN_TEAM and the
//      Windows signer subject SIGN_SUBJECT; ad hoc signing (RIGHTKIT_DEV_SIGNING, install.signing) is refused;
//   2. each `from` names a suite that ran before in this run; its carried receipt's output directory holds `path` (a path that leaves it, by `..` or
//      a link, is refused);
//   3. the file is copied (sha256 checked) into a gate-managed directory outside the snapshot, the checkout and every broker output directory, and the
//      copy is signed. The source bytes are never touched;
//   4. read back from the signed copy: mac `Identifier`, `TeamIdentifier` and the designated requirement, which must EQUAL the release-shaped one
//      (expectedDesignatedRequirement) and be neither ad hoc nor cdhash-bound; Windows Authenticode with the signer subject;
//   5. only then the evidence (file, sha256 before and after, identifier, team or subject, requirement) is written, and the main binary's signed copy
//      becomes RIGHTKIT_QA_APP_BINARY for the suites after it.
// The main binary (identifier = the bundle id) is the file named by tauri.conf.json `mainBinaryName` when it declares one, else the first file; every
// other file is a sidecar whose identifier is the release identifier resolveBundleIdentifier gives it (`sign.identifiers` / `identifierTemplate`, keyed
// `Contents/MacOS/<name>` as in the bundle). RIGHTKIT_QA_APP_BINARY is the main binary's signed copy (unset by a step that signs no main binary).
import { createHash, randomBytes } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { appendLedger, SCAN_TOKEN_ENV } from "./cargo-ledger.mjs";
import { carriedCargoMapOf } from "./carried-cargo.mjs";
import { MacosDevelopmentSigningError, assertStableDesignatedRequirement, expectedDesignatedRequirement, readDevSigningSettings, resolveMacosDevelopmentSigning } from "./development-signing.mjs";
import { lookupBundlePath, resolveBundleIdentifier } from "./mac-bundle-signing.mjs";
import { signMacosEntries } from "./mac-sign-core.mjs";
import { runMacCodesignCommands } from "./macos-signing-session.mjs";
import { resolveManagedRoots } from "./managed-roots.mjs";
import { isInside, realish } from "./qa-snapshot.mjs";
import { requireSignerToken, signWindowsFiles, verifyWindowsFiles } from "./sign-windows.mjs";
import { QA_APP_BINARY_ENV, QA_SIGNED_FILES_ENV, SIGN_SUBJECT, SIGN_TEAM, SuiteSigningError, assertBundleIdMatches, containedRelative, productIdentity } from "./suite-signing-policy.mjs";

const refuse = (code, message, details) => { throw new SuiteSigningError(code, message, details); };
const sha256 = (file) => createHash("sha256").update(fs.readFileSync(file)).digest("hex");
const output = (result) => `${result?.stdout ?? ""}\n${result?.stderr ?? ""}`;

/** Where signed copies live: under the managed data root, which the signing agent admits and the gate never builds in. */
export const defaultSigningDirectory = ({ env = process.env, platform = process.platform } = {}) => path.join(resolveManagedRoots({ env, platform }).dataRoot, "suite-signing");

/** The Authority-independent facts `codesign -dvv` reports. */
function codesignReport(file, { run, env, root }) {
  const result = run("codesign", ["-dvv", file], { cwd: root, env, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], windowsHide: true, shell: false });
  if (result?.error || result?.status !== 0) refuse("SIGN_READBACK_FAILED", `cannot read the signature of ${file}: ${result?.error?.message || output(result).trim() || `exit ${result?.status ?? "unavailable"}`}`);
  const text = output(result);
  return { identifier: /^Identifier=(.+)$/m.exec(text)?.[1]?.trim(), team: /^TeamIdentifier=(.+)$/m.exec(text)?.[1]?.trim(), adhoc: /^Signature=adhoc$/m.test(text) };
}

/** Mac: sign each copy and read it back against the release-shaped identity. Returns one evidence record per file. */
function signMac({ copies, name, config, appRoot, env, run, deps, identifier, install, signedDir, mainIndex }) {
  const settings = readDevSigningSettings(install, config, { root: appRoot });
  let signing;
  try { signing = (deps.resolveSigning ?? resolveMacosDevelopmentSigning)({ root: appRoot, bundle: signedDir, descriptor: { manifestPath: install.manifestPath }, config, env, run }); }
  catch (cause) {
    if (!(cause instanceof MacosDevelopmentSigningError)) throw cause;
    // No Developer ID identity with its private key (or an ad hoc / Apple Development request): a refusal, never a fallback.
    refuse("SIGN_CREDENTIAL_MISSING", `${name}: the Developer ID Application identity of team ${SIGN_TEAM} is not available (${cause.code}): ${cause.message}`, { cause, reason: cause.code });
  }
  // Developer ID of the one team, never ad hoc, never Apple Development: a different identity is a different designated requirement.
  if (signing.adhoc || !/^Developer ID Application:/.test(signing.identity ?? "")) refuse("SIGN_IDENTITY_REFUSED", `suite signing needs the Developer ID Application identity of team ${SIGN_TEAM}; got ${JSON.stringify(signing.identity)}. There is no ad hoc or Apple Development fallback`, { identity: signing.identity });
  if (signing.teamIdentifier !== SIGN_TEAM || !signing.identity.endsWith(`(${SIGN_TEAM})`)) refuse("SIGN_TEAM_MISMATCH", `suite signing resolved ${JSON.stringify(signing.identity)} (team ${signing.teamIdentifier}), not team ${SIGN_TEAM}`, { identity: signing.identity, team: signing.teamIdentifier });
  const entries = copies.map((copy, index) => {
    const relative = `Contents/MacOS/${path.basename(copy.file)}`;
    let id;
    if (index === mainIndex) id = identifier;
    else {
      try { id = resolveBundleIdentifier({ relative, identifiers: settings.identifiers, template: settings.identifierTemplate, bundleId: identifier, plain: true }); }
      catch (cause) { refuse("SIGN_SIDECAR_IDENTIFIER", cause.message); }
    }
    // A sidecar the release signs with no declared identifier is signed by codesign with no --identifier, which derives the file name: the same here
    // (nothing is passed), and the read-back expects exactly that name.
    const entitlements = (index === mainIndex ? lookupBundlePath(settings.entitlements, ".") : undefined) ?? lookupBundlePath(settings.entitlements, relative);
    return { file: copy.file, ...(id !== undefined ? { identifier: id } : {}), expectedIdentifier: id ?? path.basename(copy.file), ...(entitlements ? { entitlements } : {}) };
  });
  const signOne = deps.signOne ?? ((args) => runMacCodesignCommands([args], { env, run, cwd: appRoot })[0]);
  const inspect = deps.inspect ?? ((command, args) => {
    const result = run(command, args, { cwd: appRoot, env, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], windowsHide: true, shell: false });
    if (result?.error || result?.status !== 0) refuse("SIGN_READBACK_FAILED", `${command} ${args.join(" ")} failed: ${output(result).trim() || result?.error?.message}`);
    return output(result);
  });
  let records;
  try { records = signMacosEntries(entries.map(({ expectedIdentifier: _derived, ...entry }) => entry), { identity: signing.identity, signOne, inspect, bundle: signedDir }); }
  catch (cause) { refuse("SIGN_FAILED", `${name}: ${cause.message}`, { cause }); }
  return records.map((record, index) => {
    const entry = entries[index];
    const report = codesignReport(record.file, { run, env, root: appRoot });
    if (report.adhoc) refuse("SIGN_ADHOC", `${copies[index].path}: the signed copy is ad hoc signed`);
    if (report.team !== SIGN_TEAM) refuse("SIGN_TEAM_MISMATCH", `${copies[index].path}: TeamIdentifier=${report.team ?? "missing"}, expected ${SIGN_TEAM}`, { team: report.team });
    if (report.identifier !== entry.expectedIdentifier) refuse("SIGN_IDENTIFIER_MISMATCH", `${copies[index].path}: Identifier=${report.identifier ?? "missing"}, expected ${entry.expectedIdentifier}`, { identifier: report.identifier });
    const expected = expectedDesignatedRequirement(entry.expectedIdentifier, SIGN_TEAM);
    let requirement;
    try { requirement = assertStableDesignatedRequirement({ root: appRoot, installed: record.file, expected, env, run }); }
    catch (error) { if (!(error instanceof MacosDevelopmentSigningError)) throw error; refuse("SIGN_REQUIREMENT_MISMATCH", `${copies[index].path}: ${error.message}`, { expected, actual: error.installedRequirement }); }
    // assertStableDesignatedRequirement is silent about an unreadable or cdhash-bound requirement (it has no grants to protect); here either is a refusal.
    if (requirement !== expected) refuse("SIGN_REQUIREMENT_MISMATCH", `${copies[index].path}: the designated requirement could not be read back, or is cdhash-bound, instead of ${JSON.stringify(expected)}`, { expected });
    return { platform: "mac", file: copies[index].path, signed: record.file, sha256Before: record.before.sha256, sha256After: record.after.sha256, identifier: entry.expectedIdentifier, team: report.team, identity: signing.identity, requirement };
  });
}

/** Windows: sign each copy through sign-windows and check the signer subject. */
function signWin({ copies, name, env, run, deps }) {
  let evidence;
  try {
    evidence = (deps.signWindows ?? ((files) => signWindowsFiles(files)))(copies.map((copy) => copy.file));
    if (!Array.isArray(evidence) || evidence.length !== copies.length) refuse("SIGN_FAILED", `${name}: the Windows signer returned ${Array.isArray(evidence) ? evidence.length : "no"} record(s) for ${copies.length} file(s)`);
    const verified = (deps.verifyWindows ?? ((files) => verifyWindowsFiles(files)))(copies.map((copy) => copy.file));
    if (!Array.isArray(verified) || verified.length !== copies.length) refuse("SIGN_FAILED", `${name}: the Windows verifier returned ${Array.isArray(verified) ? verified.length : "no"} record(s) for ${copies.length} file(s)`);
    verified.forEach((record, index) => { if (record.subject !== SIGN_SUBJECT) refuse("SIGN_SUBJECT_MISMATCH", `${copies[index].path}: signer subject ${JSON.stringify(record.subject)}, expected ${SIGN_SUBJECT}`, { subject: record.subject }); });
    evidence.forEach((record, index) => { if (record.subject !== SIGN_SUBJECT || record.authenticode !== "Valid") refuse("SIGN_SUBJECT_MISMATCH", `${copies[index].path}: signer subject ${JSON.stringify(record.subject)} / ${JSON.stringify(record.authenticode)}, expected ${SIGN_SUBJECT} / Valid`, { subject: record.subject }); });
  } catch (error) {
    if (error instanceof SuiteSigningError) throw error;
    if (error?.code === "SIGN_WINDOWS_TOKEN_UNAVAILABLE") refuse("SIGN_WINDOWS_TOKEN_UNAVAILABLE", `${name}: ${error.message}`, { cause: error });
    refuse("SIGN_FAILED", `${name}: ${error.message}`, { cause: error });
  }
  return evidence.map((record, index) => ({ platform: "win", file: copies[index].path, signed: copies[index].file, sha256Before: record.before.sha256, sha256After: record.after.sha256, subject: record.subject, authenticode: record.authenticode, timestampPresent: record.timestampPresent }));
}

/** The path of `rel` inside one of `dirs` (receipt output directories), the last that holds a regular file there. A link that leaves the directory is refused. */
function locateSource(dirs, rel, label) {
  const found = [];
  for (const dir of dirs) {
    const candidate = path.join(dir, ...rel.split("/"));
    let info;
    try { info = fs.lstatSync(candidate); } catch { continue; }
    if (info.isSymbolicLink()) refuse("SIGN_PATH_ESCAPE", `${label}: ${candidate} is a link; a sign step signs the receipt's own file`);
    if (!info.isFile()) refuse("SIGN_SOURCE_INVALID", `${label}: ${candidate} is not a regular file`);
    if (!isInside(fs.realpathSync.native(candidate), fs.realpathSync.native(dir))) refuse("SIGN_PATH_ESCAPE", `${label}: ${candidate} resolves outside the receipt's output directory ${dir}`);
    found.push(candidate);
  }
  if (!found.length) refuse("SIGN_SOURCE_MISSING", `${label}: no ${rel} in the output directory of the suite's carried receipt (${dirs.join(", ") || "none recorded"})`);
  return found.at(-1);
}

/**
 * Run one sign step. `outputDirsOf(suite)` returns the verified carried receipts' output directories of an earlier suite of this run, or null when it
 * did not run. Returns `{ appBinary, evidence, evidenceFile, signedDir }`; throws SuiteSigningError (the step then sets nothing).
 */
export function runSignStep({ name, sign, appRoot, config, env = process.env, platform = process.platform, outputDirsOf, deps = {}, log = () => {} }) {
  const host = platform === "darwin" ? "mac" : platform === "win32" ? "win" : null;
  const carried = carriedCargoMapOf(env);
  const record = (entry) => { if (carried?.map) try { appendLedger(carried.map.ledgerDir, env[SCAN_TOKEN_ENV], { id: `sign-${process.pid}-${Date.now().toString(36)}-${randomBytes(4).toString("hex")}`, mode: "signing", cwd: appRoot, argv: ["sign", name], ...entry }); } catch (error) { log(`right-release test: cannot record the signing step in the carrier's ledger: ${error.message}`); throw error; } };
  let signedDir = null;
  try {
    if (!host) refuse("SIGN_PLATFORM", `${name}: a sign step runs on macOS (Developer ID) or Windows (Azure Artifact Signing), not ${platform}`);
    // Windows signer readiness comes first: before the signing directory exists and before any copy, the Azure CLI sign-in must yield a silent token.
    if (host === "win") {
      try { (deps.signerPreflight ?? (() => requireSignerToken()))(); }
      catch (error) {
        if (error?.code === "SIGN_WINDOWS_TOKEN_UNAVAILABLE") refuse("SIGN_WINDOWS_TOKEN_UNAVAILABLE", `${name}: ${error.message}`, { cause: error });
        refuse("SIGN_FAILED", `${name}: the Windows signer preflight failed: ${error.message}`, { cause: error });
      }
    }
    // 1. The identity is derived, and ad hoc is never an option.
    if (env.RIGHTKIT_DEV_SIGNING !== undefined) refuse("SIGN_ADHOC_REFUSED", `${name}: RIGHTKIT_DEV_SIGNING is set; a signed suite build is never ad hoc signed`);
    const install = config?.development?.targets?.[host]?.install ?? {};
    if (install.signing !== undefined) refuse("SIGN_ADHOC_REFUSED", `${name}: development.targets.${host}.install.signing ${JSON.stringify(install.signing)} asks for ad hoc signing`);
    const read = (rel) => { try { return fs.readFileSync(path.resolve(appRoot, rel), "utf8"); } catch { return null; } };
    const { identifier, mainBinaryName } = productIdentity({ readFile: read, manifestPath: install.manifestPath });
    assertBundleIdMatches(config, identifier);
    // 2. Where each file comes from.
    if (!carried?.map) refuse("SIGN_RECEIPT_MISSING", `${name}: no carried receipts in this environment (${carried?.error ?? "RIGHTKIT_CARRIED_CARGO_MAP is not set"}); a sign step signs a file named by an earlier suite's signed receipt`);
    if (typeof outputDirsOf !== "function") refuse("SIGN_RECEIPT_MISSING", `${name}: the run has no receipt reader`);
    const sources = sign.files.map((item, index) => {
      const file = { ...item, path: containedRelative(item.path, `${name}.files[${index}].path`) };
      const dirs = outputDirsOf(file.from);
      if (!dirs) refuse("SIGN_FROM_NOT_RUN", `${name}.files[${index}]: suite ${JSON.stringify(file.from)} did not run before this step in this run`);
      return { ...file, dirs, source: locateSource(dirs, file.path, `${name}.files[${index}] (${file.from}:${file.path})`) };
    });
    // 3. A gate-managed copy, outside everything the build owns.
    const signingRoot = deps.signingDirectory ?? defaultSigningDirectory({ env, platform });
    signedDir = path.join(signingRoot, `${name}-${process.pid}-${Date.now().toString(36)}-${randomBytes(4).toString("hex")}`);
    const section = carried.map.broker ?? {};
    const forbidden = [section.snapshot?.path, section.checkout, env.RK_GATE_CHECKOUT, ...sources.flatMap((source) => source.dirs)].filter((p) => typeof p === "string" && p);
    for (const place of forbidden) if (isInside(signedDir, place)) refuse("SIGN_COPY_PLACE", `${name}: the signing directory ${signedDir} is inside ${place} (the snapshot, the checkout or a broker output directory)`);
    fs.mkdirSync(signedDir, { recursive: true, mode: 0o700 });
    // The copies keep their relative paths under one directory, so a sidecar stays beside the main binary the way the build left them.
    const taken = new Set();
    const copies = sources.map((source) => {
      const file = path.join(signedDir, ...source.path.split("/"));
      if (taken.has(file)) refuse("SIGN_COPY_COLLISION", `${name}: two files would be copied to ${source.path}; give them different paths`);
      taken.add(file);
      fs.mkdirSync(path.dirname(file), { recursive: true });
      const before = sha256(source.source);
      fs.copyFileSync(source.source, file);
      fs.chmodSync(file, fs.statSync(source.source).mode & 0o777);
      if (sha256(file) !== before) refuse("SIGN_COPY_MISMATCH", `${name}: the copy of ${source.path} differs from its source`);
      return { path: source.path, from: source.from, source: source.source, file, sha256: before };
    });
    // 4. Sign the copies and read them back.
    const run = deps.run ?? spawnSync;
    // The main binary is the file named by the product's `mainBinaryName` when tauri.conf.json declares one (a step that signs only sidecars names none of
    // them), else the first file. Every other file is a sidecar.
    let mainIndex = 0;
    if (mainBinaryName) {
      const hits = copies.flatMap((copy, index) => (path.basename(copy.path).replace(/\.exe$/i, "") === mainBinaryName ? [index] : []));
      if (hits.length > 1) refuse("SIGN_MAIN_AMBIGUOUS", `${name}: ${hits.length} files are named ${mainBinaryName}, the product's main binary`);
      mainIndex = hits.length ? hits[0] : -1;
    }
    const evidence = host === "mac"
      ? signMac({ copies, name, config, appRoot, env, run, deps, identifier, install, signedDir, mainIndex })
      : signWin({ copies, name, env, run, deps });
    for (const copy of copies) if (sha256(copy.source) !== copy.sha256) refuse("SIGN_SOURCE_CHANGED", `${name}: the receipt's ${copy.path} changed while it was signed; a sign step never modifies its source`);
    // 5. Evidence first, then the variable.
    const evidenceFile = path.join(signedDir, "signing-evidence.json");
    fs.writeFileSync(evidenceFile, `${JSON.stringify({ schema: 1, step: name, ...(host === "mac" ? { identifier, team: SIGN_TEAM } : { subject: SIGN_SUBJECT }), files: evidence }, null, 2)}\n`);
    record({ status: "ran", step: name, signing: evidence });
    // Every signed file by its declared path (main binary and every sidecar), for the consumer that is handed the signed bytes.
    return { appBinary: mainIndex >= 0 ? copies[mainIndex].file : null, signedFiles: Object.fromEntries(copies.map((copy) => [copy.path, copy.file])), evidence, evidenceFile, signedDir };
  } catch (error) {
    // A step that failed leaves no signed copy behind.
    if (signedDir) { try { fs.rmSync(signedDir, { recursive: true, force: true }); } catch { /* the managed root is pruned */ } }
    if (error instanceof SuiteSigningError) { try { record({ status: "refused", step: name, code: error.code, message: error.message }); } catch { /* the failure is thrown either way */ } }
    throw error;
  }
}

export { QA_APP_BINARY_ENV, QA_SIGNED_FILES_ENV };
