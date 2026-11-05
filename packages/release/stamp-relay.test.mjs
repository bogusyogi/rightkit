// GC-A21: the relay in front of the gate's inherited-output children stamps every line and ends the way its child ended.
import test from "node:test";
import assert from "node:assert";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { lineStamper } from "./stamp-relay.mjs";

const RELAY = fileURLToPath(new URL("./stamp-relay.mjs", import.meta.url));
const STAMPED = /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z /;

test("every complete line and a final partial line carry one stamp", () => {
  const out = []; const s = lineStamper((x) => out.push(x), () => "T");
  s.push("a\nb"); s.push("c\n\n"); s.push("tail"); s.end();
  assert.deepEqual(out, ["T a\n", "T bc\n", "T \n", "T tail\n"]);
});

test("the real relay stamps stdout and stderr and keeps the child's exit code", () => {
  const r = spawnSync(process.execPath, [RELAY, process.execPath, "-e", "process.stdout.write('x\\ny'); console.error('e'); process.exit(3)"], { encoding: "utf8" });
  assert.equal(r.status, 3);
  for (const line of `${r.stdout}${r.stderr}`.split("\n").filter(Boolean)) assert.match(line, STAMPED);
  assert.deepEqual(r.stdout.split("\n").filter(Boolean).map((l) => l.replace(STAMPED, "")), ["x", "y"]);
});

test("a child killed by a signal ends the relay by the same signal", { skip: process.platform === "win32" }, () => {
  const r = spawnSync(process.execPath, [RELAY, process.execPath, "-e", "process.kill(process.pid, 'SIGTERM')"], { encoding: "utf8" });
  assert.equal(r.signal, "SIGTERM");
});

test("a child that cannot start exits 127 with a stamped reason", () => {
  const r = spawnSync(process.execPath, [RELAY, "/nonexistent/stamp-relay-test"], { encoding: "utf8" });
  assert.equal(r.status, 127);
  assert.match(r.stderr, STAMPED);
});
