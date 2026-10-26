import assert from "node:assert/strict";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { NEXT_STEPS, initTemplate, parseInitArgs, reidentifyCargoLock, rehashLegalManifest, renameText, resolveIdentity, runInitCommand, sortCargoLock } from "./init-command.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";

const TITLE = ["Right", "App"].join("");
const LOWER = ["right", "app"].join("");
const UPPER = ["RIGHT", "APP"].join("");

function fixture() {
  const root = tempRoot("right-release-init-");
  mkdirSync(path.join(root, "src-tauri"), { recursive: true });
  mkdirSync(path.join(root, "node_modules", "dep"), { recursive: true });
  writeFileSync(path.join(root, "package.json"), `{"name":"${LOWER}","productName":"${TITLE}"}\n`);
  writeFileSync(path.join(root, "src-tauri", "tauri.conf.json"), `{"identifier":"app.rightsuite.${LOWER}","env":"${UPPER}_HOME"}\n`);
  writeFileSync(path.join(root, "node_modules", "dep", "index.mjs"), `export const x = "${LOWER}";\n`);
  writeFileSync(path.join(root, "logo.png"), LOWER);
  return root;
}

test("parseInitArgs requires --name and --id and rejects unknown flags", () => {
  assert.deepEqual(parseInitArgs(["--name", "Heard Right", "--id", "app.orthiclabs.heardright", "--dry-run"]),
    { name: "Heard Right", id: "app.orthiclabs.heardright", slug: undefined, dir: ".", dryRun: true });
  assert.throws(() => parseInitArgs(["--name", "X"]), /usage/);
  assert.throws(() => parseInitArgs(["--name", "X", "--id"]), /needs a value/);
  assert.throws(() => parseInitArgs(["--name", "X", "--id", "a.b", "--bogus"]), /unknown init argument/);
});

test("resolveIdentity validates and derives the slug", () => {
  assert.equal(resolveIdentity({ name: "Heard Right", id: "app.orthiclabs.heardright" }).slug, "heardright");
  assert.throws(() => resolveIdentity({ name: "1Bad", id: "app.x.bad" }), /name must/);
  assert.throws(() => resolveIdentity({ name: "Good", id: "nodots" }), /reverse-DNS/);
  assert.throws(() => resolveIdentity({ name: "Good", id: "app.x.good", slug: "Bad-Slug" }), /slug must/);
  assert.equal(resolveIdentity({ name: "Good", id: "app.x.other" }).warnings.length, 1);
});

test("initTemplate rewrites the template identity in text files only", () => {
  const root = fixture();
  const result = initTemplate(root, { name: "Heard Right", id: "app.orthiclabs.heardright" });
  assert.deepEqual(result.changed, ["package.json", "src-tauri/tauri.conf.json"]);
  assert.equal(readFileSync(path.join(root, "package.json"), "utf8"), `{"name":"heardright","productName":"Heard Right"}\n`);
  assert.equal(readFileSync(path.join(root, "src-tauri", "tauri.conf.json"), "utf8"),
    `{"identifier":"app.orthiclabs.heardright","env":"HEARDRIGHT_HOME"}\n`);
  assert.ok(readFileSync(path.join(root, "node_modules", "dep", "index.mjs"), "utf8").includes(LOWER));
  assert.equal(readFileSync(path.join(root, "logo.png"), "utf8"), LOWER);
});

