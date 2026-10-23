// Release preflight — verify THIS MACHINE can actually complete a build, before
// the expensive work starts.
//
// Why this file exists: every one of these preconditions used to be discovered
// by failing at it, one per build cycle, deep inside a multi-minute compile,
// usually with an error that names a symptom rather than the cause. The
// knowledge then lived in a commit message, a doc, or an agent's memory — which
// only helps whoever happens to read it. An executable check helps everyone,
// deterministically, in about a second.
//
// Rules for anything added here:
//  - It must be a precondition that can FAIL A BUILD, and it must be checkable
//    cheaply. No network calls, no compiles.
//  - It must name the actual cause and the actual fix, not the symptom.
//  - It must not cry wolf. A check that warns on healthy machines gets ignored,
//    and then so does every other check. (Concrete example of what NOT to add:
//    signing-certificate expiry. Azure Trusted Signing mints short-lived ~3-day
//    certs on purpose and timestamps the signature, so "expires in 2 days" is
//    the healthy state, not a problem.)

import { existsSync, lstatSync, readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { brokerManagedHost } from "./cargo-contract.mjs";
import { createOwnershipEvidence } from "./ownership-evidence.mjs";
import { appRuntimeBudgetAdoptionCheck } from "./runtime-budgets.mjs";
import { isSwiftProject } from "./project-type.mjs";

export const PREFLIGHT_OK = "ok";
export const PREFLIGHT_WARN = "warn";
export const PREFLIGHT_FAIL = "fail";

function ok(name, detail) {
  return { name, status: PREFLIGHT_OK, detail };
}
function warn(name, detail, fix) {
  return { name, status: PREFLIGHT_WARN, detail, fix };
}
function fail(name, detail, fix) {
  return { name, status: PREFLIGHT_FAIL, detail, fix };
}

/**
 * Windows SDK tools are versioned directories; pinning one version is why builds
 * broke on machines that had a different SDK installed. Find the newest by
 * NUMERIC segment order — string sort puts 10.0.9 above 10.0.26100.
 */
export function findWindowsSdkTool(
  tool,
  { sdkRoot = "C:\\Program Files (x86)\\Windows Kits\\10\\bin", arch = "x64", readdir = readdirSync, exists = existsSync } = {},
) {
  if (!exists(sdkRoot)) return null;
  let entries;
  try {
    entries = readdir(sdkRoot);
  } catch {
    return null;
  }
  const candidates = entries
    .filter((entry) => /^10\.\d+(\.\d+)*$/.test(entry))
    .map((entry) => ({
      version: entry,
      segments: entry.split(".").map((part) => Number(part) || 0),
      toolPath: path.join(sdkRoot, entry, arch, tool),
    }))
    .filter((entry) => exists(entry.toolPath));
  if (candidates.length === 0) return null;
  candidates.sort((a, b) => {
    const length = Math.max(a.segments.length, b.segments.length);
    for (let index = 0; index < length; index += 1) {
      const diff = (b.segments[index] ?? 0) - (a.segments[index] ?? 0);
      if (diff !== 0) return diff;
    }
    return 0;
  });
  return candidates[0];
}

/**
 * OpenSSL's ./Configure is a Perl script that needs a real Windows Perl. When a
 * build is launched from Git Bash/MSYS, PATH resolves `perl` to MSYS Perl first,
 * which lacks Locale::Maketext::Simple and dies with an error that reads like an
 * OpenSSL problem. Strawberry Perl is usually installed and simply shadowed —
 * nothing is missing, it just is not the one that wins.
 */
export function resolveWindowsPerl({
  env = process.env,
  exists = existsSync,
  candidates = ["C:\\Strawberry\\perl\\bin\\perl.exe", "C:\\Perl64\\bin\\perl.exe"],
} = {}) {
  const explicit = [env.OPENSSL_SRC_PERL, env.PERL].filter(Boolean);
  for (const candidate of [...explicit, ...candidates]) {
    if (exists(candidate)) return { perlPath: candidate, dir: path.win32.dirname(candidate) };
  }
  return null;
}

/** True when the dependency graph vendors OpenSSL, i.e. a Perl build is required. */
export function graphVendorsOpenssl(lockPath, { read = readFileSync, exists = existsSync } = {}) {
  if (!exists(lockPath)) return false;
  try {
    return /name = "openssl-src"/.test(read(lockPath, "utf8"));
  } catch {
    return false;
  }
}

/**
 * right-release refuses to rebuild an already-sealed version, so a stale sealed
 * record turns into a hard stop at build start. Report the next free version
 * instead of just saying "sealed".
 */
export function nextFreeVersion(sealedDir, app, currentVersion, { readdir = readdirSync, exists = existsSync } = {}) {
  if (!exists(sealedDir)) return { taken: [], collision: false, suggestion: currentVersion };
  let entries = [];
  try {
    entries = readdir(sealedDir);
  } catch {
    return { taken: [], collision: false, suggestion: currentVersion };
  }
  const prefix = `${app}-`;
  const taken = new Set();
  for (const entry of entries) {
    if (!entry.startsWith(prefix)) continue;
    const version = entry.slice(prefix.length).split("-")[0];
    if (/^\d+\.\d+\.\d+$/.test(version)) taken.add(version);
  }
  if (!taken.has(currentVersion)) {
    return { taken: [...taken], collision: false, suggestion: currentVersion };
  }
  const [major, minor] = currentVersion.split(".").map(Number);
  let patch = Number(currentVersion.split(".")[2]);
  let suggestion = currentVersion;
  do {
    patch += 1;
    suggestion = `${major}.${minor}.${patch}`;
  } while (taken.has(suggestion));
  return { taken: [...taken], collision: true, suggestion };
}

/** The path itself, or its nearest existing ancestor (df cannot measure a directory that does not exist yet). */
export function nearestExisting(target) {
  let current = path.resolve(target);
  for (;;) {
    if (existsSync(current)) return current;
    const parent = path.dirname(current);
    if (parent === current) return null;
    current = parent;
  }
}

function freeDiskGb(targetPath) {
  if (process.platform === "win32") {
    const drive = path.parse(path.resolve(targetPath)).root.replace(/\\$/, "");
    const result = spawnSync(
      "powershell",
      ["-NoProfile", "-Command", `(Get-PSDrive -Name '${drive.replace(":", "")}').Free`],
      { encoding: "utf8", windowsHide: true },
    );
    const bytes = Number(String(result.stdout ?? "").trim());
    return Number.isFinite(bytes) && bytes > 0 ? bytes / 1024 ** 3 : null;
  }
  const result = spawnSync("df", ["-k", targetPath], { encoding: "utf8" });
  const line = String(result.stdout ?? "").trim().split("\n").at(-1) ?? "";
  const available = Number(line.split(/\s+/)[3]);
  return Number.isFinite(available) ? (available * 1024) / 1024 ** 3 : null;
}

function commandVersion(command, args = ["--version"]) {
  const result = spawnSync(command, args, { encoding: "utf8", windowsHide: true, shell: process.platform === "win32" });
  if (result.status !== 0) return null;
  return String(result.stdout ?? result.stderr ?? "").trim().split("\n")[0] || null;
}

/**
 * Run every precondition check. Pure-ish: all filesystem/process access is via
 * the injected helpers so this is testable without a real toolchain.
 */
export function collectPreflight({
  platform,
  appRoot,
  repoRoot,
  env = process.env,
  app,
  version,
  configPath,
  cargoLockPaths = [],
  nativeLayout,
  targetLink,
  minFreeGb = Number.isFinite(Number(env.RIGHT_RELEASE_MIN_FREE_GB)) && Number(env.RIGHT_RELEASE_MIN_FREE_GB) > 0 ? Number(env.RIGHT_RELEASE_MIN_FREE_GB) : 25,
  versionProbe = commandVersion,
  isBrokerManagedHost = brokerManagedHost,
  ownership = true,
  ownershipEvidenceFactory = createOwnershipEvidence,
  ownershipScope = null,
  ownershipScopeError = null,
  runtimeBudgetAdoption = appRuntimeBudgetAdoptionCheck,
  freeDisk = freeDiskGb,
  projectType,
  // false for a native iOS app with no Cargo host: no target bridge, Rust compiler cache or Rust budget adoption applies.
  rust = true,
  // false skips only the target bridge and the sccache check (a native iOS app that pins a Cargo.lock has no broker target).
  bridge = true,
  msix = false,
  findSdkTool = findWindowsSdkTool,
} = {}) {
  const checks = [];
  const windows = platform === "win" || platform === "win32";
  const swift = isSwiftProject(projectType);
  const noRust = swift || !rust;
  const noBridge = noRust || !bridge;

  if (swift) {
    const swiftVersion = versionProbe("swift");
    checks.push(
      swiftVersion
        ? ok("swift", swiftVersion)
        : fail("swift", "swift was not found on PATH; SwiftPM apps build with the Swift toolchain", "install the pinned Swift toolchain (swift.org / Swift on Windows) and verify: swift --version"),
    );
  }

  if (appRoot && !noBridge) {
    const target = targetLink ?? nativeLayout?.targetLink ?? path.join(appRoot, "src-tauri", "target");
    const targetLabel = nativeLayout?.targetPrefix || "src-tauri/target/";
    let entry = null;
    try {
      entry = lstatSync(target);
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
    }
    checks.push(
      !entry || entry.isSymbolicLink()
        ? ok("target-bridge", entry ? `${targetLabel} is a symbolic link` : `${targetLabel} is ready for the shared cache bridge`)
        // A real directory here is not corruption on a broker-managed host: the
        // build broker owns CARGO_TARGET_DIR and this is its own output (or a
        // leftover from before it took ownership), not Cache V2's bridge target.
        // Recommending `cache migrate` here is how an agent nearly renamed ~5.9 GB
        // of warm broker intermediates out from under it.
        : brokerManagedHost(env)
          ? ok("target-bridge", `${target} is a real directory; broker-managed host, left untouched`)
          : fail(
              "target-bridge",
              `${target} is a real directory; bridge setup will refuse it`,
              `run right-release cache migrate --config ${JSON.stringify(configPath ?? path.join(appRoot, "right-release.config.mjs"))} --dry-run, then repeat with --apply before building`,
            ),
    );
  }

  if (version && app && repoRoot) {
    const sealedDir = path.join(repoRoot, ".right-release", "sealed");
    const sealed = nextFreeVersion(sealedDir, app, version);
    checks.push(
      sealed.collision
        ? fail(
            "version",
            `${app} ${version} is already sealed at ${sealedDir}; right-release will refuse to rebuild it`,
            `bump to ${sealed.suggestion} in package.json and ${nativeLayout?.manifestPrefix ?? "src-tauri/"}Cargo.toml, then re-run the legal notices generator`,
          )
        : ok("version", `${version} is free to build`),
    );
  }

  if (windows) {
    // makeappx/signtool are MSIX requirements; NSIS/portable targets sign through sign-windows.
    if (msix) {
      const makeappx = process.env.MAKEAPPX_PATH ? { version: "MAKEAPPX_PATH" } : findSdkTool("makeappx.exe");
      checks.push(
        makeappx
          ? ok("windows-sdk", `makeappx.exe from SDK ${makeappx.version}`)
          : fail(
              "windows-sdk",
              "msix target configured but no makeappx.exe found under any installed Windows 10/11 SDK",
              "install the Windows SDK, or set MAKEAPPX_PATH to an explicit makeappx.exe",
            ),
      );
    }

    const signtool = findSdkTool("signtool.exe");
    const signtoolMissing = "install the Windows SDK Build Tools, or set AZURE_SIGNTOOL_PATH";
    if (signtool) checks.push(ok("signtool", `signtool.exe from SDK ${signtool.version}`));
    else if (msix) checks.push(fail("signtool", "msix target configured but no signtool.exe found under any installed Windows SDK", signtoolMissing));
    else checks.push(warn("signtool", "no signtool.exe found under any installed Windows SDK", signtoolMissing));

    const vendorsOpenssl = !noRust && cargoLockPaths.some((lockPath) => graphVendorsOpenssl(lockPath));
    if (vendorsOpenssl) {
      const perl = resolveWindowsPerl({ env });
      checks.push(
        perl
          ? ok("perl", `${perl.perlPath} (prepended to PATH for the vendored OpenSSL build)`)
          : fail(
              "perl",
              "the dependency graph vendors OpenSSL, whose ./Configure needs a real Windows Perl, and none was found",
              "install Strawberry Perl (https://strawberryperl.com), or set OPENSSL_SRC_PERL to a Windows perl.exe. MSYS/Git-Bash perl will NOT work: it lacks Locale::Maketext::Simple",
            ),
      );
    }
  }

  if (noBridge) {
    // No Rust compiler cache applies to SwiftPM targets or to a native iOS app without a Cargo host.
  } else if (isBrokerManagedHost(env)) {
    checks.push(ok("compiler-cache", "broker-managed; compiler-cache selection and status are recorded in build receipts"));
  } else {
    const sccache = versionProbe("sccache");
    checks.push(
      sccache
        ? ok("sccache", `${sccache} visible on shell PATH`)
        : warn("sccache", "sccache was not found by this shell PATH probe; cache availability is not determined here", null),
    );
  }

  if (appRoot) {
    // The floor protects the volume builds write to: the cargo target directory when the layout names one
    // (builds go there, often on a different drive from the app), else the app root.
    const free = freeDisk(nearestExisting(nativeLayout?.targetDir ?? appRoot) ?? appRoot);
    if (free === null) {
      checks.push(warn("disk", "could not determine free disk space", null));
    } else {
      checks.push(
        free < minFreeGb
          ? fail(
              "disk",
              `${free.toFixed(1)}GB free on the build drive, below the ${minFreeGb}GB floor`,
              "prune Rust target directories, or run right-release cache prune",
            )
          : ok("disk", `${free.toFixed(1)}GB free`),
      );
    }
  }

  // Held-resource crates are proven at adoption: an app that links one needs its own installed measurement.
  if (appRoot && !noRust) {
    const adoption = runtimeBudgetAdoption({ appRoot, app, version, cargoLockPaths });
    if (adoption) checks.push(adoption);
  }

  if (repoRoot && ownership && ownershipScopeError) {
    // The target's scan scope could not be determined (the build refuses the same config): fail closed, never scan wider.
    checks.push(fail("ownership", `ownership scope refused: ${ownershipScopeError}`, "fix the target's ownership scope configuration; the build refuses it too"));
  } else if (repoRoot && ownership) {
    // An app living inside the RightKit workspace repository is scoped to its own directory.
    const workspace = existsSync(path.join(repoRoot, "tools", "rightkit", "packages", "release"));
    checks.push(ownershipCheck(repoRoot, { scanRoot: workspace && appRoot ? appRoot : repoRoot, scope: ownershipScope, evidenceFactory: ownershipEvidenceFactory }));
  }

  return checks;
}

/** Release-blocking: the app keeps no unexcepted copy of a RightKit-owned capability. */
export function ownershipCheck(repoRoot, options = {}) {
  let checked;
  try { checked = (options.evidenceFactory ?? createOwnershipEvidence)(repoRoot, options.scanRoot ?? repoRoot, options.scope ?? null); }
  catch (error) { return fail("ownership", `ownership evidence failed: ${error.message}`, "restore protected RightKit ownership trust and catalog"); }
  const { result, evidence, floor } = checked;
  if (checked.ok) {
    const excepted = result.covered.length;
    const detail = excepted ? `no unapproved copies (${excepted} excepted findings)` : "no copies of RightKit-owned capabilities";
    // Pending approval enrollment is a warning only (#262); real findings still fail below.
    return checked.enrollmentPending ? { ...warn("ownership", `${detail}; ${checked.warning}`, null), evidence, result, floor } : { ...ok("ownership", detail), evidence };
  }
  const first = result.violations[0];
  const detail = !floor.ok
    ? floor.problems.join("; ")
    : first
      ? `${result.violations.length} own cop${result.violations.length === 1 ? "y" : "ies"} of RightKit capabilities, e.g. ${first.fingerprint} at ${first.file}${first.line ? `:${first.line}` : ""}`
      : result.problems[0] ?? result.coverage[0]?.reason ?? "ownership scan incomplete";
  return { ...fail("ownership", detail, "run: right-release ownership (move to RightKit, or add a dated entry to the repo exceptions file)"), evidence };
}

export function formatPreflight(checks) {
  const symbol = { [PREFLIGHT_OK]: "ok  ", [PREFLIGHT_WARN]: "WARN", [PREFLIGHT_FAIL]: "FAIL" };
  const lines = checks.map((check) => {
    const head = `  [${symbol[check.status]}] ${check.name}: ${check.detail}`;
    return check.fix && check.status !== PREFLIGHT_OK ? `${head}\n         fix: ${check.fix}` : head;
  });
  return lines.join("\n");
}

export function preflightFailures(checks) {
  return checks.filter((check) => check.status === PREFLIGHT_FAIL);
}

/**
 * PATH additions the build must inherit. Today this is only Windows Perl, but it
 * is the right home for any "the tool is installed and simply not the one that
 * wins" fix — those are invisible to a plain existence check.
 */
export function buildPathPrefix({ platform, env = process.env } = {}) {
  if (platform !== "win" && platform !== "win32") return null;
  const perl = resolveWindowsPerl({ env });
  return perl ? perl.dir : null;
}
