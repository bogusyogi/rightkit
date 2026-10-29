import { spawnSync } from "./background-process.mjs";
import { resolveProjectType } from "./project-type.mjs";
import { existsSync, readFileSync, realpathSync } from "node:fs";
import path from "node:path";
import { declaredCargoManifest, hasDefaultTauriManifest, resolveNativeCargoLayout } from "./native-cargo-layout.mjs";

function git(cwd, args) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8", windowsHide: true });
  if (result.status !== 0) {
    throw new Error(`git ${args.join(" ")} failed: ${(result.stderr || result.stdout).trim()}`);
  }
  return result.stdout;
}

// The native call expands Windows 8.3 short names (RUNNER~1), which Git reports in long form.
function canonical(file) {
  return path.normalize(realpathSync.native(file)).toLowerCase();
}

/**
 * The release must run from the primary Git worktree. `split` is a door source split (release-source-identity.mjs, row 7.04): the
 * process then builds the selected commit's snapshot, so the checkout's branch, its HEAD and whether it is detached are irrelevant
 * (GATE-CONTRACT.md: the primary checkout's state is not a build input). Only the primary-worktree identity is still required. Without
 * `split` every check is exactly as it was.
 */
export function assertPrimaryReleaseCheckout(repoRoot, env = process.env, { split = false } = {}) {
  const worktrees = git(repoRoot, ["worktree", "list", "--porcelain"]);
  const gitDir = git(repoRoot, ["rev-parse", "--git-dir"]).trim();
  const configured = spawnSync("git", ["config", "--path", "--get", "core.worktree"], {
    cwd: repoRoot, encoding: "utf8", windowsHide: true,
  });
  const primary = resolvePrimaryWorktree({
    repoRoot,
    worktrees,
    gitDir,
    coreWorktree: configured.status === 0 ? configured.stdout.trim() : "",
  });
  if (!primary) throw new Error("unable to resolve the primary Git worktree");
  if (canonical(repoRoot) !== canonical(primary)) {
    throw new Error(`right-release must be invoked from the primary Git worktree: ${primary}`);
  }
  if (split) return;
  const branch = spawnSync("git", ["symbolic-ref", "--quiet", "HEAD"], { cwd: repoRoot, encoding: "utf8", windowsHide: true });
  if (
    branch.status !== 0
    && !isVerifiedGithubActionsTagCheckout(repoRoot, env)
    && !isVerifiedGithubActionsAdmittedDispatch(repoRoot, env)
  ) {
    throw new Error("right-release requires a branch-attached primary Git checkout; detached HEAD is forbidden");
  }
}

function isVerifiedGithubActionsTagCheckout(repoRoot, env) {
  const ref = env.GITHUB_REF;
  const sha = env.GITHUB_SHA;
  if (
    env.GITHUB_ACTIONS !== "true"
    || env.GITHUB_EVENT_NAME !== "push"
    || env.GITHUB_REF_TYPE !== "tag"
    || !/^refs\/tags\/v.+$/.test(ref ?? "")
    || !/^[a-f0-9]{40}$/.test(sha ?? "")
    || !/^[A-Za-z0-9][A-Za-z0-9-]{0,38}\/[A-Za-z0-9][A-Za-z0-9._-]{0,99}$/.test(env.GITHUB_REPOSITORY ?? "")
  ) return false;
  const head = git(repoRoot, ["rev-parse", "HEAD"]).trim();
  if (head !== sha) return false;
  let tagCommit;
  let origin;
  try {
    tagCommit = git(repoRoot, ["rev-parse", `${ref}^{commit}`]).trim();
    origin = git(repoRoot, ["remote", "get-url", "origin"]).trim();
  } catch {
    return false;
  }
  if (tagCommit !== head) return false;
  const repository = origin.match(/(?:github\.com[/:])([^/\s]+\/[^/\s]+?)(?:\.git)?\/?$/i)?.[1];
  return repository?.toLowerCase() === env.GITHUB_REPOSITORY.toLowerCase();
}

