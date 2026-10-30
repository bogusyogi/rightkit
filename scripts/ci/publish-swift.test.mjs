import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { parseYaml } from "./workflow-yaml.mjs";
import { REPOSITORY, loadApprovedArtifacts } from "./upload.mjs";
import { assertSwiftDoc, proveSwiftMutations, SWIFT_MUTATIONS } from "./swift-workflow-contract.mjs";
import {
  QUALIFY_COMMANDS, SWIFT_NAME, TREE_DIGEST_CONTRACT, WORKFLOW_REF, checkApproval, checkCheckout, checkIdentity, checkSourceFiles,
  createTag, gitRunner, isCalendarDate, parseInputs, pushCredential, qualify, repositoryOfRemote, tagCommands, treeDigest, verifyAll,
} from "./publish-swift.mjs";

const COMMIT = "a".repeat(40);
const IDENTITY = ["-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid"];
const HEX = (n) => String(n).repeat(64).slice(0, 64);

// ---- inputs and identity -----------------------------------------------------------------------

const goodEnv = (extra = {}) => ({
  GITHUB_ACTIONS: "true", GITHUB_EVENT_NAME: "workflow_dispatch", GITHUB_REPOSITORY: REPOSITORY, GITHUB_REF: "refs/heads/main",
  GITHUB_WORKFLOW_REF: WORKFLOW_REF, GITHUB_SHA: COMMIT, GITHUB_TOKEN: "token", RUNNER_TEMP: os.tmpdir(),
  PUBLISH_SWIFT_VERSION: "0.9.0", PUBLISH_SWIFT_COMMIT: COMMIT, PUBLISH_DRY_RUN: "false", ...extra,
});

test("inputs are strict: plain semver, a full lowercase SHA, a literal boolean", () => {
  assert.deepEqual(parseInputs(goodEnv()), { version: "0.9.0", commit: COMMIT, dryRun: false });
  assert.equal(parseInputs(goodEnv({ PUBLISH_DRY_RUN: "true" })).dryRun, true);
  for (const bad of ["v0.9.0", "0.9", "0.9.0-rc.1", "01.2.3", "", "0.9.0 ", "0.9.0\n--force"]) assert.throws(() => parseInputs(goodEnv({ PUBLISH_SWIFT_VERSION: bad })), /plain semver/, JSON.stringify(bad));
  for (const bad of ["main", "A".repeat(40), "a".repeat(39), "a".repeat(41), "", `${COMMIT} `, "HEAD"]) assert.throws(() => parseInputs(goodEnv({ PUBLISH_SWIFT_COMMIT: bad })), /40-character/, JSON.stringify(bad));
  for (const bad of [undefined, "", "TRUE", "yes", "1"]) assert.throws(() => parseInputs(goodEnv({ PUBLISH_DRY_RUN: bad })), /exactly true or false/, String(bad));
});

test("the Actions identity must be this repository's publish-swift dispatch from main at the requested commit", () => {
  const inputs = { commit: COMMIT };
  assert.equal(checkIdentity(goodEnv(), inputs), true);
  const refusals = [
    [{ GITHUB_ACTIONS: "false" }, /only in GitHub Actions/],
    [{ GITHUB_ACTIONS: undefined }, /only in GitHub Actions/],
    [{ GITHUB_EVENT_NAME: "push" }, /workflow_dispatch/],
    [{ GITHUB_REPOSITORY: "someone-else/rightkit" }, /wrong repository/],
    [{ GITHUB_REF: "refs/heads/topic" }, /refs\/heads\/main/],
    [{ GITHUB_REF: "refs/tags/0.9.0" }, /refs\/heads\/main/],
    [{ GITHUB_WORKFLOW_REF: `${REPOSITORY}/.github/workflows/publish.yml@refs/heads/main` }, /wrong workflow/],
    [{ GITHUB_WORKFLOW_REF: `${REPOSITORY}/.github/workflows/publish-swift.yml@refs/heads/topic` }, /wrong workflow/],
    [{ GITHUB_SHA: "b".repeat(40) }, /not the dispatch ref SHA/],
    [{ GITHUB_TOKEN: "" }, /GITHUB_TOKEN/],
    [{ RUNNER_TEMP: "relative" }, /RUNNER_TEMP/],
  ];
  for (const [extra, pattern] of refusals) assert.throws(() => checkIdentity(goodEnv(extra), inputs), pattern, JSON.stringify(extra));
});

