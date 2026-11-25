// GC-A15 at the check stage (bin/rightkit-gate.mjs cargoProfileRuleLines): an app commit that carries a Cargo profile table other than
// [profile.release] is refused by check, not only by the release tool. Judged per app root the commit tracks (the directory of a
// right-release.config), by the release tool's own scan of the commit's snapshot. RightKit's own repository is not an app and gives no line.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { cargoProfileRuleLines } from "./bin/rightkit-gate.mjs";

const PACKAGE = (name) => `[package]\nname = "${name}"\nversion = "0.1.0"\nedition = "2021"\n`;

/** A snapshot directory holding `files` ({ "relative/path": text }); returns { dir, tracked }. */
function snapshot(t, files) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-profile-check-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  for (const [rel, body] of Object.entries(files)) { const file = path.join(dir, ...rel.split("/")); fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body); }
  return { dir, tracked: Object.keys(files) };
}

test("an app at the repository root with [profile.dev] gives one line that names the table, under the repository's label", async (t) => {
  const s = snapshot(t, { "right-release.config.json": "{}\n", "Cargo.toml": `${PACKAGE("app")}\n[profile.dev]\nopt-level = 1\n`, "src/lib.rs": "" });
  const lines = await cargoProfileRuleLines({ tracked: s.tracked, snapshotDir: s.dir, label: "witness-app" });
  assert.equal(lines.length, 1, lines.join("\n"));
  assert.match(lines[0], /^witness-app Cargo\.toml\b.*\[profile-extra\] \[profile\.dev\] is not allowed/);
});

test("an app with no extra profile table gives no line; [profile.release] is allowed", async (t) => {
  const none = snapshot(t, { "right-release.config.json": "{}\n", "Cargo.toml": PACKAGE("app"), "src/lib.rs": "" });
  assert.deepEqual(await cargoProfileRuleLines({ tracked: none.tracked, snapshotDir: none.dir, label: "app" }), []);
  const release = snapshot(t, { "right-release.config.json": "{}\n", "Cargo.toml": `${PACKAGE("app")}\n[profile.release]\nopt-level = 3\n`, "src/lib.rs": "" });
  assert.deepEqual(await cargoProfileRuleLines({ tracked: release.tracked, snapshotDir: release.dir, label: "app" }), []);
});

test("each app root is judged on its own and named by its directory; a manifest outside every app root is not judged here", async (t) => {
  const s = snapshot(t, {
    "apps/one/right-release.config.mjs": "export default {};\n", "apps/one/src-tauri/Cargo.toml": `${PACKAGE("one")}\n[profile.bench]\ndebug = true\n`, "apps/one/src-tauri/src/lib.rs": "",
    "apps/two/right-release.config.json": "{}\n", "apps/two/Cargo.toml": PACKAGE("two"), "apps/two/src/lib.rs": "",
    "tools/helper/Cargo.toml": `${PACKAGE("helper")}\n[profile.dev]\nopt-level = 1\n`, "tools/helper/src/lib.rs": "",
  });
  const lines = await cargoProfileRuleLines({ tracked: s.tracked, snapshotDir: s.dir, label: "repo" });
  assert.equal(lines.length, 1, lines.join("\n"));
  assert.match(lines[0], /^apps\/one .*\[profile\.bench\] is not allowed/);
});

test("a workspace member manifest with [profile.dev] gives a line (GC-A15: an app carries no profile table but [profile.release], in any manifest)", async (t) => {
  const s = snapshot(t, {
    "right-release.config.json": "{}\n",
    "Cargo.toml": "[workspace]\nmembers = [\"crates/member\"]\nresolver = \"2\"\n",
    "crates/member/Cargo.toml": `${PACKAGE("member")}\n[profile.dev]\nopt-level = 1\n`,
    "crates/member/src/lib.rs": "",
  });
  const lines = await cargoProfileRuleLines({ tracked: s.tracked, snapshotDir: s.dir, label: "app" });
  assert.equal(lines.length, 1, lines.join("\n") || "no line: member profile table not judged");
  assert.match(lines[0], /\[profile-extra\] \[profile\.dev\] is not allowed/);
});

test("RightKit's own repository gives no line (it is judged per unit); a repository that only looks like it by path is judged", async (t) => {
  const own = JSON.parse(fs.readFileSync(new URL("./package.json", import.meta.url), "utf8")).name;
  const files = (name) => ({ "packages/release/package.json": `${JSON.stringify({ name })}\n`, "right-release.config.json": "{}\n", "Cargo.toml": `${PACKAGE("e2e")}\n[profile.dev]\nopt-level = 1\n`, "src/lib.rs": "" });
  const rightkit = snapshot(t, files(own));
  assert.deepEqual(await cargoProfileRuleLines({ tracked: rightkit.tracked, snapshotDir: rightkit.dir, label: "rightkit" }), []);
  const other = snapshot(t, files("@someone/else"));
  assert.equal((await cargoProfileRuleLines({ tracked: other.tracked, snapshotDir: other.dir, label: "other" })).length, 1);
});
