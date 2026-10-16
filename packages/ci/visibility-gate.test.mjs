import test from "node:test";
import assert from "node:assert/strict";
import { assertPublic, resolveVisibility, parseOwnerRepo } from "./policy/visibility-gate.mjs";

function ghExecFile(visibility) {
  return async (cmd, cmdArgs) => {
    if (cmd === "gh") return { stdout: JSON.stringify({ visibility }) };
    throw new Error(`unexpected exec: ${cmd} ${cmdArgs.join(" ")}`);
  };
}

function failingGhExecFile(remoteUrl) {
  return async (cmd, cmdArgs) => {
    if (cmd === "gh") throw new Error("gh: command not found");
    if (cmd === "git" && cmdArgs[0] === "remote") return { stdout: `${remoteUrl}\n` };
    throw new Error(`unexpected exec: ${cmd} ${cmdArgs.join(" ")}`);
  };
}

test("assertPublic allows a repo gh reports as public", async () => {
  const result = await assertPublic("/repo", { execFile: ghExecFile("PUBLIC") });
  assert.equal(result.allowed, true);
  assert.equal(result.visibility, "public");
});

test("assertPublic refuses a repo gh reports as private — no override", async () => {
  const result = await assertPublic("/repo", { execFile: ghExecFile("PRIVATE") });
  assert.equal(result.allowed, false);
  assert.equal(result.visibility, "private");
  assert.match(result.reason, /public repos only/);
});

test("assertPublic refuses a repo gh reports as internal", async () => {
  const result = await assertPublic("/repo", { execFile: ghExecFile("INTERNAL") });
  assert.equal(result.allowed, false);
  assert.equal(result.visibility, "internal");
});

test("assertPublic falls back to git-remote + API when gh is unavailable, and allows a confirmed-public repo", async () => {
  const execFile = failingGhExecFile("https://github.com/acme/widgets.git");
  const fetchFn = async () => ({ ok: true, json: async () => ({ private: false }) });
  const result = await assertPublic("/repo", { execFile, fetch: fetchFn });
  assert.equal(result.allowed, true);
  assert.equal(result.source, "git-remote-api");
});

test("assertPublic fails closed to 'unknown' when neither gh nor the API can confirm public", async () => {
  const execFile = failingGhExecFile("https://github.com/acme/widgets.git");
  const fetchFn = async () => ({ ok: false, status: 404 });
  const result = await assertPublic("/repo", { execFile, fetch: fetchFn });
  assert.equal(result.allowed, false);
  assert.equal(result.visibility, "unknown");
  assert.match(result.reason, /fail closed/);
});

test("assertPublic never throws even when every network path errors", async () => {
  const execFile = async () => {
    throw new Error("no network");
  };
  const result = await assertPublic("/repo", { execFile, fetch: async () => { throw new Error("no network"); } });
  assert.equal(result.allowed, false);
  assert.equal(result.visibility, "unknown");
});

test("resolveVisibility never throws and returns a typed result on total failure", async () => {
  const execFile = async () => {
    throw new Error("boom");
  };
  const result = await resolveVisibility("/repo", { execFile });
  assert.equal(result.source, "error");
  assert.equal(result.visibility, "unknown");
  assert.ok(typeof result.error === "string");
});

test("parseOwnerRepo handles https and ssh remote forms", () => {
  assert.deepEqual(parseOwnerRepo("https://github.com/acme/widgets.git"), { owner: "acme", repo: "widgets" });
  assert.deepEqual(parseOwnerRepo("git@github.com:acme/widgets.git"), { owner: "acme", repo: "widgets" });
  assert.equal(parseOwnerRepo("not-a-github-url"), null);
});

// F16 regression: the old repo-name group excluded '.' from its character
// class, so it could never reach the end-of-string anchor for a dotted repo
// name — the whole match failed and a legitimate public repo could not be
// identified through the gh-unavailable fallback. Reproduces the QA report's
// exact probe input.
test("F16: parseOwnerRepo identifies a valid GitHub repo whose name contains dots", () => {
  assert.deepEqual(parseOwnerRepo("https://github.com/owner/repo.with.dot.git"), {
    owner: "owner",
    repo: "repo.with.dot",
  });
  assert.deepEqual(parseOwnerRepo("git@github.com:owner/repo.with.dot.git"), {
    owner: "owner",
    repo: "repo.with.dot",
  });
});

// F16 non-regression: an unconfigured enterprise GitHub host must still fail
// closed to null (the unauthenticated-API fallback only ever targets
// api.github.com, so this parse never reaches an enterprise instance).
test("F16: parseOwnerRepo still fails closed for an unconfigured enterprise GitHub host", () => {
  assert.equal(parseOwnerRepo("https://github.example.com/owner/repo.git"), null);
});

// Canonicalization hygiene regression: uppercase host, embedded userinfo
// (credentials), and a non-standard port are all *genuinely* github.com —
// DNS is case-insensitive, userinfo is just embedded credentials, and the
// API query ignores the port. These are not lookalike hosts, so they must
// still resolve to the real owner/repo rather than fail closed. The WHATWG
// URL parser already lowercases hostnames and separates userinfo/port from
// host — parseOwnerRepo's `url.hostname` comparison relies on exactly that,
// so this locks the behavior in against any future regression that starts
// comparing against the raw authority string instead of `url.hostname`.
test("canonicalization: parseOwnerRepo accepts uppercase github.com host", () => {
  assert.deepEqual(parseOwnerRepo("https://GITHUB.COM/owner/repo"), { owner: "owner", repo: "repo" });
  assert.deepEqual(parseOwnerRepo("https://GitHub.com/owner/repo.git"), { owner: "owner", repo: "repo" });
});

test("canonicalization: parseOwnerRepo accepts a github.com remote with embedded userinfo", () => {
  assert.deepEqual(parseOwnerRepo("https://attacker@github.com/victim/repo"), {
    owner: "victim",
    repo: "repo",
  });
});

test("canonicalization: parseOwnerRepo accepts a github.com remote on a non-standard port", () => {
  assert.deepEqual(parseOwnerRepo("https://github.com:8443/owner/repo"), { owner: "owner", repo: "repo" });
});

// Lookalike-family non-regression (N1): every one of these has a host that
// is NOT github.com — they must keep failing closed to null. Re-asserted
// here alongside the canonicalization-hygiene cases above so the two
// concerns (accept real github.com in any authority shape, reject anything
// that merely contains the substring "github.com") are tested side by side.
test("N1 lookalikes: parseOwnerRepo still rejects every non-github.com host", () => {
  assert.equal(parseOwnerRepo("https://notgithub.com/owner/repo"), null);
  assert.equal(parseOwnerRepo("https://github.com.evil.io/owner/repo"), null);
  assert.equal(parseOwnerRepo("https://mygithub.com/owner/repo"), null);
  assert.equal(parseOwnerRepo("https://github.com@evil.io/owner/repo"), null);
  assert.equal(parseOwnerRepo("https://github.como/owner/repo"), null);
  assert.equal(parseOwnerRepo("https://github.example.com/owner/repo"), null);
});