test("repository of a remote ignores credentials, .git and a trailing slash", () => {
  for (const url of ["https://github.com/bogusyogi/rightkit", "https://github.com/bogusyogi/rightkit.git/", "git@github.com:bogusyogi/rightkit.git", "https://x-access-token:abc@github.com/bogusyogi/rightkit.git"]) assert.equal(repositoryOfRemote(url), REPOSITORY, url);
  assert.equal(repositoryOfRemote("/tmp/origin.git"), null);
  assert.equal(repositoryOfRemote("https://github.com/Orthic-Labs/rightkit.git"), "Orthic-Labs/rightkit");
});

// ---- source files ------------------------------------------------------------------------------

function sourceTree({ version = "0.9.0", changelog = `# RightKitSwift changelog\n\n## ${version} - 2026-10-20\n\nbody\n` } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-swift-src-"));
  fs.mkdirSync(path.join(root, "swift", "RightKitSwift"), { recursive: true });
  fs.writeFileSync(path.join(root, "swift", "RightKitSwift", "VERSION"), `${version}\n`);
  fs.writeFileSync(path.join(root, "swift", "RightKitSwift", "CHANGELOG.md"), changelog);
  return { root, cleanup: () => fs.rmSync(root, { recursive: true, force: true }) };
}

test("calendar dates are real dates", () => {
  for (const good of ["2026-10-20", "2028-02-29"]) assert.equal(isCalendarDate(good), true, good);
  for (const bad of ["2026-13-01", "2026-02-30", "2027-02-29", "26-10-20", "2026-1-2", "unpublished", "", undefined]) assert.equal(isCalendarDate(bad), false, String(bad));
});

test("VERSION must equal the version and the changelog needs exactly one dated heading for it", () => {
  const ok = sourceTree();
  try { assert.equal(checkSourceFiles({ root: ok.root, version: "0.9.0" }), "2026-10-20"); } finally { ok.cleanup(); }
  const dash = sourceTree({ changelog: "# c\n\n## 0.9.0 — 2026-10-20\n" });
  try { assert.equal(checkSourceFiles({ root: dash.root, version: "0.9.0" }), "2026-10-20"); } finally { dash.cleanup(); }
  const mismatch = sourceTree({ version: "0.9.1" });
  try { assert.throws(() => checkSourceFiles({ root: mismatch.root, version: "0.9.0" }), /VERSION is "0.9.1", not 0.9.0/); } finally { mismatch.cleanup(); }
  const changelogs = {
    "unpublished staging": "# c\n\n## 0.9.0 — unpublished staging\n",
    "no heading": "# c\n\n## 0.8.1 - 2026-10-18\n",
    "an impossible date": "# c\n\n## 0.9.0 - 2026-02-30\n",
    "the heading of another version": "# c\n\n## 0.9.00 - 2026-10-20\n## 10.9.0 - 2026-10-20\n",
    "two headings": "# c\n\n## 0.9.0 - 2026-10-20\n\n## 0.9.0 - 2026-10-21\n",
    "heading not at line start": "# c\n\n - ## 0.9.0 - 2026-10-20\n",
  };
  for (const [label, changelog] of Object.entries(changelogs)) {
    const tree = sourceTree({ changelog });
    try { assert.throws(() => checkSourceFiles({ root: tree.root, version: "0.9.0" }), /YYYY-MM-DD/, label); } finally { tree.cleanup(); }
  }
  const missing = sourceTree();
  fs.rmSync(path.join(missing.root, "swift", "RightKitSwift", "CHANGELOG.md"));
  try { assert.throws(() => checkSourceFiles({ root: missing.root, version: "0.9.0" }), /unreadable/); } finally { missing.cleanup(); }
});