// A tag-last release chain never has a tag to check out: the whole point is that
// publication mints the tag only after qualification passes. Every stage instead
// checks out one exact commit SHA admitted from the default branch, which leaves
// HEAD detached. Refusing that would force the retired tag-first shape back.
//
// This exemption is deliberately STRICTER than the tag one above. A tag proves
// only that some ref points at this commit; here the commit must additionally be
// the exact revision this run was admitted against AND be genuinely reachable
// from the remote branch the dispatch came from, so a detached HEAD parked on an
// arbitrary, orphaned, or force-pushed commit is still refused.
function isVerifiedGithubActionsAdmittedDispatch(repoRoot, env) {
  const sha = env.GITHUB_SHA;
  const admitted = env.RIGHT_GIT_SOURCE_REVISION;
  if (
    env.GITHUB_ACTIONS !== "true"
    || env.GITHUB_EVENT_NAME !== "workflow_dispatch"
    || env.GITHUB_REF_TYPE !== "branch"
    || !/^refs\/heads\/.+$/.test(env.GITHUB_REF ?? "")
    || !/^[a-f0-9]{40}$/.test(sha ?? "")
    || !/^[a-f0-9]{40}$/.test(admitted ?? "")
    || !/^[A-Za-z0-9][A-Za-z0-9-]{0,38}\/[A-Za-z0-9][A-Za-z0-9._-]{0,99}$/.test(env.GITHUB_REPOSITORY ?? "")
  ) return false;
  // The checkout must be the exact revision the admission stage bound this run
  // to — not merely some commit the dispatch happened to land on.
  const head = git(repoRoot, ["rev-parse", "HEAD"]).trim();
  if (head !== admitted) return false;
  let origin;
  try {
    origin = git(repoRoot, ["remote", "get-url", "origin"]).trim();
  } catch {
    return false;
  }
  const repository = origin.match(/(?:github\.com[/:])([^/\s]+\/[^/\s]+?)(?:\.git)?\/?$/i)?.[1];
  if (repository?.toLowerCase() !== env.GITHUB_REPOSITORY.toLowerCase()) return false;
  // Reachability from the remote branch that was dispatched. A shallow or
  // ref-less checkout cannot prove this, and so does not get the exemption.
  const dispatchBranch = (env.GITHUB_REF ?? "").replace(/^refs\/heads\//, "");
  const remoteRef = `refs/remotes/origin/${dispatchBranch}`;
  if (spawnSync("git", ["rev-parse", "--verify", "--quiet", remoteRef], { cwd: repoRoot, windowsHide: true }).status !== 0) return false;
  return spawnSync("git", ["merge-base", "--is-ancestor", head, remoteRef], { cwd: repoRoot, windowsHide: true }).status === 0;
}

export function resolvePrimaryWorktree({ repoRoot, worktrees, gitDir, coreWorktree }) {
  const listed = worktrees.match(/^worktree (.+)$/m)?.[1];
  if (!listed) return undefined;
  const resolvedGitDir = path.resolve(repoRoot, gitDir);
  if (path.resolve(listed) === resolvedGitDir && coreWorktree) {
    return path.resolve(resolvedGitDir, coreWorktree);
  }
  return listed;
}

export function resolveConfiguredBuildInputs(config, target, label = "release config") {
  const configured = target?.buildInputs ?? config?.buildInputs;
  if (!Array.isArray(configured?.include) || configured.include.length === 0) {
    throw new Error(`${label} must declare non-empty buildInputs.include paths`);
  }
  return configured;
}

/** The validated or raw iOS package spec of a target (`targets.ios.iosPackage`, or the recipe the build expands it to). */
export function iosPackageSpecOf(target) {
  if (target?.recipe?.kind === "iosPackage") return target.recipe.spec ?? {};
  if (target?.iosPackage && typeof target.iosPackage === "object") return target.iosPackage;
  return null;
}

/**
 * Native iOS mode: an iOS target with iosPackage and no src-tauri/Cargo.toml (a declared nativeAssembly.cargoManifest does not
 * leave it). The app is an xcodegen project built by xcodebuild, not a Tauri host: no broker target, no target bridge, no
 * Tauri layout. A declared manifest only pins its Cargo.toml and Cargo.lock as inputs; iosPackage.prepare runs the cargo build.
 * An iOS app that has src-tauri/Cargo.toml (Pulse) keeps the Cargo-backed behaviour exactly.
 */
export function isNativeIosTarget({ appRoot, target, exists } = {}) {
  return iosPackageSpecOf(target) !== null && !hasDefaultTauriManifest({ appRoot, exists });
}

/**
 * prepare commands exist to build the app's Rust static library. Without a declared nativeAssembly.cargoManifest the release
 * would hash and seal none of that Rust source graph, so a native iOS target may not carry them.
 */
export function assertNativeIosPrepareHasManifest(iosSpec, config, appRoot) {
  if (declaredCargoManifest(config)) {
    const problems = nativeIosPrepareProblems({ appRoot, config, prepare: iosSpec?.prepare ?? [] });
    if (problems.length) throw new Error(`iosPackage.prepare must be only bare cargo commands when a Rust manifest is declared; express the Rust build as cargo commands in prepare (no node, scripts, pnpm, python, rustup run, +toolchain or other manifest-path), so the features it enables can be checked and the release pins exactly the manifest it builds:\n- ${problems.join("\n- ")}`);
    return;
  }
  if (Array.isArray(iosSpec?.prepare) && iosSpec.prepare.length > 0) {
    throw new Error("targets.ios.iosPackage.prepare builds a Rust library but no Cargo manifest is declared; declare nativeAssembly.cargoManifest (and keep its Cargo.lock beside it, or name it with nativeAssembly.cargoLockPath) so the Rust manifest and lock are pinned as required release inputs");
  }
}

const FEATURE_FLAGS = new Set(["--features", "-F", "--feature"]);
// The only cargo subcommands a native iOS prepare may run: each takes --features and builds exactly one manifest. xtask, aliases
// (which .cargo/config.toml can point anywhere) and cargo-<custom> subcommands are opaque to the feature scan.
const PREPARE_SUBCOMMANDS = new Set(["build", "rustc", "check"]);

/**
 * With a declared Rust manifest, every prepare command is a bare `cargo`: nothing else can be scanned for the features it enables
 * or be trusted to build only the manifest the release pins. Returns the reasons a command is refused (empty when all are fine).
 */
export function nativeIosPrepareProblems({ appRoot, config, prepare = [] }) {
  const declared = declaredCargoManifest(config);
  if (!declared) return [];
  const manifest = path.resolve(appRoot, declared);
  const problems = [];
  prepare.forEach((command, index) => {
    const where = `targets.ios.iosPackage.prepare[${index}] (${command?.cmd})`;
    if (command?.cmd !== "cargo") { problems.push(`${where} is not a bare cargo command`); return; }
    const args = (command.args ?? []).map(String);
    if (args[0]?.startsWith("+")) problems.push(`${where} selects a toolchain with ${args[0]}`);
    else if (!PREPARE_SUBCOMMANDS.has(args[0])) problems.push(`${where} runs cargo ${args[0] ?? "(no subcommand)"}; only cargo build, rustc and check are allowed (no xtask, alias or cargo-<custom> subcommand)`);
    const cwd = path.resolve(appRoot, command.cwd ?? ".");
    let manifestPathSeen = false;
    args.forEach((arg, position) => {
      if (arg === "--config" || arg.startsWith("--config=")) problems.push(`${where} passes ${arg.split("=")[0]}, which can change what cargo builds`);
      if (arg === "-Z" || /^-Z.+/.test(arg)) problems.push(`${where} passes the unstable flag ${arg.split("=")[0]}`);
      if (arg === "--target-dir" || arg.startsWith("--target-dir=")) problems.push(`${where} passes --target-dir; build output goes where the release expects it`);
      const pathValue = arg === "--manifest-path" ? args[position + 1] : arg.startsWith("--manifest-path=") ? arg.slice("--manifest-path=".length) : undefined;
      if (pathValue !== undefined) manifestPathSeen = true;
      if (pathValue !== undefined && path.relative(path.resolve(cwd, pathValue), manifest) !== "") problems.push(`${where} builds ${pathValue}, not the declared manifest ${declared}`);
      const featureValue = FEATURE_FLAGS.has(arg) ? args[position + 1] : arg.startsWith("--features=") ? arg.slice("--features=".length) : /^-F.+/.test(arg) ? arg.slice(2) : undefined;
      if (featureValue !== undefined && /[$%`]/.test(featureValue)) problems.push(`${where} selects features from a variable (${arg}); the Cargo features it enables cannot be determined`);
    });
    // No cwd fallback: cargo would otherwise build whatever Cargo.toml the working directory holds.
    if (!manifestPathSeen) problems.push(`${where} has no --manifest-path; pass --manifest-path ${declared} so it builds exactly the declared manifest`);
    // The lock is hashed before prepare runs; without --locked cargo may rewrite it and build different dependencies.
    if (!args.some((arg) => arg === "--locked" || arg === "--frozen")) problems.push(`${where} has no --locked; pass --locked so cargo builds exactly the pinned Cargo.lock`);
  });
  return problems;
}

/** The repo-relative-to-app Package.resolved an iosPackage with trusted Swift macros/plugins pins, or null when it trusts none. */
export function iosTrustPackageResolved(iosSpec) {
  const listed = (value) => Array.isArray(value) && value.length > 0;
  if (!listed(iosSpec?.trustedSwiftMacros) && !listed(iosSpec?.trustedSwiftPlugins)) return null;
  if (typeof iosSpec.packageResolved === "string" && iosSpec.packageResolved) return iosSpec.packageResolved;
  if (typeof iosSpec.project !== "string" || !iosSpec.project) return null;
  return path.posix.join(iosSpec.project.replaceAll("\\", "/"), "project.xcworkspace", "xcshareddata", "swiftpm", "Package.resolved");
}

const outside = (root, file) => { const relative = path.relative(root, file); return relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative); };

export function resolveReleaseBuildInputs({ repoRoot, appRoot, configPath, config, target, env = process.env }) {
  const swift = resolveProjectType({ appRoot, config, target }) === "swiftpm";
  const iosSpec = iosPackageSpecOf(target);
  const nativeIos = !swift && isNativeIosTarget({ appRoot, target });
  if (nativeIos) assertNativeIosPrepareHasManifest(iosSpec, config, appRoot);
  // Native iOS: a declared manifest is resolved only to pin its Cargo.toml and lock (refused when there is no lock, or when
  // either lies outside the repository). The returned layout is null, so no consumer links a broker target or bridges a cargo target.
  const declaredManifest = nativeIos ? declaredCargoManifest(config) : undefined;
  if (declaredManifest && outside(repoRoot, path.resolve(appRoot, declaredManifest))) throw new Error(`declared Cargo manifest ${declaredManifest} is outside the repository ${repoRoot}; a release pins only sources it can seal`);
  const pinnedLayout = nativeIos
    ? (declaredManifest ? resolveNativeCargoLayout({ appRoot, config, requireManifest: true, requireLock: true, repoRoot }) : null)
    : resolveNativeCargoLayout({
      appRoot,
      config,
      requireManifest: !swift,
      requireLock: !swift,
    });
  if (nativeIos && pinnedLayout && outside(repoRoot, pinnedLayout.lockPath)) throw new Error(`Cargo.lock ${pinnedLayout.lockPath} is outside the repository ${repoRoot}; a release pins only sources it can seal`);
  const nativeLayout = nativeIos ? null : pinnedLayout;
  const optionalIfPresent = (names) => names.map((f) => path.join(appRoot, f)).filter((f) => existsSync(f));
  // The xcodegen spec is an input whenever iosPackage names one (a declared-but-missing spec is refused below).
  // Only native iOS mode: an iOS app with src-tauri keeps exactly its pre-existing input set.
  const iosProjectSpec = nativeIos && typeof iosSpec?.projectSpec === "string" && iosSpec.projectSpec ? [path.resolve(appRoot, iosSpec.projectSpec)] : [];
  // Reviewed Swift macro/plugin trust (iosPackage.trustedSwiftMacros / trustedSwiftPlugins): the Package.resolved holding the pins is a
  // required, hashed, git-tracked release input on both iOS paths (native and src-tauri). Without a list nothing is added.
  const packageResolvedInput = iosTrustPackageResolved(iosSpec);
  if (packageResolvedInput && outside(repoRoot, path.resolve(appRoot, packageResolvedInput))) throw new Error(`Package.resolved ${packageResolvedInput} is outside the repository ${repoRoot}; a release pins only sources it can seal`);
  const iosPackageResolved = packageResolvedInput ? [path.resolve(appRoot, packageResolvedInput)] : [];
  const requiredInputs = [
    configPath,
    ...(swift
      // SwiftPM: Package.swift is required; Package.resolved and the Node manifests only when they exist.
      ? [path.join(appRoot, "Package.swift"), ...optionalIfPresent(["Package.resolved", "package.json", "pnpm-lock.yaml"])]
      : nativeIos
      // Native iOS: package.json and pnpm-lock.yaml only when the app has them, plus the declared Cargo manifest and its lock.
      ? [...optionalIfPresent(["package.json", "pnpm-lock.yaml"]), ...(pinnedLayout ? [pinnedLayout.manifestPath, pinnedLayout.lockPath] : [])]
      : [path.join(appRoot, "package.json"), path.join(appRoot, "pnpm-lock.yaml"), nativeLayout.manifestPath, nativeLayout.lockPath]),
    ...iosProjectSpec,
    ...iosPackageResolved,
    ...(target?.preflight?.files ?? []).map((file) => path.resolve(appRoot, expandEnv(file, env))),
  ].filter((file, index, files) => files.indexOf(file) === index);
  for (const file of requiredInputs) {
    if (!existsSync(file)) throw new Error(`missing required release input: ${file}`);
  }

  const configured = resolveConfiguredBuildInputs(config, target);
  const appPrefix = path.relative(repoRoot, appRoot).replaceAll("\\", "/");
  const qualify = (pattern) => path.posix.normalize([
    appPrefix,
    pattern.replaceAll("\\", "/").replace(/^\.\//, ""),
  ].filter(Boolean).join("/"));
  const required = requiredInputs
    .map((file) => path.relative(repoRoot, file).replaceAll("\\", "/"))
    .filter((file) => file && file !== ".." && !file.startsWith("../") && !path.isAbsolute(file));

  return {
    requiredInputs,
    nativeLayout,
    // The Cargo.lock a native iOS app pins (its layout is null by design); preflight checks it like any other lock.
    cargoLockPaths: nativeIos && pinnedLayout ? [pinnedLayout.lockPath] : [],
    buildInputs: {
      include: configured.include.map(qualify),
      exclude: (configured.exclude ?? []).map(qualify),
      required,
      json: Object.fromEntries(Object.entries(configured.json ?? {}).map(([file, fields]) => [qualify(file), fields])),
    },
  };
}

function pathspec(pattern, exclude = false) {
  const normalized = pattern.replaceAll("\\", "/").replace(/^\.\//, "");
  return `:(top,${exclude ? "exclude," : ""}glob)${normalized}`;
}

function jsonProjectionChanged(repoRoot, file, fields, baseline) {
  try {
    const working = JSON.parse(readFileSync(path.join(repoRoot, file), "utf8"));
    const committed = JSON.parse(git(repoRoot, ["show", `${baseline}:${file}`]));
    const project = (value) => fields.map((segments) => segments.reduce((current, segment) => current?.[segment], value));
    return JSON.stringify(project(working)) !== JSON.stringify(project(committed));
  } catch {
    return true;
  }
}

function fileChangedFromBaseline(repoRoot, file, baseline) {
  const workingPath = path.join(repoRoot, file);
  if (!existsSync(workingPath)) return true;
  try {
    const workingHash = git(repoRoot, ["hash-object", "--", file]).trim();
    const baselineHash = git(repoRoot, ["rev-parse", `${baseline}:${file}`]).trim();
    return workingHash !== baselineHash;
  } catch {
    return true;
  }
}

export function dirtyBuildInputs(repoRoot, inputs, baseline = "HEAD") {
  if (!Array.isArray(inputs?.include) || inputs.include.length === 0) {
    throw new Error("release config must declare non-empty buildInputs.include paths");
  }
  const required = new Set(inputs.required ?? []);
  const files = [
    ...statusFiles(repoRoot, inputs.include, inputs.exclude),
    ...statusFiles(repoRoot, [...required], []),
  ];
  return [...new Set(files)]
    .filter((file) => required.has(file)
      ? fileChangedFromBaseline(repoRoot, file, baseline)
      : inputs.json?.[file]
      ? jsonProjectionChanged(repoRoot, file, inputs.json[file], baseline)
      : fileChangedFromBaseline(repoRoot, file, baseline))
    .sort();
}

function statusFiles(repoRoot, include, exclude = []) {
  if (!include.length) return [];
  const specs = [
    ...include.map((pattern) => pathspec(pattern)),
    ...exclude.map((pattern) => pathspec(pattern, true)),
  ];
  const output = git(repoRoot, ["status", "--porcelain=v1", "-z", "--untracked-files=all", "--", ...specs]);
  const records = output.split("\0");
  const files = [];
  for (let index = 0; index < records.length; index += 1) {
    const record = records[index];
    if (!record) continue;
    const status = record.slice(0, 2);
    files.push(record.slice(3).replaceAll("\\", "/"));
    if (/[RC]/.test(status)) index += 1;
  }
  return files;
}

function expandEnv(value, env) {
  return String(value)
    .replace(/%([^%]+)%/g, (_, name) => env[name] ?? "")
    .replace(/\$\{([^}]+)\}/g, (_, name) => env[name] ?? "");
}
