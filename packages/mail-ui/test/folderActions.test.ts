// Per-folder Gmail action mapping. Locks in the "1:1 Gmail web" rule so the
// 2026-07-03 "button silently no-ops because the thread doesn't carry INBOX"
// bug class can't come back. If you change lib/folderActions.ts you should
// also change these expectations; the test is the contract.
import test from "node:test";
import assert from "node:assert/strict";
import {
  getListToolbarActions,
  getReaderActions,
  invokeAction,
  isDraftThread,
  type ThreadAction,
} from "../src/lib/folderActions";

const NOOP_VERBS: ThreadAction[] = [
  // Verbs that don't have a slot in `invokeAction` because they route through
  // a dedicated path (snooze → `unsnoozeThread`, block → filters, etc.).
  // `mute` and `important` ARE bulk_actions, so they're not in this set.
  "snooze",
  "block_sender",
  "filter_like_this",
  "translate",
  "summarize",
  "important",
];

test("Drafts list toolbar only exposes Discard + read/star/snooze (no archive, no spam)", () => {
  const actions = getListToolbarActions("drafts");
  assert.ok(actions.includes("delete_drafts"), "must expose Discard");
  assert.equal(
    actions.includes("archive"),
    false,
    "drafts can't be archived (no INBOX to drop)",
  );
  assert.equal(
    actions.includes("spam"),
    false,
    "drafts can't be reported as spam (you wrote it)",
  );
  assert.equal(
    actions.includes("unarchive"),
    false,
    "drafts don't expose Move to Inbox",
  );
  // The 'mute' verb is intentionally hidden on drafts because muting your
  // own draft is meaningless.
  assert.equal(actions.includes("mute"), false);
});

test("Sent list toolbar only exposes Delete (preserves SENT label on trash)", () => {
  const actions = getListToolbarActions("sent");
  assert.ok(actions.includes("trash"));
  assert.equal(actions.includes("archive"), false);
  assert.equal(actions.includes("spam"), false);
  assert.equal(actions.includes("delete_drafts"), false);
});

test("Trash list toolbar exposes Restore + Delete-forever (the latter via the toolbar not via bulk_action)", () => {
  const actions = getListToolbarActions("trash");
  assert.ok(actions.includes("untrash"));
  assert.equal(actions.includes("archive"), false);
  assert.equal(actions.includes("spam"), false);
  assert.equal(actions.includes("trash"), false, "trash doesn't offer another trash");
});

test("Spam list toolbar exposes Not-spam + Delete-forever", () => {
  const actions = getListToolbarActions("spam");
  assert.ok(actions.includes("unspam"));
  assert.equal(actions.includes("archive"), false);
  assert.equal(actions.includes("spam"), false, "spam doesn't offer another spam report");
});

test("Snoozed list toolbar exposes Unsnooze only", () => {
  const actions = getListToolbarActions("snoozed");
  assert.ok(actions.includes("unsnooze"));
  assert.equal(actions.includes("archive"), false);
  assert.equal(actions.includes("spam"), false);
});

test("All Mail list toolbar exposes Move-to-Inbox + Delete (no Archive — already archived)", () => {
  const actions = getListToolbarActions("allmail");
  assert.ok(actions.includes("unarchive"));
  assert.ok(actions.includes("trash"));
  assert.equal(
    actions.includes("archive"),
    false,
    "everything in All Mail is already archived",
  );
  assert.equal(actions.includes("spam"), false);
});

test("Inbox list toolbar exposes the full Gmail action set", () => {
  const actions = getListToolbarActions("inbox");
  for (const verb of ["archive", "spam", "trash", "mark_read", "mark_unread", "star"]) {
    assert.ok(actions.includes(verb as ThreadAction), `inbox must include ${verb}`);
  }
});

test("Starred list toolbar mirrors Inbox (Star is a filter, not a verb that disables actions)", () => {
  const inbox = getListToolbarActions("inbox");
  const starred = getListToolbarActions("starred");
  assert.deepEqual(starred, inbox, "Starred and Inbox share the same toolbar verbs in Gmail web");
});

test("Reader toolbar mirrors the list toolbar but trims to per-thread verbs", () => {
  // Drafts: only Discard + Important (the banner Edit button is rendered separately).
  const draftActions = getReaderActions("drafts");
  assert.deepEqual(draftActions, ["delete_drafts", "important"]);
  // Sent: Delete + Important.
  assert.deepEqual(getReaderActions("sent"), ["trash", "important"]);
  // Trash: Restore + Important.
  assert.deepEqual(getReaderActions("trash"), ["untrash", "important"]);
  // Spam: Not spam + Important.
  assert.deepEqual(getReaderActions("spam"), ["unspam", "important"]);
  // Snoozed: Unsnooze + Important.
  assert.deepEqual(getReaderActions("snoozed"), ["unsnooze", "important"]);
  // All Mail: Move to Inbox + Delete + Important.
  assert.deepEqual(getReaderActions("allmail"), ["unarchive", "trash", "important"]);
});

test("Reader toolbar on Inbox exposes Archive + Spam + Trash + Mark unread + Mute + Important", () => {
  const actions = getReaderActions("inbox");
  assert.deepEqual(actions, [
    "archive",
    "spam",
    "trash",
    "mark_unread",
    "mute",
    "important",
  ]);
});

test("invokeAction routes delete_drafts to its own IPC kind, not bulk", () => {
  const r = invokeAction("delete_drafts", []);
  assert.equal(r.kind, "delete_drafts");
});

test("invokeAction routes trash/untrash/unspam/unarchive/mute to bulk_action", () => {
  for (const v of ["trash", "untrash", "unspam", "unarchive", "mute", "archive", "spam"]) {
    const r = invokeAction(v as ThreadAction, []);
    assert.equal(r.kind, "bulk", `${v} should route to bulk_action`);
  }
});

test("invokeAction maps mark_read to bulk read and mark_unread to bulk unread", () => {
  const read = invokeAction("mark_read", []);
  assert.equal(read.kind, "bulk");
  if (read.kind === "bulk") assert.equal(read.action, "read");
  const unread = invokeAction("mark_unread", []);
  assert.equal(unread.kind, "bulk");
  if (unread.kind === "bulk") assert.equal(unread.action, "unread");
});

test("invokeAction returns noop for verbs handled outside the toolbar (snooze, block, etc.)", () => {
  for (const v of NOOP_VERBS) {
    const r = invokeAction(v, []);
    assert.equal(r.kind, "noop", `${v} should be noop in this helper (handled elsewhere)`);
  }
});

test("isDraftThread is true only when DRAFT is in the label set", () => {
  assert.equal(isDraftThread(["INBOX", "DRAFT"]), true);
  assert.equal(isDraftThread(["DRAFT"]), true);
  assert.equal(isDraftThread(["INBOX"]), false);
  assert.equal(isDraftThread([]), false);
});