// ---- reviewer approval -------------------------------------------------------------------------

const entry = (extra = {}) => ({ registry: "swift", name: SWIFT_NAME, version: "0.9.0", sha256: HEX(1), reviewer: "Oracle (agent source-byte review)", reviewRef: "agent-review:/root/swift-0.9.0", ...extra });
function approvedFile(artifacts, extra = {}) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-swift-approved-"));
  const file = path.join(dir, "approved-artifacts.json");
  fs.writeFileSync(file, JSON.stringify({ schema: 2, candidates: [], artifacts, ...extra }));
  return { file, cleanup: () => fs.rmSync(dir, { recursive: true, force: true }) };
}

test("the reviewer-approval model expresses a swift tag and still rejects unknown registries", () => {
  const good = approvedFile([entry()]);
  try { assert.deepEqual(loadApprovedArtifacts(good.file).problems, []); } finally { good.cleanup(); }
  const unknown = approvedFile([entry({ registry: "pip" })]);
  try { assert.match(loadApprovedArtifacts(unknown.file).problems.join(";"), /registry must be npm, crates or swift/); } finally { unknown.cleanup(); }
  const twice = approvedFile([entry(), entry({ sha256: HEX(2) })]);
  try { assert.match(loadApprovedArtifacts(twice.file).problems.join(";"), /approved twice/); } finally { twice.cleanup(); }
});

test("approval needs exactly one swift entry for this name and version with the tree digest", () => {
  const digest = HEX(1);
  const ok = approvedFile([entry()]);
  try { assert.equal(checkApproval({ approvedFile: ok.file, version: "0.9.0", digest }).reviewRef, "agent-review:/root/swift-0.9.0"); } finally { ok.cleanup(); }
  const cases = {
    "no entries": [[], /not reviewer-approved/],
    "another version": [[entry({ version: "0.9.1" })], /not reviewer-approved/],
    "an npm entry with the same name": [[entry({ registry: "npm" })], /not reviewer-approved/],
    "another package name": [[entry({ name: "OtherSwift" })], /not reviewer-approved/],
    "another digest": [[entry({ sha256: HEX(2) })], /is not the reviewer-approved 222222222222/],
    "an uppercase digest": [[entry({ sha256: HEX(1).toUpperCase().replace(/1/g, "A") })], /not valid/],
    "a blank reviewer": [[entry({ reviewer: "" })], /not valid/],
    "an extra key": [[{ ...entry(), extra: true }], /not valid/],
  };
  for (const [label, [artifacts, pattern]] of Object.entries(cases)) {
    const f = approvedFile(artifacts);
    try { assert.throws(() => checkApproval({ approvedFile: f.file, version: "0.9.0", digest }), pattern, label); } finally { f.cleanup(); }
  }
  assert.throws(() => checkApproval({ approvedFile: path.join(os.tmpdir(), "rk-swift-no-such-file.json"), version: "0.9.0", digest }), /not valid/);
});

// ---- qualification -----------------------------------------------------------------------------

test("qualify runs swift build, swift test and the generic iOS and watchOS builds of the root package, in that order", () => {
  const xcode = (platform) => ["xcodebuild", ["build", "-scheme", "RightKitSwift-Package", "-destination", `generic/platform=${platform}`, "-skipPackagePluginValidation", "CODE_SIGNING_ALLOWED=NO"]];
  assert.deepEqual(QUALIFY_COMMANDS, [["swift", ["build"]], ["swift", ["test"]], xcode("iOS"), xcode("watchOS")]);
  const calls = [];
  const count = qualify({ root: "/checkout", log: () => {}, run: (command, args, root) => { calls.push([command, args, root]); return { status: 0 }; } });
  assert.equal(count, 4);
  assert.deepEqual(calls, QUALIFY_COMMANDS.map(([command, args]) => [command, args, "/checkout"]));
});

