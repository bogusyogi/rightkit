import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const register = fileURLToPath(new URL("./ts-register.mjs", import.meta.url));

test("--import ts-register runs ts with extensionless, index and JSON imports", () => {
  const dir = mkdtempSync(join(tmpdir(), "ts-register-"));
  try {
    mkdirSync(join(dir, "lib"));
    writeFileSync(join(dir, "lib", "index.ts"), "export const fromIndex: number = 2;\n");
    writeFileSync(join(dir, "util.ts"), "export const util: string = 'u';\n");
    writeFileSync(join(dir, "data.json"), '{"n":3}\n');
    writeFileSync(join(dir, "main.ts"), [
      "import { util } from './util';",
      "import { fromIndex } from './lib';",
      "import data from './data.json';",
      "if (util !== 'u' || fromIndex !== 2 || data.n !== 3) throw new Error('bad');",
      "",
    ].join("\n"));
    const run = spawnSync(process.execPath, ["--import", register, join(dir, "main.ts")], { encoding: "utf8" });
    assert.equal(run.status, 0, run.stdout + run.stderr);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("without the hook the same import fails", () => {
  const dir = mkdtempSync(join(tmpdir(), "ts-register-"));
  try {
    writeFileSync(join(dir, "util.ts"), "export const util = 1;\n");
    writeFileSync(join(dir, "main.ts"), "import { util } from './util';\nconsole.log(util);\n");
    const run = spawnSync(process.execPath, [join(dir, "main.ts")], { encoding: "utf8" });
    assert.notEqual(run.status, 0);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
