import assert from "node:assert/strict";
import { spawnSync } from "./background-process.mjs";
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";

test("main guard runs CLI through a directory symlink", (t) => {
  const root = mkdtempSync(path.join(tmpdir(), "rk-main-guard-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const target = path.join(root, "target"), link = path.join(root, "link");
  mkdirSync(target);
  writeFileSync(path.join(target, "cli.mjs"), `import { realpathSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) process.stdout.write("ran\\n");
`);
  try { symlinkSync(target, link, "dir"); }
  catch (error) {
    if (process.platform === "win32" && error.code === "EPERM") { t.skip("directory symlinks require Windows permission"); return; }
    throw error;
  }
  const result = spawnSync(process.execPath, [path.join(link, "cli.mjs")], { encoding: "utf8", windowsHide: true, timeout: 5_000 });
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, "ran\n");
});