test("qualify stops at the first failing command and refuses", () => {
  for (let failing = 0; failing < QUALIFY_COMMANDS.length; failing += 1) {
    const calls = [];
    assert.throws(() => qualify({ log: () => {}, run: (command, args) => { calls.push(command); return { status: calls.length === failing + 1 ? 1 : 0 }; } }), /swift publish refused: .* failed \(exit 1\)/, `command ${failing}`);
    assert.equal(calls.length, failing + 1, "nothing runs after the failure");
  }
});

test("qualify refuses a command that cannot start or is killed by a signal", () => {
  assert.throws(() => qualify({ log: () => {}, run: () => ({ error: new Error("spawn swift ENOENT"), status: null }) }), /could not run: spawn swift ENOENT/);
  assert.throws(() => qualify({ log: () => {}, run: () => ({ status: null, signal: "SIGKILL" }) }), /swift build failed/);
  assert.throws(() => qualify({ log: () => {}, run: () => undefined }), /swift build failed/);
});

test("the qualify subcommand takes no argument and is not an input-driven mode", () => {
  const source = fs.readFileSync(new URL("./publish-swift.mjs", import.meta.url), "utf8");
  assert.match(source, /mode === "qualify"[\s\S]*?need\(rest\.length === 0, "usage: publish-swift\.mjs qualify"\)/);
  assert.ok(!/REQUIRED_CI_JOBS|selectCiRun|ciJobProblems|listAttemptJobs|createGitHubActionsClient/.test(source), "the shared-CI guard is gone");
});

// ---- git fixture -------------------------------------------------------------------------------

// A bare origin plus a clone laid out like this repository. Origin's recorded URL is the real GitHub URL; an insteadOf
// rewrite points that URL at the bare repository so fetch, ls-remote and push stay local.
function fixture({ version = "0.9.0" } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-swift-pub-"));
  const origin = path.join(root, "origin.git");
  const work = path.join(root, "work");
  const git = (args, cwd = work) => {
    const result = spawnSync("git", args, { cwd, encoding: "utf8" });
    assert.equal(result.status, 0, `${args.join(" ")}: ${result.stderr}`);
    return result.stdout.trim();
  };
  fs.mkdirSync(origin); git(["init", "-q", "--bare", "-b", "main"], origin);
  fs.mkdirSync(path.join(work, "swift", "RightKitSwift", "Sources"), { recursive: true });
  fs.mkdirSync(path.join(work, "ci"), { recursive: true });
  fs.writeFileSync(path.join(work, "Package.swift"), "// root\n");
  fs.writeFileSync(path.join(work, "swift", "RightKitSwift", "Package.swift"), 'let package = Package(name: "RightKitSwift")\n');
  fs.writeFileSync(path.join(work, "swift", "RightKitSwift", "VERSION"), `${version}\n`);
  fs.writeFileSync(path.join(work, "swift", "RightKitSwift", "CHANGELOG.md"), `# RightKitSwift changelog\n\n## ${version} - 2026-10-20\n`);
  fs.writeFileSync(path.join(work, "swift", "RightKitSwift", "Sources", "A.swift"), "let a = 1\n");
  git(["init", "-q", "-b", "main"]);
  git(["remote", "add", "origin", `https://github.com/${REPOSITORY}.git`]);
  git(["config", `url.${origin}.insteadOf`, `https://github.com/${REPOSITORY}.git`]);
  git(["add", "."]);
  git([...IDENTITY, "commit", "-q", "-m", "first"]);
  const first = git(["rev-parse", "HEAD"]);
  fs.writeFileSync(path.join(work, "README.txt"), "later\n");
  git(["add", "."]);
  git([...IDENTITY, "commit", "-q", "-m", "second"]);
  git(["push", "-q", "origin", "main"]);
  const commit = git(["rev-parse", "HEAD"]);
  const runner = gitRunner(work);
  const stubDrift = (status = 0) => () => ({ status, stdout: "", stderr: status ? "drift" : "" });
  return { root, origin, work, git, first, commit, runner, stubDrift, digest: () => treeDigest(runner, commit), cleanup: () => fs.rmSync(root, { recursive: true, force: true }) };
}