test("initTemplate carries the renamed Cargo.lock's digest into every notice that records it (notice-lock-digest CA-14)", async () => {
  const { createHash } = await import("node:crypto");
  const sha = (text) => createHash("sha256").update(text).digest("hex");
  const root = fixture();
  const lock = `version = 4\n\n[[package]]\nname = "${LOWER}"\nversion = "0.1.0"\n`;
  const other = "f".repeat(64);
  writeFileSync(path.join(root, "Cargo.lock"), lock);
  mkdirSync(path.join(root, "legal"), { recursive: true });
  writeFileSync(path.join(root, "legal", "THIRD_PARTY_NOTICES.md"), `inputs: pnpm-lock.yaml sha256 ${other}; Cargo.lock sha256 ${sha(lock)}\n`);
  writeFileSync(path.join(root, "legal", "THIRD_PARTY_RUST_LICENSES.html"), `<!-- Cargo.lock sha256=${sha(lock)} -->\n`);
  const result = initTemplate(root, { name: "Heard Right", id: "app.orthiclabs.heardright" });
  const renamed = readFileSync(path.join(root, "Cargo.lock"), "utf8");
  assert.notEqual(renamed, lock, "the rename changes the lock");
  assert.equal(readFileSync(path.join(root, "legal", "THIRD_PARTY_NOTICES.md"), "utf8"), `inputs: pnpm-lock.yaml sha256 ${other}; Cargo.lock sha256 ${sha(renamed)}\n`, "only the lock's digest moves");
  assert.equal(readFileSync(path.join(root, "legal", "THIRD_PARTY_RUST_LICENSES.html"), "utf8"), `<!-- Cargo.lock sha256=${sha(renamed)} -->\n`);
  assert.ok(result.changed.includes("legal/THIRD_PARTY_NOTICES.md") && result.changed.includes("legal/THIRD_PARTY_RUST_LICENSES.html"));
});

test("initTemplate moves only the lock's provenance field: the same digest anywhere else is left alone", async () => {
  const { createHash } = await import("node:crypto");
  const sha = (text) => createHash("sha256").update(text).digest("hex");
  const root = fixture();
  const lock = `version = 4\n\n[[package]]\nname = "${LOWER}"\nversion = "0.1.0"\n`;
  const old = sha(lock);
  writeFileSync(path.join(root, "Cargo.lock"), lock);
  mkdirSync(path.join(root, "legal"), { recursive: true });
  mkdirSync(path.join(root, "evidence"), { recursive: true });
  // Same 64 hex characters as the lock's digest, but not the lock's provenance field.
  const unrelated = {
    "legal/THIRD_PARTY_NOTICES.md": `inputs: pnpm-lock.yaml sha256 ${old}; Cargo.lock sha256 ${old}\n`,
    "evidence/run.json": `{"input":"${old}","lock":"Cargo.lock"}\n`,
    "src-tauri/fixture.rs": `const PINNED: &str = "${old}"; // Cargo.lock\n`,
    "legal/other.md": `Cargo.toml sha256 ${old}\n`,
  };
  for (const [file, text] of Object.entries(unrelated)) writeFileSync(path.join(root, file), text);
  initTemplate(root, { name: "Heard Right", id: "app.orthiclabs.heardright" });
  const now = sha(readFileSync(path.join(root, "Cargo.lock"), "utf8"));
  assert.notEqual(now, old);
  assert.equal(readFileSync(path.join(root, "legal", "THIRD_PARTY_NOTICES.md"), "utf8"), `inputs: pnpm-lock.yaml sha256 ${old}; Cargo.lock sha256 ${now}\n`, "the pnpm-lock field keeps its value; only the Cargo.lock field moves");
  for (const file of ["evidence/run.json", "src-tauri/fixture.rs", "legal/other.md"]) assert.equal(readFileSync(path.join(root, file), "utf8"), unrelated[file], `${file} is not a Cargo.lock provenance field`);
});

test("renameText leaves the RightApps distribution service alone: only the template identity renames", () => {
  const identity = { name: "Play Right", id: "app.orthiclabs.playright", slug: "playright" };
  const text = [`"provider": "r2-${LOWER}s"`, `${TITLE}s registry`, `${UPPER}S_HOST`, `${LOWER}-engine ${LOWER}_protocol ${UPPER}_HOME ${TITLE}_0.1.0 app.rightsuite.${LOWER}`, `${LOWER}s.`, `${LOWER}store`].join("\n");
  assert.equal(renameText(text, identity), [`"provider": "r2-${LOWER}s"`, `${TITLE}s registry`, `${UPPER}S_HOST`, "playright-engine playright_protocol PLAYRIGHT_HOME Play Right_0.1.0 app.orthiclabs.playright", `${LOWER}s.`, "playrightstore"].join("\n"));
});

