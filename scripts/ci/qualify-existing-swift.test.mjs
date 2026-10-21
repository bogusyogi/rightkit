import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { validateInputs, validateRunContext, validateTagResolution, validateConsumerResolution, officialHeaders, PACKAGE_URL, WORKFLOW_REF } from "./qualify-existing-swift.mjs";

const commit = "a".repeat(40);
const tagSha = "b".repeat(40);
const other = "c".repeat(40);
const tag = "0.8.1";
const inputs = () => ({ SWIFT_QUALIFY_TAG: tag, SWIFT_QUALIFY_COMMIT: commit, SWIFT_QUALIFY_ARCHIVE_SHA256: "d".repeat(64), RUNNER_TEMP: path.resolve("scratch") });
const context = () => ({ GITHUB_REPOSITORY: "bogusyogi/rightkit", GITHUB_EVENT_NAME: "workflow_dispatch", GITHUB_REF: "refs/heads/main", GITHUB_WORKFLOW_REF: "bogusyogi/rightkit/.github/workflows/qualify-existing-swift.yml@refs/heads/main", RUNNER_OS: "macOS", GITHUB_RUN_ID: "123", GITHUB_RUN_ATTEMPT: "2", GITHUB_SHA: other });
const event = () => ({ repository: { full_name: "bogusyogi/rightkit", private: false } });
const ref = (type = "commit", sha = commit) => ({ ref: `refs/tags/${tag}`, object: { type, sha } });
const resolved = () => ({ version: 3, pins: [{ identity: "rightkit", kind: "remoteSourceControl", location: PACKAGE_URL, state: { version: tag, revision: commit } }] });

test("qualification inputs accept immutable plain semver identity without consulting VERSION", () => {
  assert.deepEqual(validateInputs(inputs()), { tag, commit, archiveSha256: "d".repeat(64), runnerTemp: path.resolve("scratch") });
  for (const bad of ["v0.8.1", "0.8.1-beta.1", "0.8.1+meta", "01.8.1", "0.8", "0.8.1\n", "../0.8.1", "0.8.1;echo nope"]) assert.throws(() => validateInputs({ ...inputs(), SWIFT_QUALIFY_TAG: bad }));
  for (const [key, value] of [["SWIFT_QUALIFY_COMMIT", "main"], ["SWIFT_QUALIFY_COMMIT", commit.slice(1)], ["SWIFT_QUALIFY_ARCHIVE_SHA256", "d".repeat(63)], ["RUNNER_TEMP", "relative"]]) assert.throws(() => validateInputs({ ...inputs(), [key]: value }));
});

test("official API calls require & carry job read token; archive bytes never carry it", () => {
  assert.equal(WORKFLOW_REF, "bogusyogi/rightkit/.github/workflows/qualify-existing-swift.yml@refs/heads/main");
  assert.equal(officialHeaders(true, "read-fixture").Authorization, "Bearer read-fixture");
  for (const token of [undefined, "", 42]) assert.throws(() => officialHeaders(true, token), /GITHUB_TOKEN read authentication required/);
  assert.ok(!("Authorization" in officialHeaders(false, "read-fixture")));
  assert.ok(!("Authorization" in officialHeaders(false)));
});

test("only public canonical main dispatch on macOS carries Actions identity", () => {
  assert.equal(validateRunContext(context(), event()).url, `${PACKAGE_URL}/actions/runs/123/attempts/2`);
  for (const patch of [{ GITHUB_REPOSITORY: "fork/rightkit" }, { GITHUB_EVENT_NAME: "push" }, { GITHUB_REF: "refs/tags/0.8.1" }, { GITHUB_WORKFLOW_REF: "bogusyogi/rightkit/.github/workflows/publish-swift.yml@refs/heads/main" }, { GITHUB_WORKFLOW_REF: undefined }, { RUNNER_OS: "Linux" }, { GITHUB_RUN_ID: "" }, { GITHUB_RUN_ATTEMPT: "0" }, { GITHUB_SHA: "main" }]) assert.throws(() => validateRunContext({ ...context(), ...patch }, event()));
  for (const repository of [{ full_name: "bogusyogi/rightkit", private: true }, { full_name: "fork/rightkit", private: false }, { full_name: "bogusyogi/rightkit" }]) assert.throws(() => validateRunContext(context(), { repository }));
});

test("lightweight & annotated tags resolve only exact expected commit", () => {
  assert.deepEqual(validateTagResolution(tag, commit, ref()), { refSha: commit, commit, annotatedObjects: [] });
  const chain = [{ sha: tagSha, object: { type: "tag", sha: other } }, { sha: other, object: { type: "commit", sha: commit } }];
  assert.deepEqual(validateTagResolution(tag, commit, ref("tag", tagSha), chain), { refSha: tagSha, commit, annotatedObjects: [tagSha, other] });
  assert.throws(() => validateTagResolution(tag, other, ref()));
  assert.throws(() => validateTagResolution(tag, commit, { ...ref(), ref: "refs/tags/0.8.2" }));
  assert.throws(() => validateTagResolution(tag, commit, ref("tree")));
  assert.throws(() => validateTagResolution(tag, commit, ref(), chain));
  assert.throws(() => validateTagResolution(tag, commit, ref("tag", tagSha), []));
  assert.throws(() => validateTagResolution(tag, commit, ref("tag", tagSha), [{ sha: other, object: { type: "commit", sha: commit } }]));
});

test("annotation cycles & chains beyond bounded dereference limit fail closed", () => {
  assert.throws(() => validateTagResolution(tag, commit, ref("tag", tagSha), [{ sha: tagSha, object: { type: "tag", sha: tagSha } }]));
  const shas = Array.from({ length: 9 }, (_, i) => (i + 1).toString(16).repeat(40));
  const chain = shas.map((sha, i) => ({ sha, object: i === 8 ? { type: "commit", sha: commit } : { type: "tag", sha: shas[i + 1] } }));
  assert.throws(() => validateTagResolution(tag, commit, ref("tag", shas[0]), chain));
});

test("remote consumer proves canonical identity, exact tag & exact immutable revision together", () => {
  for (const version of [2, 3]) assert.deepEqual(validateConsumerResolution({ ...resolved(), version }, tag, commit), { identity: "rightkit", location: PACKAGE_URL, version: tag, revision: commit });
  for (const patch of [{ identity: "RightKitSwift" }, { kind: "localSourceControl" }, { location: `${PACKAGE_URL}.git` }, { location: "https://github.com/fork/rightkit" }, { state: { version: tag, revision: other } }, { state: { revision: commit } }, { state: { version: tag, revision: commit, branch: "main" } }]) {
    const document = resolved();
    Object.assign(document.pins[0], patch);
    assert.throws(() => validateConsumerResolution(document, tag, commit));
  }
  assert.throws(() => validateConsumerResolution({ version: 3, pins: [] }, tag, commit));
  assert.throws(() => validateConsumerResolution({ version: 3, pins: [...resolved().pins, ...resolved().pins] }, tag, commit));
  assert.throws(() => validateConsumerResolution({ ...resolved(), version: 1 }, tag, commit));
});