test("the tree digest binds Package.swift and swift/RightKitSwift and nothing else", () => {
  const f = fixture();
  try {
    const digest = f.digest();
    assert.match(digest, /^[0-9a-f]{64}$/);
    assert.equal(treeDigest(f.runner, f.first), digest, "a commit that changes only other paths keeps the digest, so the approval commit may follow the reviewed one");
    assert.equal(TREE_DIGEST_CONTRACT, "rightkit-swift-tree/v1");
    fs.writeFileSync(path.join(f.work, "swift", "RightKitSwift", "Sources", "A.swift"), "let a = 2\n");
    f.git(["add", "."]); f.git([...IDENTITY, "commit", "-q", "-m", "edit source"]);
    assert.notEqual(treeDigest(f.runner, f.git(["rev-parse", "HEAD"])), digest);
    fs.writeFileSync(path.join(f.work, "Package.swift"), "// root changed\n");
    f.git(["add", "."]); f.git([...IDENTITY, "commit", "-q", "-m", "edit root manifest"]);
    const rootChanged = treeDigest(f.runner, f.git(["rev-parse", "HEAD"]));
    assert.notEqual(rootChanged, digest);
    assert.throws(() => treeDigest(f.runner, "b".repeat(40)), /cannot list/);
    assert.throws(() => treeDigest(f.runner, "main"), /full commit SHA/);
  } finally { f.cleanup(); }
});

test("a checkout that passes every guard returns the tree digest", () => {
  const f = fixture();
  try { assert.equal(checkCheckout({ git: f.runner, root: f.work, commit: f.commit, version: "0.9.0", drift: f.stubDrift() }), f.digest()); } finally { f.cleanup(); }
});

test("checkout guards refuse a wrong HEAD, a foreign origin, a dirty tree, an unpushed commit, an existing tag and drift", () => {
  const cases = {
    "HEAD is not the requested commit": [(f) => f.first, /not the requested commit/],
    "origin is another repository": [(f) => { f.git(["config", "remote.origin.url", "https://github.com/Orthic-Labs/rightkit.git"]); f.git(["config", `url.${f.origin}.insteadOf`, "https://github.com/Orthic-Labs/rightkit.git"]); }, /origin is not bogusyogi\/rightkit/],
    "dirty tracked file": [(f) => fs.appendFileSync(path.join(f.work, "README.txt"), "x"), /not clean/],
    "stray untracked file": [(f) => fs.writeFileSync(path.join(f.work, "stray.txt"), "x"), /not clean/],
    "commit not on origin/main": [(f) => { fs.writeFileSync(path.join(f.work, "later.txt"), "x"); f.git(["add", "."]); f.git([...IDENTITY, "commit", "-q", "-m", "unpushed"]); return f.git(["rev-parse", "HEAD"]); }, /not on origin\/main/],
    "tag exists locally": [(f) => f.git(["tag", "0.9.0"]), /already exists locally/],
    "tag exists on origin": [(f) => { f.git(["tag", "0.9.0", f.first]); f.git(["push", "-q", "origin", "refs/tags/0.9.0"]); f.git(["tag", "-d", "0.9.0"]); }, /already exists on origin/],
  };
  for (const [label, [arrange, pattern]] of Object.entries(cases)) {
    const f = fixture();
    try {
      let commit = f.commit;
      const arranged = arrange(f);
      if (/^[0-9a-f]{40}$/.test(arranged ?? "")) commit = arranged;
      assert.throws(() => checkCheckout({ git: f.runner, root: f.work, commit, version: "0.9.0", drift: f.stubDrift() }), pattern, label);
    } finally { f.cleanup(); }
  }
  const f = fixture();
  try { assert.throws(() => checkCheckout({ git: f.runner, root: f.work, commit: f.commit, version: "0.9.0", drift: f.stubDrift(1) }), /drifted/); } finally { f.cleanup(); }
});

