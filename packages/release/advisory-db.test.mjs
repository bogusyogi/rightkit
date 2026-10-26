import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { advisoryCacheRoot, ensureAdvisoryDb, prepareDenyOverlay, readAdvisoryPin } from "./advisory-db.mjs";

const COMMIT = "a".repeat(40);
const PIN = { url: "https://github.com/rustsec/advisory-db", commit: COMMIT };
const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), "rk-advdb-test-"));

// A fake git: records calls; `fetch` writes a marker so the temp dir is a recognisable checkout; rev-parse answers `head`.
function fakeGit({ head = COMMIT, failStep = null, calls = [] } = {}) {
  const git = (args, { cwd }) => {
    calls.push({ args, cwd });
    if (args[0] === failStep) return { status: 128, stdout: "", stderr: "fatal: unable to access (503)" };
    if (args[0] === "checkout") fs.writeFileSync(path.join(cwd, "marker"), "x");
    if (args[0] === "rev-parse") return fs.existsSync(path.join(cwd, "marker")) ? { status: 0, stdout: `${head}\n`, stderr: "" } : { status: 128, stdout: "", stderr: "not a repo" };
    return { status: 0, stdout: "", stderr: "" };
  };
  return git;
}

test("pin file: the committed advisory-db.json is a url and a 40-hex commit", () => {
  const pin = readAdvisoryPin();
  assert.equal(pin.url, "https://github.com/rustsec/advisory-db");
  assert.match(pin.commit, /^[0-9a-f]{40}$/);
  assert.throws(() => readAdvisoryPin({ readFile: () => '{"url":"https://x","commit":"main"}' }), /40 lowercase hex/);
  assert.throws(() => readAdvisoryPin({ readFile: () => { throw new Error("ENOENT"); } }), /unreadable/);
});

test("ensureAdvisoryDb: materialises exactly the pinned commit at <cacheRoot>/advisory-db/<commit> by shallow fetch", () => {
  const cacheRoot = tmp(); const calls = [];
  const dir = ensureAdvisoryDb({ cacheRoot, pin: PIN, git: fakeGit({ calls }) });
  assert.equal(dir, path.join(cacheRoot, "advisory-db", COMMIT));
  assert.ok(fs.existsSync(path.join(dir, "marker")));
  assert.deepEqual(calls.map((c) => c.args.slice(0, 5)).filter((a) => a[0] !== "rev-parse"), [["init", "-q"], ["remote", "add", "origin", PIN.url], ["fetch", "-q", "--depth", "1", "origin"], ["checkout", "-q", "FETCH_HEAD"]]);
  assert.equal(calls.find((c) => c.args[0] === "fetch").args.at(-1), COMMIT);
  assert.deepEqual(fs.readdirSync(path.join(cacheRoot, "advisory-db")), [COMMIT], "no temp directory left behind");
});

test("ensureAdvisoryDb: a present checkout whose HEAD equals the pin is reused without fetching", () => {
  const cacheRoot = tmp();
  ensureAdvisoryDb({ cacheRoot, pin: PIN, git: fakeGit() });
  const calls = [];
  ensureAdvisoryDb({ cacheRoot, pin: PIN, git: fakeGit({ calls }) });
  assert.deepEqual(calls.map((c) => c.args[0]), ["rev-parse"]);
});

test("ensureAdvisoryDb: a present checkout at another commit is replaced, never trusted", () => {
  const cacheRoot = tmp(); const dest = path.join(cacheRoot, "advisory-db", COMMIT);
  fs.mkdirSync(dest, { recursive: true }); fs.writeFileSync(path.join(dest, "stale"), "x");
  const calls = [];
  const got = ensureAdvisoryDb({ cacheRoot, pin: PIN, git: (args, o) => (args[0] === "rev-parse" && fs.existsSync(path.join(o.cwd, "stale")) ? { status: 0, stdout: `${"c".repeat(40)}\n`, stderr: "" } : fakeGit({ calls })(args, o)) });
  assert.equal(got, dest);
  assert.ok(!fs.existsSync(path.join(dest, "stale")));
  assert.ok(fs.existsSync(path.join(dest, "marker")));
});

