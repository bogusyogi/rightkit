// The ledger records a Windows shell-shim start as the process it carries (spawn-ledger.mjs unwrapCmdShim), so acceptance
// judges see rustright/cargo/pnpm starts instead of opaque cmd.exe strings (NN40 on Dell, run muse-fulldev-4).
import assert from "node:assert/strict";
import { test } from "node:test";
import { unwrapCmdShim } from "./spawn-ledger.mjs";

test("a gate-built cmd shim start unwraps to the shim and its exact arguments", () => {
  const got = unwrapCmdShim("C:\\Windows\\System32\\cmd.exe", ["/d", "/s", "/c", '""D:\\.rightkit-managed\\agent-bin\\rustright.cmd" cargo doc --profile release -p rk-core"']);
  assert.deepEqual(got, { cmd: "D:\\.rightkit-managed\\agent-bin\\rustright.cmd", args: ["cargo", "doc", "--profile", "release", "-p", "rk-core"] });
  assert.deepEqual(unwrapCmdShim("cmd.exe", ["/c", '"C:\\pnpm\\pnpm.cmd" --version']), { cmd: "C:\\pnpm\\pnpm.cmd", args: ["--version"] });
  assert.deepEqual(unwrapCmdShim("cmd.exe", ["/d", "/s", "/c", '""C:\\a b\\tool.cmd""']), { cmd: "C:\\a b\\tool.cmd", args: [] });
});

test("anything that is not a cmd shim start is recorded unchanged", () => {
  assert.deepEqual(unwrapCmdShim("git", ["rev-parse", "HEAD"]), { cmd: "git", args: ["rev-parse", "HEAD"] });
  assert.deepEqual(unwrapCmdShim("cmd.exe", ["/k", "echo hi"]), { cmd: "cmd.exe", args: ["/k", "echo hi"] });
  assert.deepEqual(unwrapCmdShim("node.exe", ["-e", "1"]), { cmd: "node.exe", args: ["-e", "1"] });
});