// ---- the whole verification and the tag --------------------------------------------------------

function world({ version = "0.9.0", mutateApproved } = {}) {
  const f = fixture({ version });
  const approved = approvedFile([entry({ version, sha256: f.digest() })]);
  if (mutateApproved) mutateApproved(approved);
  const env = goodEnv({ GITHUB_SHA: f.commit, PUBLISH_SWIFT_COMMIT: f.commit, PUBLISH_SWIFT_VERSION: version });
  const verify = (overrides = {}) => verifyAll({ env, root: f.work, git: f.runner, approvedFile: approved.file, drift: f.stubDrift(), ...overrides });
  return { f, approved, env, verify, cleanup: () => { f.cleanup(); approved.cleanup(); } };
}

test("verifyAll returns the evidence of a fully qualified commit and writes nothing", async () => {
  const w = world();
  try {
    const verified = await w.verify();
    assert.equal(verified.version, "0.9.0"); assert.equal(verified.commit, w.f.commit); assert.equal(verified.digest, w.f.digest());
    assert.equal(verified.changelogDate, "2026-10-20"); assert.equal(verified.dryRun, false);
    assert.equal(w.f.git(["tag", "-l"]), ""); assert.equal(w.f.git(["ls-remote", "--tags", "origin"]), "");
  } finally { w.cleanup(); }
});

test("verifyAll refuses each missing proof before anything is tagged", async () => {
  const cases = {
    "no approval entry": [{ mutateApproved: (a) => fs.writeFileSync(a.file, JSON.stringify({ schema: 2, candidates: [], artifacts: [] })) }, /not reviewer-approved/],
    "approval for other bytes": [{ mutateApproved: (a) => fs.writeFileSync(a.file, JSON.stringify({ schema: 2, candidates: [], artifacts: [entry({ sha256: HEX(7) })] })) }, /is not the reviewer-approved/],
  };
  for (const [label, [options, pattern]] of Object.entries(cases)) {
    const w = world(options);
    try {
      await assert.rejects(w.verify(), pattern, label);
    } finally { w.cleanup(); }
  }
  const wrongVersion = world();
  try {
    fs.writeFileSync(path.join(wrongVersion.f.work, "swift", "RightKitSwift", "VERSION"), "0.9.1\n");
    wrongVersion.f.git(["add", "."]); wrongVersion.f.git([...IDENTITY, "commit", "-q", "-m", "bump"]); wrongVersion.f.git(["push", "-q", "origin", "main"]);
    const commit = wrongVersion.f.git(["rev-parse", "HEAD"]);
    await assert.rejects(wrongVersion.verify({ env: { ...wrongVersion.env, GITHUB_SHA: commit, PUBLISH_SWIFT_COMMIT: commit } }), /VERSION is "0.9.1"/);
  } finally { wrongVersion.cleanup(); }
  const notDispatched = world();
  try { await assert.rejects(notDispatched.verify({ env: { ...notDispatched.env, GITHUB_REF: "refs/heads/topic" } }), /refs\/heads\/main/); } finally { notDispatched.cleanup(); }
});

test("an approval commit that follows the reviewed commit keeps the digest, so the order review -> approval entry works", async () => {
  const w = world();
  try {
    const digest = w.f.digest();
    fs.writeFileSync(path.join(w.f.work, "ci", "approved-artifacts.json"), "{}\n");
    w.f.git(["add", "."]); w.f.git([...IDENTITY, "commit", "-q", "-m", "approval entry"]); w.f.git(["push", "-q", "origin", "main"]);
    const later = w.f.git(["rev-parse", "HEAD"]);
    assert.equal(treeDigest(w.f.runner, later), digest);
  } finally { w.cleanup(); }
});

