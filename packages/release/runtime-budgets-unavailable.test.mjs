import assert from "node:assert/strict";
import test from "node:test";
import { figureUnavailableOn, HOST_UNAVAILABLE_FIGURES, parseBudgetResult } from "./runtime-budgets.mjs";

const line = (disk) => `RESULT budget crate=rightkit-x version=0.1.0 resident_bytes=1000 idle_cpu_percent=0.1 wakeups_per_s=0.5 disk_ops_per_s=${disk} scale_unit=records scale_count=10`;

test("disk_ops_per_s may be unavailable (null); resident_bytes and scale_count never", () => {
  const parsed = parseBudgetResult(line("unavailable"));
  assert.equal(parsed.ok, true);
  assert.equal(parsed.value.diskOpsPerSecond, null);
  assert.equal(parseBudgetResult(line("unavailable").replace("resident_bytes=1000", "resident_bytes=unavailable")).ok, false);
  assert.equal(parseBudgetResult(line("unavailable").replace("scale_count=10", "scale_count=unavailable")).ok, false);
  assert.equal(parseBudgetResult(line("-1")).ok, false);
});

test("only macOS may leave disk operations unmeasured", () => {
  assert.deepEqual(HOST_UNAVAILABLE_FIGURES.darwin, ["diskOpsPerSecond"]);
  assert.equal(figureUnavailableOn("darwin", "diskOpsPerSecond"), true);
  assert.equal(figureUnavailableOn("darwin", "wakeupsPerSecond"), false);
  assert.equal(figureUnavailableOn("win32", "diskOpsPerSecond"), false);
  assert.equal(figureUnavailableOn("linux", "diskOpsPerSecond"), false);
});
