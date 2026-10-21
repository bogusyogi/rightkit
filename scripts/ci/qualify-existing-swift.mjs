import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

export const REPOSITORY = "bogusyogi/rightkit";
export const PACKAGE_URL = `https://github.com/${REPOSITORY}`;
export const WORKFLOW_REF = `${REPOSITORY}/.github/workflows/qualify-existing-swift.yml@refs/heads/main`;
const HEX40 = /^[a-f0-9]{40}(?![\s\S])/;
const HEX64 = /^[a-f0-9]{64}(?![\s\S])/;
const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?![\s\S])/;
const MAX_TAG_DEPTH = 8;

export function validateInputs(env) {
  if (!SEMVER.test(env.SWIFT_QUALIFY_TAG ?? "")) throw new Error("SWIFT_QUALIFY_TAG must be plain semver");
  if (!HEX40.test(env.SWIFT_QUALIFY_COMMIT ?? "")) throw new Error("SWIFT_QUALIFY_COMMIT must be 40 lowercase hex characters");
  if (!HEX64.test(env.SWIFT_QUALIFY_ARCHIVE_SHA256 ?? "")) throw new Error("SWIFT_QUALIFY_ARCHIVE_SHA256 must be 64 lowercase hex characters");
  if (!env.RUNNER_TEMP || !path.isAbsolute(env.RUNNER_TEMP)) throw new Error("RUNNER_TEMP must be absolute");
  return { tag: env.SWIFT_QUALIFY_TAG, commit: env.SWIFT_QUALIFY_COMMIT, archiveSha256: env.SWIFT_QUALIFY_ARCHIVE_SHA256, runnerTemp: env.RUNNER_TEMP };
}

export function validateRunContext(env, event) {
  if (env.GITHUB_REPOSITORY !== REPOSITORY || event?.repository?.full_name !== REPOSITORY || event.repository.private !== false) throw new Error("qualification requires public bogusyogi/rightkit");
  if (env.GITHUB_EVENT_NAME !== "workflow_dispatch" || env.GITHUB_REF !== "refs/heads/main" || env.RUNNER_OS !== "macOS") throw new Error("qualification requires main workflow_dispatch on macOS");
  if (env.GITHUB_WORKFLOW_REF !== WORKFLOW_REF) throw new Error("qualification requires the qualify-existing-swift workflow on main");
  if (!/^[1-9]\d*(?![\s\S])/.test(env.GITHUB_RUN_ID ?? "") || !/^[1-9]\d*(?![\s\S])/.test(env.GITHUB_RUN_ATTEMPT ?? "") || !HEX40.test(env.GITHUB_SHA ?? "")) throw new Error("missing Actions run identity");
  return { repository: REPOSITORY, runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT, workflow: env.GITHUB_WORKFLOW, workflowRef: env.GITHUB_WORKFLOW_REF, sha: env.GITHUB_SHA, ref: env.GITHUB_REF, event: env.GITHUB_EVENT_NAME, url: `${PACKAGE_URL}/actions/runs/${env.GITHUB_RUN_ID}/attempts/${env.GITHUB_RUN_ATTEMPT}` };
}

/** Validate the complete official API dereference chain, including annotated tags. */
export function validateTagResolution(tag, expectedCommit, ref, objects = []) {
  if (!SEMVER.test(tag) || !HEX40.test(expectedCommit) || ref?.ref !== `refs/tags/${tag}`) throw new Error("tag reference mismatch");
  let object = ref.object;
  const seen = new Set();
  let index = 0;
  while (object?.type === "tag") {
    if (!HEX40.test(object.sha ?? "") || seen.has(object.sha) || index >= MAX_TAG_DEPTH) throw new Error("invalid or excessive annotated tag chain");
    seen.add(object.sha);
    const next = objects[index++];
    if (next?.sha !== object.sha) throw new Error("annotated tag object mismatch");
    object = next.object;
  }
  if (index !== objects.length || object?.type !== "commit" || object.sha !== expectedCommit) throw new Error("tag does not resolve to expected commit");
  return { refSha: ref.object.sha, commit: object.sha, annotatedObjects: objects.map((item) => item.sha) };
}

export function validateConsumerResolution(resolved, tag, commit) {
  if (!SEMVER.test(tag) || !HEX40.test(commit) || ![2, 3].includes(resolved?.version) || !Array.isArray(resolved.pins) || resolved.pins.length !== 1) throw new Error("unexpected consumer resolution format or pins");
  const pin = resolved.pins[0];
  if (pin.identity !== "rightkit" || pin.kind !== "remoteSourceControl" || pin.location !== PACKAGE_URL || pin.state?.version !== tag || pin.state?.revision !== commit || pin.state?.branch != null) throw new Error("consumer did not resolve canonical exact tag & commit");
  return { identity: pin.identity, location: pin.location, version: pin.state.version, revision: pin.state.revision };
}