test("dry run reports changes and writes nothing; a second run changes nothing", () => {
  const root = fixture();
  const before = readFileSync(path.join(root, "package.json"), "utf8");
  const lines = [];
  runInitCommand(["--name", "Heard Right", "--id", "app.orthiclabs.heardright", "--dir", root, "--dry-run"], { stdout: (l) => lines.push(l), stderr: () => {} });
  assert.equal(readFileSync(path.join(root, "package.json"), "utf8"), before);
  assert.match(lines[0], /2 file\(s\) would change/);
  initTemplate(root, { name: "Heard Right", id: "app.orthiclabs.heardright" });
  assert.deepEqual(initTemplate(root, { name: "Heard Right", id: "app.orthiclabs.heardright" }).changed, []);
});

// A lock fragment shaped like cargo writes it: [[package]] tables by name, `dependencies` lists by name.
const LOCK = [
  "# This file is automatically @generated by Cargo.",
  "# It is not intended for manual editing.",
  "version = 4",
  "",
  "[[package]]",
  'name = "bytes"',
  'version = "1.0.0"',
  'source = "registry+https://github.com/rust-lang/crates.io-index"',
  'checksum = "00"',
  "",
  "[[package]]",
  `name = "${LOWER}"`,
  'version = "0.1.0"',
  "dependencies = [",
  ` "${LOWER}-protocol",`,
  ' "rightkit-job-queue",',
  ' "serde_json",',
  "]",
  "",
  "[[package]]",
  `name = "${LOWER}-protocol"`,
  'version = "0.1.0"',
  "dependencies = [",
  ' "rightkit-job-queue",',
  "]",
  "",
  "[[package]]",
  'name = "rightkit-job-queue"',
  'version = "0.5.1"',
  'source = "registry+https://github.com/rust-lang/crates.io-index"',
  'checksum = "11"',
  "",
  "[[package]]",
  'name = "serde_json"',
  'version = "1.0.0"',
  'source = "registry+https://github.com/rust-lang/crates.io-index"',
  'checksum = "22"',
  "",
].join("\n");

const lockNames = (text) => [...text.matchAll(/^name = "([^"]+)"$/gm)].map((m) => m[1]);
const lockDeps = (text, name) => {
  const block = text.split("\n\n").find((b) => b.includes(`name = "${name}"\n`));
  return /dependencies = \[\n([\s\S]*?)\n\]/.exec(block)?.[1].split("\n").map((line) => line.trim().replace(/^"|",?$/g, "")) ?? [];
};

test("reidentifyCargoLock renames the template crates and restores cargo's order so --locked accepts the lock", () => {
  // "zeta" sorts after every crate in the lock: the renamed tables and dependency entries must move to the end.
  const zeta = reidentifyCargoLock(LOCK, { name: "Zeta", id: "app.orthiclabs.zeta", slug: "zeta" });
  assert.deepEqual(lockNames(zeta), ["bytes", "rightkit-job-queue", "serde_json", "zeta", "zeta-protocol"]);
  assert.deepEqual(lockDeps(zeta, "zeta"), ["rightkit-job-queue", "serde_json", "zeta-protocol"]);
  assert.ok(!zeta.includes(LOWER));
  assert.ok(zeta.startsWith("# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n\n[[package]]\n"));
  assert.ok(zeta.endsWith('checksum = "11"\n\n[[package]]\nname = "serde_json"\nversion = "1.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "22"\n\n[[package]]\nname = "zeta"\nversion = "0.1.0"\ndependencies = [\n "rightkit-job-queue",\n "serde_json",\n "zeta-protocol",\n]\n\n[[package]]\nname = "zeta-protocol"\nversion = "0.1.0"\ndependencies = [\n "rightkit-job-queue",\n]\n'));
  // "apex" sorts first.
  assert.deepEqual(lockNames(reidentifyCargoLock(LOCK, { name: "Apex", id: "app.orthiclabs.apex", slug: "apex" })), ["apex", "apex-protocol", "bytes", "rightkit-job-queue", "serde_json"]);
  // CRLF survives, and an already sorted lock is returned byte for byte.
  assert.equal(sortCargoLock(LOCK), LOCK);
  assert.equal(sortCargoLock(LOCK.replace(/\n/g, "\r\n")), LOCK.replace(/\n/g, "\r\n"));
});

