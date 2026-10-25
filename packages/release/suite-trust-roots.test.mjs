import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { RIGHT_SUITE_LICENSE_PUBLIC_KEY_HEX, RIGHT_SUITE_UPDATER_PUBLIC_KEYS } from "./suite-api.generated.mjs";
import { RULES, main, runRules } from "./app-check.mjs";
import { execFileSync } from "./background-process.mjs";
import { RULES as PREFLIGHT_RULES } from "../../scripts/preflight.mjs";
import { engineRecordStore, licenceKeyPatterns, suiteKeyLiteral, suiteUpdaterKey, updaterKeyPattern } from "./suite-trust-roots.mjs";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const git = (cwd, ...args) => execFileSync("git", args, { cwd, encoding: "utf8" });

/** A committed temporary repository, so the rules read what `git ls-files` lists the way a preflight run does. */
function repo(t, files, { name = "AppRepo" } = {}) {
  const parent = fs.mkdtempSync(path.join(os.tmpdir(), "rk-suite-roots-"));
  t.after(() => fs.rmSync(parent, { recursive: true, force: true }));
  const root = path.join(parent, name); // the baseline key is the lowercased basename
  fs.mkdirSync(root);
  git(root, "init", "-q");
  for (const [k, v] of [["user.email", "t@example.test"], ["user.name", "t"], ["core.autocrlf", "false"]]) git(root, "config", k, v);
  for (const [file, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
    fs.writeFileSync(path.join(root, file), content);
  }
  git(root, "add", "-A");
  git(root, "commit", "-q", "-m", "fixture");
  return { root, parent, files: git(root, "ls-files", "-z").split("\0").filter(Boolean) };
}

const HEX = RIGHT_SUITE_LICENSE_PUBLIC_KEY_HEX;
const KEY = Buffer.from(HEX, "hex");
const BYTES = [...KEY];
const chunk = (items, size) => Array.from({ length: Math.ceil(items.length / size) }, (_, i) => items.slice(i * size, (i + 1) * size));
const wrapped = (items, eol = "\n") => chunk(items, 8).map((row) => `    ${row.join(", ")}`).join(`,${eol}`);
const hexByte = (b) => `0x${b.toString(16).padStart(2, "0")}`;
const flip = (text, at) => `${text.slice(0, at)}${text[at] === "A" ? "B" : "A"}${text.slice(at + 1)}`;
const other = (b) => (b + 1) % 256;

const found = (findings) => findings.map((f) => `${f.file}|${f.subject}`).sort();
const KEY_SOURCES = /rightkit_license::suite_api::LICENSE_PUBLIC_KEY.*@rightkit\/app-update.*RightSuiteAPI \(Swift\)/;

test("suite-key-literal finds the licence key in every spelling an app pastes it, and says where to take it from", (t) => {
  const { root, files } = repo(t, {
    "src/decimal.rs": `// licence key\n\nconst KEY: [u8; 32] = [${BYTES.join(", ")}];\n`,
    "src/compact.rs": `const KEY: [u8; 32] = [${BYTES.join(",")}];\n`,
    "src/wrapped.rs": `const KEY: [u8; 32] = [\n${wrapped(BYTES.map((b) => `${b}_u8`))},\n];\n`,
    "src/suffixed.rs": `const KEY: [u8; 32] = [${BYTES.map((b, i) => `${b}${i % 2 ? "u8" : ""}`).join(", ")}];\n`,
    "src/crlf.ts": `const key = [\r\n${wrapped(BYTES, "\r\n")},\r\n];\n`,
    "src/prefixed.rs": `const KEY: [u8; 32] = [${BYTES.map(hexByte).join(", ")}];\n`,
    "src/upper.swift": `let key: [UInt8] = [${BYTES.map((b) => `0x${hexByte(b).slice(2).toUpperCase()}`).join(", ")}]\n`,
    "src/short.go": `var key = []byte{${BYTES.map((b) => `0x${b.toString(16)}`).join(", ")}}\n`,
    "src/typed.rs": `const KEY: [u8; 32] = [\n${wrapped(BYTES.map((b) => `${hexByte(b)}_u8`))},\n];\n`,
    "src/lower.ts": `export const KEY = "${HEX}";\n`,
    "src/upper.ts": `export const KEY = "${HEX.toUpperCase()}";\n`,
    "src/url.swift": `let key = "${KEY.toString("base64url")}"\n`,
    "src/padded.ts": `const key = "${KEY.toString("base64")}";\n`,
    "src/pem.txt": `${Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), KEY]).toString("base64")}\n`,
  });
  assert.deepEqual(found(suiteKeyLiteral(root, files)), [
    "src/compact.rs|licence-key:bytes",
    "src/crlf.ts|licence-key:bytes",
    "src/decimal.rs|licence-key:bytes",
    "src/lower.ts|licence-key:hex",
    "src/padded.ts|licence-key:base64",
    "src/pem.txt|licence-key:base64",
    "src/prefixed.rs|licence-key:bytes",
    "src/short.go|licence-key:bytes",
    "src/suffixed.rs|licence-key:bytes",
    "src/typed.rs|licence-key:bytes",
    "src/upper.swift|licence-key:bytes",
    "src/upper.ts|licence-key:hex",
    "src/url.swift|licence-key:base64url",
    "src/wrapped.rs|licence-key:bytes",
  ]);
  for (const f of suiteKeyLiteral(root, files)) assert.match(f.fix, KEY_SOURCES, f.file);
});

test("suite-key-literal sees a pasted array with a comment on every element, and \\x escapes", (t) => {
  const commented = `const KEY: [u8; 32] = [\n${BYTES.map((b, i) => `    ${hexByte(b)}, // byte ${i}`).join("\n")}\n];\n`;
  const blocked = `const KEY = [${BYTES.map((b, i) => `${b} /* ${i} */`).join(", ")}];\n`;
  const escaped = `const KEY = "${BYTES.map((b) => `\\x${b.toString(16).padStart(2, "0")}`).join("")}";\n`;
  const { root, files } = repo(t, { "src/commented.rs": commented, "src/blocked.ts": blocked, "src/escaped.go": escaped });
  assert.deepEqual(found(suiteKeyLiteral(root, files)), ["src/blocked.ts|licence-key:bytes", "src/commented.rs|licence-key:bytes", "src/escaped.go|licence-key:escaped"]);
});

test("suite-key-literal reports one finding per file and spelling, with the lines in the problem text only", (t) => {
  const { root, files } = repo(t, {
    "src/decimal.rs": `// licence key\n\nconst KEY: [u8; 32] = [${BYTES.join(", ")}];\n`,
    "src/wrapped.rs": `// licence key\nconst KEY: [u8; 32] = [\n${wrapped(BYTES)},\n];\n`,
    "src/both.rs": `const A: [u8; 32] = [${BYTES.join(", ")}];\n\nconst B: [u8; 32] = [${BYTES.map(hexByte).join(", ")}];\n`,
    "src/many.ts": Array.from({ length: 9 }, () => `"${HEX}"`).join("\n"),
    "src/twice.ts": `const a = "${HEX}", b = "${HEX}";\n`,
    "src/mixed.ts": `const hex = "${HEX}";\nconst url = "${KEY.toString("base64url")}";\n`,
  });
  const findings = suiteKeyLiteral(root, files);
  const problem = (file, subject) => findings.find((f) => f.file === file && f.subject === subject).problem;
  assert.equal(findings.length, 7, found(findings).join("\n"));
  assert.match(problem("src/decimal.rs", "licence-key:bytes"), /^src\/decimal\.rs:3 carries the suite licence public key as a byte array$/);
  assert.match(problem("src/wrapped.rs", "licence-key:bytes"), /^src\/wrapped\.rs:3 /, "a wrapped array reports the line of its first element");
  assert.match(problem("src/both.rs", "licence-key:bytes"), /^src\/both\.rs:1,3 /, "decimal and prefixed arrays are one spelling");
  assert.match(problem("src/twice.ts", "licence-key:hex"), /^src\/twice\.ts:1 /, "two copies on one line are one line");
  assert.match(problem("src/many.ts", "licence-key:hex"), /^src\/many\.ts:1,2,3,4,5,6,7,8,… /, "the list is capped");
  assert.match(problem("src/mixed.ts", "licence-key:hex"), /^src\/mixed\.ts:1 .* as 64 hex digits$/);
  assert.match(problem("src/mixed.ts", "licence-key:base64url"), /^src\/mixed\.ts:2 .* as unpadded base64url$/);
  // The identity the ratchet stores has no line number: an edit above the key does not make it a new finding.
  const identities = runRules({ root, files, only: ["suite-key-literal"] }).failures.map((f) => f.identity);
  assert.ok(identities.includes("suite-key-literal|src/decimal.rs|licence-key:bytes"), identities.join("\n"));
  assert.ok(identities.every((id) => !/:\d+(\||$)/.test(id)), identities.join("\n"));
});

test("suite-key-literal does not find a near miss: a changed byte, 31 bytes, a wrong digit, a glued digit", (t) => {
  const changed = (items) => items.map((b, i) => (i === 5 ? other(b) : b));
  const hexAt = 20;
  const wrongDigit = `${HEX.slice(0, hexAt)}${HEX[hexAt] === "a" ? "b" : "a"}${HEX.slice(hexAt + 1)}`;
  const url = KEY.toString("base64url");
  const { root, files } = repo(t, {
    "src/changed.rs": `const KEY: [u8; 32] = [${changed(BYTES).join(", ")}];\n`,
    "src/changed_hex.rs": `const KEY: [u8; 32] = [${changed(BYTES).map(hexByte).join(", ")}];\n`,
    "src/short.rs": `const KEY: [u8; 31] = [${BYTES.slice(0, 31).join(", ")}];\n`,
    "src/short_hex.rs": `const KEY: [u8; 31] = [${BYTES.slice(0, 31).map(hexByte).join(", ")}];\n`,
    "src/shifted.rs": `const KEY: [u8; 31] = [${BYTES.slice(1).join(", ")}];\n`,
    "src/glued_first.rs": `const KEY: [u8; 32] = [1${BYTES.join(", ")}];\n`,
    "src/glued_last.rs": `const KEY: [u8; 32] = [${BYTES.slice(0, 31).join(", ")}, ${BYTES[31]}5];\n`,
    "src/float.rs": `const KEY: [f32; 32] = [${BYTES.join(", ")}.5];\n`,
    "src/wrong_suffix.rs": `const KEY: [u8; 32] = [${BYTES.map((b) => `${b}u16`).join(", ")}];\n`,
    "src/semicolons.rs": `const KEY: [u8; 32] = [${BYTES.join("; ")}];\n`,
    "src/wrong_digit.ts": `export const KEY = "${wrongDigit}";\n`,
    "src/short_hex.ts": `export const KEY = "${HEX.slice(0, 62)}";\n`,
    "src/wrong_url.swift": `let key = "${flip(url, 10)}"\n`,
    "src/short_url.swift": `let key = "${url.slice(0, 42)}"\n`,
    "src/wrong_padded.ts": `const key = "${flip(KEY.toString("base64"), 10)}";\n`,
  });
  assert.deepEqual(suiteKeyLiteral(root, files), []);
});

test("suite-key-literal skips documentation, dependency trees, binaries and test code, and still reads look-alike paths", (t) => {
  const pasted = `export const KEY = "${HEX}";\n`;
  const skipped = [
    "docs/key.txt", "README.md", "notes/KEY.MD", "node_modules/pkg/key.js", "crates/a/target/debug/key.txt",
    "tests/key.rs", "test/key.rs", "src/__tests__/key.ts", "test-vectors/key.json", "swift/Tests/KeyTests.swift", "fixtures/key.json",
    "crates/a/Tests/key.rs", "src/key.test.ts", "src/key.spec.ts", "src/key_test.go", "src/key_tests.rs",
  ];
  const scannedPaths = ["src/key.ts", "src/docs/key.ts", "apps/x/docs/key.txt", "src/testing/key.ts", "src/contest.ts", "src/docs_ui/key.ts", "docs-site/key.ts", "src/latest_tests_data.ts", "src/mytest.ts", ".github/key.yml"];
  const { root, files } = repo(t, {
    ...Object.fromEntries([...skipped, ...scannedPaths].map((f) => [f, pasted])),
    // A binary file that happens to carry the text: an image, an archive, UTF-16 text.
    "assets/key.bin": Buffer.concat([Buffer.from(pasted), Buffer.from([0, 1, 2])]),
  });
  // .git is never tracked, so it is listed by hand.
  fs.mkdirSync(path.join(root, ".git/hooks"), { recursive: true });
  fs.writeFileSync(path.join(root, ".git/hooks/pasted"), pasted);
  assert.deepEqual(found(suiteKeyLiteral(root, [...files, ".git/hooks/pasted"])), scannedPaths.map((f) => `${f}|licence-key:hex`).sort());
});

test("suite-key-literal leaves the contract owner's own files alone only in RightKit, which is the repository with the generator", (t) => {
  const owner = [
    "contracts/right-suite-api.v1.json",
    "crates/rightkit-license/src/suite_api_generated.rs",
    "crates/rightkit-license/test-vectors/right-suite-api.v1.json",
    "packages/app-update/src/suite-api.generated.ts",
    "packages/release/suite-api.generated.mjs",
    "swift/RightKitSwift/Sources/RightKitLicense/RightSuiteAPI.generated.swift",
  ];
  const pasted = `"licensePublicKeyHex": "${HEX}"\n`;
  const rightkit = repo(t, { ...Object.fromEntries(owner.map((f) => [f, pasted])), "scripts/gen-suite-api.mjs": "// the generator\n", "src/other.rs": pasted, "crates/rightkit-license/src/other.rs": pasted });
  assert.deepEqual(found(suiteKeyLiteral(rightkit.root, rightkit.files)), ["crates/rightkit-license/src/other.rs|licence-key:hex", "src/other.rs|licence-key:hex"], "only the generated files are exempt, not the crate or the tree");
  // An app that carries a copy of one of these paths pasted the key.
  const app = repo(t, Object.fromEntries(owner.map((f) => [f, pasted])));
  assert.deepEqual(found(suiteKeyLiteral(app.root, app.files)), owner.filter((f) => !f.includes("/test-vectors/")).map((f) => `${f}|licence-key:hex`).sort());
});

test("suite-key-literal finds the updater key text outside a tauri conf, and a conf may carry the licence key's mistakes but not be exempt from them", (t) => {
  const { publicKey } = RIGHT_SUITE_UPDATER_PUBLIC_KEYS[0];
  const { root, files } = repo(t, {
    "scripts/release.mjs": `const PUBKEY = "${publicKey}";\n\n\nexport default PUBKEY;\n`,
    "src/updater.rs": `const PUBKEY: &str = "${publicKey}";\nconst AGAIN: &str = "${publicKey}";\n`,
    "src/near.rs": `const PUBKEY: &str = "${flip(publicKey, 12)}";\n`,
    "src-tauri/tauri.conf.json": JSON.stringify({ plugins: { updater: { pubkey: publicKey } } }),
    "src-tauri/tauri.macos.conf.json": JSON.stringify({ plugins: { updater: { pubkey: publicKey } } }),
    "src-tauri/tauri.dev.win.conf.json": JSON.stringify({ plugins: { updater: { pubkey: publicKey } } }),
    "crates/x/tauri.conf.json": JSON.stringify({ note: HEX, plugins: { updater: { pubkey: publicKey } } }),
    "src-tauri/not-tauri.conf.json": JSON.stringify({ pubkey: publicKey }),
    "src-tauri/tauri.json": JSON.stringify({ pubkey: publicKey }),
  });
  const findings = suiteKeyLiteral(root, files);
  assert.deepEqual(found(findings), [
    "crates/x/tauri.conf.json|licence-key:hex",
    "scripts/release.mjs|updater-key",
    "src-tauri/not-tauri.conf.json|updater-key",
    "src-tauri/tauri.json|updater-key",
    "src/updater.rs|updater-key",
  ]);
  assert.match(findings.find((f) => f.file === "scripts/release.mjs").problem, /^scripts\/release\.mjs:1 carries the suite updater public key text$/);
  assert.match(findings.find((f) => f.file === "src/updater.rs").problem, /^src\/updater\.rs:1,2 /);
  assert.match(findings.find((f) => f.subject === "updater-key").fix, /plugins\.updater\.pubkey.*Tauri's plugin config requires it.*@rightkit\/app-update/);
});

test("licenceKeyPatterns: the contract's two base64 alphabets give one finding per paste, whichever the key", () => {
  const hits = (patterns, subject, text) => [...text.matchAll(patterns.find((p) => p.subject === subject).pattern)].length;
  // Every byte 0x01 encodes with letters only, so base64 and base64url read the same unpadded; the padded text is base64 alone.
  const same = licenceKeyPatterns("01".repeat(32));
  const url = Buffer.alloc(32, 1).toString("base64url");
  assert.equal(hits(same, "licence-key:base64url", url), 1);
  assert.equal(hits(same, "licence-key:base64", url), 0);
  assert.equal(hits(same, "licence-key:base64url", `${url}=`), 0);
  assert.equal(hits(same, "licence-key:base64", `${url}=`), 1);
  // Every byte 0xfb encodes with + and / (- and _ in the URL alphabet): the spellings differ, and padding is optional for base64.
  const apart = licenceKeyPatterns("fb".repeat(32));
  const std = Buffer.alloc(32, 0xfb).toString("base64");
  const urlSafe = Buffer.alloc(32, 0xfb).toString("base64url");
  assert.match(std, /[+/]/);
  assert.equal(hits(apart, "licence-key:base64", std), 1);
  assert.equal(hits(apart, "licence-key:base64", std.replace(/=+$/, "")), 1);
  assert.equal(hits(apart, "licence-key:base64", urlSafe), 0);
  assert.equal(hits(apart, "licence-key:base64url", urlSafe), 1);
  assert.equal(hits(apart, "licence-key:base64url", std), 0);
  // 0x7 and 0x07 are one byte, and the 64 digits match in either case.
  const small = licenceKeyPatterns("07".repeat(32));
  assert.equal(hits(small, "licence-key:bytes", `[${Array(32).fill("0x7").join(", ")}]`), 1);
  assert.equal(hits(small, "licence-key:bytes", `[${Array(32).fill("0x07_u8").join(", ")}]`), 1);
  assert.equal(hits(small, "licence-key:bytes", `[${Array(32).fill("7").join(", ")}]`), 1);
  assert.equal(hits(small, "licence-key:hex", "07".repeat(32)), 1);
  assert.equal(hits(licenceKeyPatterns("ab".repeat(32)), "licence-key:hex", "AB".repeat(32)), 1);
  // A rotation that left the hex malformed must fail the rule, not scan for a shorter key.
  assert.throws(() => licenceKeyPatterns("abc"), /not 64 hex digits/);
  assert.throws(() => licenceKeyPatterns(`${"0".repeat(63)}g`), /not 64 hex digits/);
});

test("updaterKeyPattern matches each contract key as literal text and refuses a key with no text", () => {
  assert.equal(updaterKeyPattern([]), null);
  const pattern = updaterKeyPattern([{ publicKey: "a+b/c==" }, { publicKey: "d.e" }]);
  assert.equal(pattern.test("x a+b/c== y"), true);
  assert.equal(pattern.test("aab/c=="), false, "+ is not a quantifier");
  assert.equal(pattern.test("dxe"), false, ". is not a wildcard");
  assert.throws(() => updaterKeyPattern([{ publicKey: "" }]), /no text/);
  assert.throws(() => updaterKeyPattern([{ publicKey: undefined }]), /no text/);
});

const conf = (updater) => JSON.stringify({ productName: "App", plugins: { updater } }, null, 2);
const [{ publicKey: CONTRACT_KEY }] = RIGHT_SUITE_UPDATER_PUBLIC_KEYS;

test("suite-updater-key accepts a copy of every contract key and a file with nothing to check", (t) => {
  const { root, files } = repo(t, {
    ...Object.fromEntries(RIGHT_SUITE_UPDATER_PUBLIC_KEYS.map((k, i) => [`app${i}/tauri.conf.json`, conf({ endpoints: ["https://example.test/u.json"], pubkey: k.publicKey })])),
    "bom/tauri.conf.json": `﻿${conf({ pubkey: CONTRACT_KEY })}`,
    "platform/tauri.macos.conf.json": conf({ pubkey: CONTRACT_KEY }),
    "override/tauri.windows.conf.json": JSON.stringify({ bundle: { windows: {} } }),
    // a platform override that adjusts the updater and leaves the key to the base config
    "override/tauri.linux.conf.json": JSON.stringify({ plugins: { updater: { windows: { installMode: "passive" } } } }),
    "bare/tauri.conf.json": "{}",
    "shell/tauri.conf.json": JSON.stringify({ plugins: { shell: { open: true } } }),
    "null/tauri.conf.json": conf(null),
    "src/not-a-conf.json": conf("PLACEHOLDER"),
    "src/tauri.json": conf("PLACEHOLDER"),
  });
  assert.deepEqual(suiteUpdaterKey(root, files), []);
});

test("suite-updater-key refuses a placeholder, a foreign key, a non-string, a missing key and an unparseable file", (t) => {
  const foreign = Buffer.from("untrusted comment: minisign public key: 0000000000000000\nRWQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n").toString("base64");
  const { root, files } = repo(t, {
    "placeholder/tauri.conf.json": conf({ endpoints: [], pubkey: "PLACEHOLDER-REPLACE-WITH-SUITE-UPDATER-PUBKEY" }),
    "foreign/tauri.conf.json": conf({ pubkey: foreign }),
    "near/tauri.conf.json": conf({ pubkey: flip(CONTRACT_KEY, 12) }),
    "padded/tauri.conf.json": conf({ pubkey: `${CONTRACT_KEY}\n` }),
    "empty/tauri.conf.json": conf({ pubkey: "" }),
    "number/tauri.conf.json": conf({ pubkey: 7 }),
    "null/tauri.conf.json": conf({ pubkey: null }),
    "missing/tauri.conf.json": conf({ endpoints: ["https://example.test/u.json"] }),
    "blank/tauri.conf.json": conf({}),
    "platform/tauri.macos.conf.json": conf({ pubkey: "PLACEHOLDER" }),
    "cut/tauri.conf.json": '{ "plugins": { "updater": ',
    "comma/tauri.windows.conf.json": '{ "plugins": { "updater": { "pubkey": "x", } } }',
    "empty-file/tauri.conf.json": "",
  });
  const findings = suiteUpdaterKey(root, files);
  assert.deepEqual(found(findings), [
    "blank/tauri.conf.json|plugins.updater.pubkey",
    "cut/tauri.conf.json|unparseable",
    "comma/tauri.windows.conf.json|unparseable",
    "empty-file/tauri.conf.json|unparseable",
    "empty/tauri.conf.json|plugins.updater.pubkey",
    "foreign/tauri.conf.json|plugins.updater.pubkey",
    "missing/tauri.conf.json|plugins.updater.pubkey",
    "near/tauri.conf.json|plugins.updater.pubkey",
    "null/tauri.conf.json|plugins.updater.pubkey",
    "number/tauri.conf.json|plugins.updater.pubkey",
    "padded/tauri.conf.json|plugins.updater.pubkey",
    "placeholder/tauri.conf.json|plugins.updater.pubkey",
    "platform/tauri.macos.conf.json|plugins.updater.pubkey",
  ].sort());
  const problem = (file) => findings.find((f) => f.file === file).problem;
  assert.match(problem("placeholder/tauri.conf.json"), /^placeholder\/tauri\.conf\.json:\d+ has plugins\.updater\.pubkey "PLACEHOLDER-REPLACE-WITH-SUITE-U…", which is not a publicKey of the suite contract$/);
  assert.match(problem("missing/tauri.conf.json"), /^missing\/tauri\.conf\.json:\d+ has no plugins\.updater\.pubkey$/);
  assert.match(problem("number/tauri.conf.json"), /not a string$/);
  assert.match(problem("cut/tauri.conf.json"), /^cut\/tauri\.conf\.json is not valid JSON, so its plugins\.updater\.pubkey cannot be checked$/);
  for (const f of findings) assert.match(f.fix, /copy of a publicKey in contracts\/right-suite-api\.v1\.json.*Tauri's plugin config requires one/, f.file);
});

test("suite-updater-key puts the line of the pubkey, or of the updater block when the key is missing, in the problem text", (t) => {
  const { root, files } = repo(t, {
    "a/tauri.conf.json": `{\n  "productName": "App",\n  "bundle": { "createUpdaterArtifacts": true },\n  "plugins": {\n    "shell": { "open": true },\n    "updater": {\n      "endpoints": [],\n      "pubkey": "PLACEHOLDER"\n    }\n  }\n}\n`,
    "b/tauri.conf.json": `{\n  "plugins": {\n    "updater": {\n      "endpoints": []\n    }\n  },\n  "other": { "pubkey": "x" }\n}\n`,
  });
  const [a, b] = suiteUpdaterKey(root, files);
  assert.match(a.problem, /^a\/tauri\.conf\.json:8 /);
  assert.match(b.problem, /^b\/tauri\.conf\.json:3 /);
  assert.deepEqual(runRules({ root, files, only: ["suite-updater-key"] }).failures.map((f) => f.identity), ["suite-updater-key|a/tauri.conf.json|plugins.updater.pubkey", "suite-updater-key|b/tauri.conf.json|plugins.updater.pubkey"]);
});

test("suite-updater-key skips the same trees as the key rule: docs, dependencies, tests, fixtures and binaries", (t) => {
  const bad = conf({ pubkey: "PLACEHOLDER" });
  const { root, files } = repo(t, {
    "docs/tauri.conf.json": bad,
    "node_modules/x/tauri.conf.json": bad,
    "tests/tauri.conf.json": bad,
    "test/tauri.conf.json": bad,
    "src/__tests__/tauri.conf.json": bad,
    "test-vectors/tauri.conf.json": bad,
    "fixtures/app/tauri.conf.json": bad,
    "swift/Tests/tauri.conf.json": bad,
    "src/tauri.test.conf.json": bad,
    "wide/tauri.conf.json": Buffer.concat([Buffer.from(bad), Buffer.from([0])]),
    "app/tauri.conf.json": bad,
  });
  assert.deepEqual(found(suiteUpdaterKey(root, files)), ["app/tauri.conf.json|plugins.updater.pubkey"]);
});

test("engine-record-store finds the call outside tests and the licence crate, and nothing that only names it", (t) => {
  const call = "let source = rightkit_license::CredentialTokenSource::from_record_store(\n    records,\n    account,\n);\n";
  const { root, files } = repo(t, {
    "src-tauri/src/pro_gate.rs": `// reads the record\n${call}let again = CredentialTokenSource::from_record_store(records);\n`,
    "engine/bins/app/src/license.rs": call,
    "src/engine.ts": "const s = TokenSource.from_record_store(records);\n",
    "crates/rightkit-license-extra/src/a.rs": call,
    "src/only_named.rs": "//! `CredentialTokenSource::from_record_store`, the daemon-side reader\nfn from_record_store_with(x: u8) {}\nlet y = from_record_store;\n",
    // the owner, tests, documentation, dependencies, binaries
    "crates/rightkit-license/src/lib.rs": call,
    "crates/rightkit-license/src/client/reader.rs": call,
    "crates/rightkit-license/examples/read.rs": call,
    "tests/engine.rs": call,
    "src/commands/license_tests.rs": call,
    "src/architecture.test.ts": call,
    "swift/Tests/EngineTests.swift": call,
    "docs/architecture/license-api.md": call,
    "docs/example.rs": call,
    "README.md": call,
    "node_modules/x/index.js": call,
    "vendor-bin/blob.bin": Buffer.concat([Buffer.from(call), Buffer.from([0])]),
  });
  const findings = engineRecordStore(root, files);
  assert.deepEqual(found(findings), [
    "crates/rightkit-license-extra/src/a.rs|from_record_store",
    "engine/bins/app/src/license.rs|from_record_store",
    "src-tauri/src/pro_gate.rs|from_record_store",
    "src/engine.ts|from_record_store",
  ]);
  assert.match(findings.find((f) => f.file === "src-tauri/src/pro_gate.rs").problem, /^src-tauri\/src\/pro_gate\.rs:2,6 calls from_record_store: the engine reads the owner's record store itself$/);
  assert.match(findings[0].fix, /rightkit_credentials::handoff.*"Helpers, sidecars and CLIs"/);
  assert.deepEqual(runRules({ root, files, only: ["engine-record-store"] }).failures.map((f) => f.identity).sort(), found(findings).map((f) => `engine-record-store|${f}`).sort());
});

test("the rules do not find their own source or the files that register them", () => {
  const own = ["packages/release/suite-trust-roots.mjs", "packages/release/app-check.mjs", "scripts/preflight.mjs"];
  assert.deepEqual([...suiteKeyLiteral(REPO, own), ...suiteUpdaterKey(REPO, own), ...engineRecordStore(REPO, own)], []);
});

test("RULES registers the three trust-root rules after speech-link, each with its lane C3 gap", () => {
  const at = RULES.findIndex((r) => r.id === "speech-link");
  assert.ok(at >= 0);
  assert.deepEqual(RULES.slice(at + 1, at + 4).map((r) => [r.id, r.gap]), [["suite-key-literal", "C3-04"], ["suite-updater-key", "C3-14"], ["engine-record-store", "C3-05"]]);
  for (const rule of RULES.slice(at + 1, at + 4)) assert.equal(typeof rule.run, "function", rule.id);
  // RightKit's own preflight runs the same rules, the same objects.
  for (const rule of RULES.slice(at + 1, at + 4)) assert.ok(PREFLIGHT_RULES.includes(rule), `preflight runs ${rule.id}`);
});

test("through preflight: a pasted key fails with its gap and fix, a listed identity is KNOWN, a gone one is STALE", async (t) => {
  const { root: repoDir, parent } = repo(t, {
    "src/key.rs": `const KEY: &str = "${HEX}";\n`,
    "src-tauri/tauri.conf.json": conf({ pubkey: "PLACEHOLDER" }),
    "src/gate.rs": "let s = CredentialTokenSource::from_record_store(r);\n",
  });
  const run = async (known, ...more) => {
    const baseline = path.join(parent, "baseline.json");
    fs.writeFileSync(baseline, JSON.stringify({ schema: 1, about: "t", known }));
    const lines = [];
    const code = await main(["--repo", repoDir, "--worktree", "--baseline", baseline, "--only", "suite-key-literal,suite-updater-key,engine-record-store", ...more], { out: (l) => lines.push(l), err: (l) => lines.push(l) });
    return { code, text: lines.join("\n") };
  };
  const ids = ["suite-key-literal|src/key.rs|licence-key:hex", "suite-updater-key|src-tauri/tauri.conf.json|plugins.updater.pubkey", "engine-record-store|src/gate.rs|from_record_store"];
  const fresh = await run({});
  assert.equal(fresh.code, 1);
  assert.match(fresh.text, /^FAIL suite-key-literal \(C3-04\): src\/key\.rs:1 carries the suite licence public key as 64 hex digits$/m);
  assert.match(fresh.text, /^FAIL suite-updater-key \(C3-14\): src-tauri\/tauri\.conf\.json:\d+ has plugins\.updater\.pubkey "PLACEHOLDER"/m);
  assert.match(fresh.text, /^FAIL engine-record-store \(C3-05\): src\/gate\.rs:1 calls from_record_store/m);
  assert.match(fresh.text, /^ {5}fix: take the key from rightkit_license::suite_api::LICENSE_PUBLIC_KEY/m);
  const known = await run({ apprepo: ids });
  assert.equal(known.code, 0, known.text);
  assert.match(known.text, /^KNOWN suite-key-literal: src\/key\.rs:1 /m);
  assert.match(known.text, /\(3 known finding\(s\)/);
  const stale = await run({ apprepo: [...ids, "suite-key-literal|src/old.rs|licence-key:bytes"] });
  assert.equal(stale.code, 1);
  assert.match(stale.text, /^STALE baseline entry suite-key-literal\|src\/old\.rs\|licence-key:bytes: /m);
  assert.equal((await run({ apprepo: ids.slice(0, 2) })).code, 1, "an unlisted finding still fails");
});