/** API calls carry the job read token like every sibling CI script; archive bytes never do. */
export function officialHeaders(json, token) {
  if (json && (typeof token !== "string" || token === "")) throw new Error("GITHUB_TOKEN read authentication required");
  return { Accept: json ? "application/vnd.github+json" : "application/octet-stream", "User-Agent": "rightkit-existing-swift-qualification", ...(json ? { "X-GitHub-Api-Version": "2022-11-28", Authorization: `Bearer ${token}` } : {}) };
}

async function fetchOfficial(url, json = false, token) {
  const response = await fetch(url, { redirect: "error", signal: AbortSignal.timeout(120_000), headers: officialHeaders(json, token) });
  if (!response.ok) throw new Error(`official fetch failed: ${response.status} ${url}`);
  return json ? response.json() : Buffer.from(await response.arrayBuffer());
}

async function resolveTag(tag, commit, token) {
  const base = `https://api.github.com/repos/${REPOSITORY}/git`;
  const ref = await fetchOfficial(`${base}/ref/tags/${tag}`, true, token);
  const objects = [];
  let object = ref.object;
  const seen = new Set();
  while (object?.type === "tag") {
    if (!HEX40.test(object.sha ?? "") || seen.has(object.sha) || objects.length >= MAX_TAG_DEPTH) throw new Error("invalid or excessive annotated tag chain");
    seen.add(object.sha);
    const next = await fetchOfficial(`${base}/tags/${object.sha}`, true, token);
    objects.push(next);
    object = next.object;
  }
  return validateTagResolution(tag, commit, ref, objects);
}

// Reject links & special files as well as traversal, before any member is extracted.
const EXTRACT = `import pathlib, sys, tarfile
archive, destination = sys.argv[1:]
root = pathlib.Path(destination).resolve()
with tarfile.open(archive, "r:gz") as source:
    members = source.getmembers()
    tops = set()
    for member in members:
        name = pathlib.PurePosixPath(member.name)
        if name.is_absolute() or ".." in name.parts or not name.parts or not (member.isdir() or member.isfile()):
            raise ValueError("unsafe archive member: " + member.name)
        target = root.joinpath(*name.parts).resolve()
        if not target.is_relative_to(root):
            raise ValueError("archive escape")
        tops.add(name.parts[0])
    if len(tops) != 1:
        raise ValueError("archive must contain one root")
    source.extractall(root, members=members, filter="data")
    print(next(iter(tops)))
`;

