import assert from "node:assert/strict";
import test from "node:test";
import { parse as parseToml } from "smol-toml";
import { comparePyVersions, parsePyVersion, parseRequirements, poetrySpec, pythonLockProblems, satisfies, parseSpecifier } from "./python-lock.mjs";

const run = (files, dir = ".") => pythonLockProblems({ dir, names: Object.keys(files), read: (f) => files[f.replace(/^.*\//, "")], parseToml });
const UV = (pkgs) => pkgs.map(([n, v]) => `[[package]]\nname = "${n}"\nversion = "${v}"\n`).join("\n");

test("pins file: every entry must be an exact == pin", () => {
  assert.deepEqual(run({ "requirements.pins.txt": "# c\ntorch==2.4.0 --hash=sha256:ab\nnumpy==2.0.2\n--extra-index-url https://x/y\n" }), []);
  const out = run({ "requirements.pins.txt": "torch==2.4.0\nnumpy>=2.0.2\nsentencepiece~=0.1.98\n" }, "tools");
  assert.equal(out.length, 2);
  assert.match(out[0], /^tools\/requirements\.pins\.txt:2: numpy >=2\.0\.2 is not an exact == pin in a lock file/);
});

test("pins file against the requirements it pins: disagreement and missing pin", () => {
  const files = { "requirements.vast.in": "numpy>=2.0\ntorch==2.4.0\nscipy\n", "requirements.vast.pins.txt": "numpy==1.26.4\ntorch==2.3.0\n" };
  const out = run(files);
  assert.equal(out.length, 3, out.join("\n"));
  assert.match(out.join("\n"), /requirements\.vast\.in:1: numpy >=2\.0 is not satisfied by requirements\.vast\.pins\.txt, which has 1\.26\.4/);
  assert.match(out.join("\n"), /requirements\.vast\.in:2: torch ==2\.4\.0 is not satisfied by requirements\.vast\.pins\.txt, which has 2\.3\.0/);
  assert.match(out.join("\n"), /requirements\.vast\.in:3: scipy has no pin in requirements\.vast\.pins\.txt/);
});

test("pip-tools pair: requirements.in compiled to requirements.txt", () => {
  assert.deepEqual(run({ "requirements.in": "Flask>=2.0,<3\nrequests[socks]\n", "requirements.txt": "flask==2.3.3\nrequests==2.32.0\nwerkzeug==3.0.0\n" }), []);
  const out = run({ "requirements.in": "flask>=3\n", "requirements.txt": "flask==2.3.3\nwerkzeug>=3\n" });
  assert.equal(out.length, 2);
  assert.match(out[0], /werkzeug >=3 is not an exact == pin/);
  assert.match(out[1], /flask >=3 is not satisfied by requirements\.txt, which has 2\.3\.3/);
});

test("requirements.txt beside uv.lock: entries must agree with the lock; alone, unpinned entries are the declaration", () => {
  assert.deepEqual(run({ "requirements.txt": "numpy>=2.0\n" }), []);
  const files = { "requirements.txt": "numpy>=2.0\nrich==13.0.0\nmissing\n", "uv.lock": UV([["numpy", "1.26.4"], ["rich", "13.0.0"]]) };
  const out = run(files);
  assert.equal(out.length, 2, out.join("\n"));
  assert.match(out[0], /numpy >=2\.0 is not satisfied by uv\.lock, which has 1\.26\.4/);
  assert.match(out[1], /missing has no locked version in uv\.lock/);
});

test("pyproject.toml against uv.lock and poetry.lock", () => {
  const py = '[project]\nname = "app"\ndependencies = ["httpx>=0.27,<1", "pydantic~=2.7", "anyio; python_version < \'3.8\'"]\n[project.optional-dependencies]\ndev = ["pytest>=8"]\n[dependency-groups]\nlint = ["ruff==0.5.0"]\n';
  const ok = run({ "pyproject.toml": py, "uv.lock": UV([["httpx", "0.27.2"], ["pydantic", "2.9.1"], ["pytest", "8.3.0"], ["ruff", "0.5.0"], ["app", "0.1.0"]]) });
  assert.deepEqual(ok, []);
  const bad = run({ "pyproject.toml": py, "uv.lock": UV([["httpx", "0.26.0"], ["pydantic", "3.0.0"], ["ruff", "0.5.1"]]) });
  assert.equal(bad.length, 4, bad.join("\n"));
  assert.match(bad.join("\n"), /\[project\]\.dependencies: httpx >=0\.27,<1 is not satisfied by uv\.lock, which has 0\.26\.0/);
  assert.match(bad.join("\n"), /pytest >=8 has no locked version in uv\.lock/);
  const poetry = '[tool.poetry.dependencies]\npython = "^3.11"\nrequests = "^2.31"\nflask = { version = "~2.3", extras = ["async"] }\n';
  assert.deepEqual(run({ "pyproject.toml": poetry, "poetry.lock": UV([["requests", "2.32.3"], ["flask", "2.3.9"]]) }), []);
  assert.match(run({ "pyproject.toml": poetry, "poetry.lock": UV([["requests", "3.0.0"], ["flask", "2.3.9"]]) })[0], /requests \^2\.31 is not satisfied by poetry\.lock/);
});

test("unreadable requirement lines are findings", () => {
  const out = run({ "requirements.pins.txt": "pkg @ https://x/y.whl\n-e ./local\nweird===???\n" });
  assert.equal(out.length, 3, out.join("\n"));
  assert.match(out[0], /requirements\.pins\.txt:1: requirement pkg @ https/);
});

test("pyproject without a lock is not checked; bad TOML is a finding", () => {
  assert.deepEqual(run({ "pyproject.toml": '[project]\ndependencies = ["a>=1"]\n' }), []);
  assert.match(run({ "pyproject.toml": "[project", "uv.lock": UV([]) })[0], /pyproject\.toml: cannot be parsed as TOML/);
});

test("version helpers", () => {
  assert.ok(comparePyVersions(parsePyVersion("1.0.0"), parsePyVersion("1.0rc1")) > 0);
  assert.ok(comparePyVersions(parsePyVersion("1.0.post1"), parsePyVersion("1.0")) > 0);
  assert.ok(satisfies(parsePyVersion("2.7.3"), parseSpecifier("~=2.7")) && !satisfies(parsePyVersion("3.0"), parseSpecifier("~=2.7")));
  assert.ok(satisfies(parsePyVersion("1.2.9"), parseSpecifier("==1.2.*")) && !satisfies(parsePyVersion("1.3.0"), parseSpecifier("==1.2.*")));
  assert.equal(parseRequirements("a==1\\\n  --hash=sha256:x\n").reqs[0].pinned, true);
  assert.equal(poetrySpec("^0.2.3").length, 2);
  assert.equal(poetrySpec("1 || 2"), null);
});