test("sortCargoLock keeps same-name packages in version order and leaves trailing tables alone", () => {
  const lock = [
    "version = 4", "",
    "[[package]]", 'name = "b"', 'version = "0.10.0"', "",
    "[[package]]", 'name = "a"', 'version = "1.0.0"', "dependencies = [", ' "b 0.10.0",', ' "b 0.9.0",', "]", "",
    "[[package]]", 'name = "b"', 'version = "0.9.0"', "",
    "[metadata]", 'x = "y"', "",
  ].join("\n");
  const sorted = sortCargoLock(lock);
  assert.deepEqual(lockNames(sorted), ["a", "b", "b"]);
  assert.deepEqual([...sorted.matchAll(/^version = "([^"]+)"$/gm)].map((m) => m[1]), ["1.0.0", "0.9.0", "0.10.0"]);
  assert.match(sorted, /dependencies = \[\n "b 0\.9\.0",\n "b 0\.10\.0",\n\]/);
  assert.ok(sorted.endsWith('\n\n[metadata]\nx = "y"\n'));
});

test("initTemplate re-identifies Cargo.lock (it has no text extension) and the committed template lock round-trips", () => {
  const root = fixture();
  writeFileSync(path.join(root, "Cargo.lock"), LOCK);
  const result = initTemplate(root, { name: "Zeta", id: "app.orthiclabs.zeta" });
  assert.ok(result.changed.includes("Cargo.lock"));
  assert.deepEqual(lockNames(readFileSync(path.join(root, "Cargo.lock"), "utf8")), ["bytes", "rightkit-job-queue", "serde_json", "zeta", "zeta-protocol"]);
  const committed = readFileSync(new URL("../../desktop-app-template/Cargo.lock", import.meta.url), "utf8");
  assert.equal(sortCargoLock(committed), committed, "the template lock is in cargo's order, so the sorter changes nothing it did not rename");
});

// ---- the legal manifest digests follow the renamed bytes ----------------------------------------------------------------------
import { createHash } from "node:crypto";
const sha = (text) => createHash("sha256").update(text).digest("hex");
function legalFixture() {
  const root = fixture();
  mkdirSync(path.join(root, "legal"), { recursive: true });
  const docs = {
    "EULA.md": "Suite terms, no product name.\n",
    "PRIVACY_NOTICE.md": `# ${TITLE} Privacy Notice\n`,
    "THIRD_PARTY_NPM_LICENSES.html": `<p>Build provenance: ${TITLE} 0.1.0</p>\n`,
  };
  for (const [name, text] of Object.entries(docs)) writeFileSync(path.join(root, "legal", name), text);
  const manifest = {
    schema: 2, appKey: LOWER, productName: TITLE,
    documents: [
      { id: "eula", role: "eula", path: "EULA.md", material: true, sha256: sha(docs["EULA.md"]) },
      { id: "privacy", role: "privacy_notice", path: "PRIVACY_NOTICE.md", material: false, sha256: sha(docs["PRIVACY_NOTICE.md"]) },
    ],
    appendices: [{ id: "npm", title: "npm", path: "THIRD_PARTY_NPM_LICENSES.html", sha256: sha(docs["THIRD_PARTY_NPM_LICENSES.html"]) }],
  };
  writeFileSync(path.join(root, "legal", "legal-manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
  return { root, docs };
}
const readManifest = (root) => JSON.parse(readFileSync(path.join(root, "legal", "legal-manifest.json"), "utf8"));

test("initTemplate recomputes the legal manifest digests of documents and appendices the rename changed, and only those", () => {
  const { root } = legalFixture();
  const result = initTemplate(root, { name: "Heard Right", id: "app.orthiclabs.heardright" });
  const manifest = readManifest(root);
  const bytes = (name) => readFileSync(path.join(root, "legal", name));
  assert.equal(manifest.documents[0].sha256, sha(bytes("EULA.md")), "an identity-free document keeps its digest");
  assert.equal(manifest.documents[1].sha256, sha(bytes("PRIVACY_NOTICE.md")));
  assert.equal(manifest.appendices[0].sha256, sha(bytes("THIRD_PARTY_NPM_LICENSES.html")));
  assert.match(bytes("PRIVACY_NOTICE.md").toString("utf8"), /^# Heard Right Privacy Notice/);
  assert.equal(manifest.appKey, "heardright");
  assert.ok(result.changed.includes("legal/legal-manifest.json"));
  assert.deepEqual(result.warnings.filter((w) => /material legal document/.test(w)), []);
  assert.deepEqual(initTemplate(root, { name: "Heard Right", id: "app.orthiclabs.heardright" }).changed, [], "a second run changes nothing");
});

test("a dry run reports the manifest as changed and writes nothing", () => {
  const { root } = legalFixture();
  const before = readFileSync(path.join(root, "legal", "legal-manifest.json"), "utf8");
  const result = initTemplate(root, { name: "Heard Right", id: "app.orthiclabs.heardright", dryRun: true });
  assert.ok(result.changed.includes("legal/legal-manifest.json"));
  assert.equal(readFileSync(path.join(root, "legal", "legal-manifest.json"), "utf8"), before);
  assert.match(readFileSync(path.join(root, "legal", "PRIVACY_NOTICE.md"), "utf8"), new RegExp(`^# ${TITLE}`));
});

test("a rename that changes a material legal document warns that the acceptance contract needs legal-snapshot", () => {
  const { root } = legalFixture();
  writeFileSync(path.join(root, "legal", "EULA.md"), `The ${TITLE} terms.\n`);
  const manifest = readManifest(root);
  manifest.documents[0].sha256 = sha(`The ${TITLE} terms.\n`);
  writeFileSync(path.join(root, "legal", "legal-manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
  const result = initTemplate(root, { name: "Heard Right", id: "app.orthiclabs.heardright" });
  assert.equal(readManifest(root).documents[0].sha256, sha("The Heard Right terms.\n"));
  assert.ok(result.warnings.some((w) => /material legal document legal\/EULA\.md/.test(w) && /legal-snapshot/.test(w)), result.warnings.join("\n"));
});

test("rehashLegalManifest leaves a manifest whose digests already match byte for byte, and skips a file it cannot read", () => {
  const text = `${JSON.stringify({ documents: [{ path: "a.md", sha256: sha("a") }, { path: "gone.md", sha256: "x" }] }, null, 2)}\n`;
  const same = rehashLegalManifest(text, (file) => (file === "a.md" ? "a" : null));
  assert.deepEqual([same.changed, same.text], [[], text]);
  const moved = rehashLegalManifest(text, (file) => (file === "a.md" ? "b" : null));
  assert.deepEqual(moved.changed, ["a.md"]);
  assert.equal(JSON.parse(moved.text).documents[0].sha256, sha("b"));
  assert.equal(JSON.parse(moved.text).documents[1].sha256, "x");
});

test("init prints the steps no command can do, including the host registration and trust-refresh a person runs", () => {
  const root = fixture();
  const lines = [];
  runInitCommand(["--name", "Heard Right", "--id", "app.orthiclabs.heardright", "--dir", root], { stdout: (l) => lines.push(l), stderr: () => {} });
  const text = lines.join("\n");
  assert.match(text, /^next:$/m);
  for (const step of NEXT_STEPS) assert.ok(text.includes(step), step);
  assert.match(text, /ownership-repositories\.json/);
  assert.match(text, /right-release ownership trust-refresh/);
  const dry = [];
  runInitCommand(["--name", "Heard Right", "--id", "app.orthiclabs.heardright", "--dir", fixture(), "--dry-run"], { stdout: (l) => dry.push(l), stderr: () => {} });
  assert.ok(!dry.join("\n").includes("next:"), "a dry run prints no next steps");
});