test("the tag is a lightweight tag on the commit and only that one ref is pushed", () => {
  assert.deepEqual(tagCommands("0.9.0", COMMIT), [["git", ["tag", "0.9.0", COMMIT]], ["git", ["push", "origin", "refs/tags/0.9.0:refs/tags/0.9.0"]]]);
  const f = fixture();
  try {
    createTag({ git: f.runner, pushGit: f.runner, version: "0.9.0", commit: f.commit });
    assert.match(f.git(["ls-remote", "--tags", f.origin, "refs/tags/0.9.0"]), new RegExp(`^${f.commit}\\s+refs/tags/0.9.0$`));
    assert.equal(f.git(["cat-file", "-t", "0.9.0"]), "commit", "lightweight, not annotated");
    assert.equal(f.git(["for-each-ref", "refs/heads", "--format=%(refname)"]), "refs/heads/main");
  } finally { f.cleanup(); }
});

test("a rejected push removes the local tag and refuses; origin keeps its existing tag", () => {
  const f = fixture();
  try {
    f.git(["tag", "0.9.0", f.first]); f.git(["push", "-q", "origin", "refs/tags/0.9.0"]); f.git(["tag", "-d", "0.9.0"]);
    assert.throws(() => createTag({ git: f.runner, pushGit: f.runner, version: "0.9.0", commit: f.commit }), /pushing tag 0.9.0 failed/);
    assert.equal(f.git(["tag", "-l"]), "");
    assert.match(f.git(["ls-remote", "--tags", f.origin, "refs/tags/0.9.0"]), new RegExp(`^${f.first}\\s`));
  } finally { f.cleanup(); }
});

test("the push credential travels through git's environment, never an argument", () => {
  const env = pushCredential("s3cret-token");
  assert.equal(env.GIT_CONFIG_COUNT, "1");
  assert.equal(env.GIT_CONFIG_KEY_0, "http.https://github.com/.extraheader");
  assert.equal(env.GIT_CONFIG_VALUE_0, `AUTHORIZATION: basic ${Buffer.from("x-access-token:s3cret-token").toString("base64")}`);
  for (const [, args] of tagCommands("0.9.0", COMMIT)) assert.ok(!args.join(" ").includes("s3cret-token"));
});

// ---- the workflow ------------------------------------------------------------------------------

test("publish-swift.yml satisfies its contract and every weakening is rejected", () => {
  const doc = parseYaml(fs.readFileSync(new URL("../../.github/workflows/publish-swift.yml", import.meta.url), "utf8"));
  assert.equal(assertSwiftDoc(doc), true);
  assert.deepEqual(proveSwiftMutations(doc), []);
  assert.ok(SWIFT_MUTATIONS.length >= 40);
  const names = SWIFT_MUTATIONS.map(([name]) => name);
  for (const required of ["verify gains contents write", "tag runs for a dry run", "dry run defaults to false", "publications stop sharing one lock", "checkout follows an input", "dispatch value in shell source", "repository secret in the tag step", "extra step in the write job", "preflight missing", "qualify missing", "qualify moved into the tag job", "verify gains the release environment", "verify gains a job secret"]) assert.ok(names.includes(required), required);
});

test("contents: write appears only in the tag job, and the push token is the job token", () => {
  const text = fs.readFileSync(new URL("../../.github/workflows/publish-swift.yml", import.meta.url), "utf8");
  assert.equal(text.match(/contents: write/g).length, 1);
  assert.ok(text.indexOf("contents: write") > text.indexOf("\n  tag:"));
  assert.ok(!/secrets\./.test(text) && !/git push/.test(text.replace(/^#.*$/gm, "")));
});
