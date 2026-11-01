import test from "node:test";
import assert from "node:assert/strict";
import {
  consumeSyncReload,
  createSyncReloadGate,
  noteSyncAccount,
  shouldReplayHiddenSync,
} from "../src/lib/syncReloadGate";

test("hidden sync replay is preserved until the window becomes visible", () => {
  const gate = createSyncReloadGate();
  noteSyncAccount(gate, "acct_a");

  assert.equal(consumeSyncReload(gate, "hidden", true), false);
  assert.equal(gate.pendingAccountIds.has("acct_a"), true);
  assert.equal(gate.pendingWhileHidden, true);

  assert.equal(shouldReplayHiddenSync(gate, "hidden"), false);
  assert.equal(shouldReplayHiddenSync(gate, "visible"), true);

  assert.equal(consumeSyncReload(gate, "visible", true), true);
  assert.equal(gate.pendingWhileHidden, false);
  assert.equal(gate.pendingAccountIds.size, 0);
});

test("unlocked visible replay is skipped when nothing is pending", () => {
  const gate = createSyncReloadGate();

  assert.equal(shouldReplayHiddenSync(gate, "visible"), false);
  assert.equal(consumeSyncReload(gate, "visible", true), false);
  assert.equal(consumeSyncReload(gate, "visible", false), false);
});
