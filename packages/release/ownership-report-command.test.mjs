import assert from "node:assert/strict";
import { readFileSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";

test("report CLI writes Markdown and JSON and fails closed for unscanned apps", () => {
  const workspace = mkdtempSync(path.join(tmpdir(), "rk-report-cli-"));
  const markdown = path.join(workspace, "report.md"), json = path.join(workspace, "report.json");
  const command = fileURLToPath(new URL("./ownership-report-command.mjs", import.meta.url));
  try {
    const result = spawnSync(process.execPath, [command, "--workspace", workspace, "--markdown", markdown, "--json-out", json, "--timeout-ms", "1000"], { encoding: "utf8", windowsHide: true });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /ownership report:/);
    const report = JSON.parse(readFileSync(json, "utf8"));
    assert.ok(report.apps.length > 0);
    assert.ok(report.apps.every((app) => app.status === "unscanned"));
    assert.match(readFileSync(markdown, "utf8"), /RightKit ownership report/);
  } finally { rmSync(workspace, { recursive: true, force: true }); }
});