test("ensureAdvisoryDb: refuses with the reason when git cannot fetch, and leaves nothing behind", () => {
  const cacheRoot = tmp();
  assert.throws(() => ensureAdvisoryDb({ cacheRoot, pin: PIN, git: fakeGit({ failStep: "fetch" }) }), /not available: git fetch failed \(fatal: unable to access \(503\)\).*refuses/s);
  assert.deepEqual(fs.readdirSync(path.join(cacheRoot, "advisory-db")), []);
});

test("ensureAdvisoryDb: refuses a fetch that lands on another commit, a bad pin and a relative cache root", () => {
  const cacheRoot = tmp();
  assert.throws(() => ensureAdvisoryDb({ cacheRoot, pin: PIN, git: fakeGit({ head: "d".repeat(40) }) }), /not the pinned/);
  assert.ok(!fs.existsSync(path.join(cacheRoot, "advisory-db", COMMIT)));
  assert.throws(() => ensureAdvisoryDb({ cacheRoot, pin: { url: PIN.url, commit: "HEAD" }, git: fakeGit() }), /40 lowercase hex/);
  assert.throws(() => ensureAdvisoryDb({ cacheRoot: "relative", pin: PIN, git: fakeGit() }), /absolute/);
});

test("advisoryCacheRoot: override, Windows LOCALAPPDATA, else ~/.cache/rightkit (XDG_CACHE_HOME is never read)", () => {
  assert.equal(advisoryCacheRoot({ env: { RK_ADVISORY_CACHE_ROOT: "/x/y" }, platform: "linux", home: "/h" }), path.resolve("/x/y"));
  assert.throws(() => advisoryCacheRoot({ env: { RK_ADVISORY_CACHE_ROOT: "rel" }, platform: "linux", home: "/h" }), /absolute/);
  assert.equal(advisoryCacheRoot({ env: { XDG_CACHE_HOME: "/xdg" }, platform: "linux", home: "/h" }), path.join("/h", ".cache", "rightkit"));
  assert.equal(advisoryCacheRoot({ env: { LOCALAPPDATA: "C:\\Users\\a\\AppData\\Local" }, platform: "win32", home: "C:\\Users\\a" }), "C:\\Users\\a\\AppData\\Local\\RightKit");
});

test("prepareDenyOverlay: db-path points at a directory holding the pinned checkout under cargo-deny's name; the rest of deny.toml is kept", () => {
  const db = tmp();
  const toml = '[graph]\nall-features = true\n\n[advisories]\n# note\ndb-path = "/elsewhere"\nversion = 2\nignore = []\n\n[licenses]\nallow = ["MIT"]\n';
  const { configPath, cleanup } = prepareDenyOverlay({ denyToml: toml, dbDir: db, pin: PIN });
  try {
    const text = fs.readFileSync(configPath, "utf8");
    assert.equal(text.match(/db-path/g).length, 1, "the repository's own db-path is replaced");
    assert.ok(text.includes("version = 2") && text.includes('allow = ["MIT"]') && text.includes("all-features = true"));
    const dbPath = JSON.parse(/db-path = (".*")/.exec(text)[1]);
    assert.equal(fs.realpathSync(path.join(dbPath, "advisory-db-3157b0e258782691")), fs.realpathSync(db));
  } finally { cleanup(); }
  assert.ok(!fs.existsSync(configPath));
  const none = prepareDenyOverlay({ denyToml: "", dbDir: db, pin: PIN });
  try { assert.match(fs.readFileSync(none.configPath, "utf8"), /\[advisories\]\ndb-path = /); } finally { none.cleanup(); }
  assert.throws(() => prepareDenyOverlay({ denyToml: "", dbDir: db, pin: { url: "https://example.com/db", commit: COMMIT } }), /not known/);
});
