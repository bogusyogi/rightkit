import assert from "node:assert/strict";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { initTemplate, parseInitArgs, resolveIdentity, runInitCommand } from "./init-command.mjs";
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