export async function qualifyExistingSwift(env = process.env) {
  if (!env.RUNNER_TEMP || !path.isAbsolute(env.RUNNER_TEMP)) throw new Error("RUNNER_TEMP must be absolute");
  const evidence = path.join(fs.realpathSync(env.RUNNER_TEMP), "swift-existing-qualification");
  fs.mkdirSync(evidence, { recursive: true });
  const receiptPath = path.join(evidence, "receipt.json");
  const receipt = { schemaVersion: 1, qualification: "existing-swift-tag", status: "RUNNING", stage: "inputs", startedAt: new Date().toISOString(), nativeStages: [], remoteConsumer: { status: "NOT_RUN" }, namedConsumerProtocolEvidence: "NOT_PROVEN", wholePK16Qualified: false };
  const save = () => fs.writeFileSync(receiptPath, `${JSON.stringify(receipt, null, 2)}\n`);
  save();
  const stage = async (name, operation) => {
    receipt.stage = name;
    save();
    const value = await operation();
    save();
    return value;
  };
  const command = async (name, executable, args, cwd) => stage(name, async () => {
    const logPath = path.join(evidence, `${name}.log`);
    const fd = fs.openSync(logPath, "w");
    const row = { stage: name, executable, args, cwd, log: logPath, status: "RUNNING", startedAt: new Date().toISOString() };
    receipt.nativeStages.push(row);
    save();
    try {
      const result = await new Promise((resolve, reject) => {
        const childEnv = { ...env };
        delete childEnv.GITHUB_TOKEN;
        const child = spawn(executable, args, { cwd, env: { ...childEnv, SWIFTPM_MODULECACHE_OVERRIDE: path.join(work, "module-cache"), CLANG_MODULE_CACHE_PATH: path.join(work, "clang-cache") }, stdio: ["ignore", fd, fd], shell: false });
        const timer = setTimeout(() => child.kill("SIGKILL"), 45 * 60 * 1000);
        child.once("error", (error) => { clearTimeout(timer); reject(error); });
        child.once("close", (code, signal) => { clearTimeout(timer); resolve({ code, signal }); });
      });
      Object.assign(row, result, { status: result.code === 0 && !result.signal ? "PASS" : "FAIL" });
      if (row.status !== "PASS") throw new Error(`${name} failed (exit ${result.code}, signal ${result.signal})`);
    } catch (error) {
      row.status = "FAIL";
      row.error = error.message;
      throw error;
    } finally {
      row.finishedAt = new Date().toISOString();
      fs.closeSync(fd);
      save();
    }
  });
  let work;
  try {
    const inputs = validateInputs(env);
    receipt.actions = validateRunContext(env, JSON.parse(fs.readFileSync(env.GITHUB_EVENT_PATH, "utf8")));
    Object.assign(receipt, { tag: inputs.tag, commit: inputs.commit, archiveURL: `https://codeload.github.com/${REPOSITORY}/tar.gz/${inputs.commit}`, expectedArchiveSha256: inputs.archiveSha256 });
    work = fs.mkdtempSync(path.join(fs.realpathSync(inputs.runnerTemp), "swift-existing-work-"));
    receipt.initialTag = await stage("resolve-tag-before", () => resolveTag(inputs.tag, inputs.commit, env.GITHUB_TOKEN));
    await stage("download-verify-archive", async () => {
      const bytes = await fetchOfficial(receipt.archiveURL);
      receipt.archiveSha256 = createHash("sha256").update(bytes).digest("hex");
      if (receipt.archiveSha256 !== inputs.archiveSha256) throw new Error("archive SHA-256 mismatch; extraction refused");
      fs.writeFileSync(path.join(work, "source.tar.gz"), bytes, { flag: "wx" });
    });
    const extracted = path.join(work, "source");
    fs.mkdirSync(extracted);
    await command("extract-verified-archive", "python3", ["-c", EXTRACT, path.join(work, "source.tar.gz"), extracted], work);
    const roots = fs.readdirSync(extracted);
    if (roots.length !== 1 || roots[0] !== `rightkit-${inputs.commit}`) throw new Error("unexpected archive root");
    const packageDir = path.join(extracted, roots[0], "swift", "RightKitSwift");
    await command("swift-version", "swift", ["--version"], work);
    await command("xcode-version", "xcodebuild", ["-version"], work);
    await command("native-macos-build", "swift", ["build"], packageDir);
    await command("native-macos-test", "swift", ["test"], packageDir);
    for (const platform of ["iOS", "watchOS"]) await command(`native-${platform.toLowerCase()}-build`, "xcodebuild", ["build", "-scheme", "RightKitSwift-Package", "-destination", `generic/platform=${platform}`, "-skipPackagePluginValidation", "CODE_SIGNING_ALLOWED=NO"], packageDir);
    const consumer = path.join(work, "remote-consumer");
    fs.mkdirSync(path.join(consumer, "Sources", "Qualification"), { recursive: true });
    fs.writeFileSync(path.join(consumer, "Package.swift"), `// swift-tools-version: 6.1\nimport PackageDescription\nlet package = Package(name: "Qualification", platforms: [.macOS(.v14)], products: [.executable(name: "Qualification", targets: ["Qualification"])], dependencies: [.package(url: "${PACKAGE_URL}", exact: "${inputs.tag}")], targets: [.executableTarget(name: "Qualification", dependencies: [.product(name: "RightKitFS", package: "rightkit")])])\n`);
    fs.writeFileSync(path.join(consumer, "Sources", "Qualification", "main.swift"), 'import RightKitFS\nprecondition(RightKitAppRoot.isValidAppName("Qualification"))\nprecondition(!RightKitAppRoot.isValidAppName("../escape"))\nprint("RightKitFS remote consumer passed")\n');
    receipt.remoteConsumer.status = "RUNNING";
    const isolation = ["--cache-path", path.join(work, "consumer-cache"), "--config-path", path.join(work, "consumer-config"), "--security-path", path.join(work, "consumer-security")];
    await command("remote-consumer-resolve", "swift", ["package", ...isolation, "--disable-dependency-cache", "resolve"], consumer);
    receipt.remoteConsumer.resolution = await stage("validate-consumer-resolution", () => validateConsumerResolution(JSON.parse(fs.readFileSync(path.join(consumer, "Package.resolved"), "utf8")), inputs.tag, inputs.commit));
    await command("remote-consumer-run", "swift", ["run", ...isolation, "--disable-automatic-resolution", "Qualification"], consumer);
    receipt.remoteConsumer.status = "PASS";
    receipt.finalTag = await stage("resolve-tag-after", () => resolveTag(inputs.tag, inputs.commit, env.GITHUB_TOKEN));
    if (JSON.stringify(receipt.initialTag) !== JSON.stringify(receipt.finalTag)) throw new Error("tag moved during qualification");
    receipt.stage = "finished";
    receipt.status = "PASS";
  } catch (error) {
    receipt.status = "FAIL";
    receipt.error = error.message;
    if (receipt.remoteConsumer.status === "RUNNING") receipt.remoteConsumer.status = "FAIL";
    throw error;
  } finally {
    receipt.finishedAt = new Date().toISOString();
    save();
  }
  return receipt;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  qualifyExistingSwift().catch((error) => { console.error(error.message); process.exitCode = 1; });
}
