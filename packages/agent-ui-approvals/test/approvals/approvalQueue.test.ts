import test from "node:test";
import assert from "node:assert/strict";
import {
  APPROVAL_TIMEOUT_MS,
  DELETE_NOTICE,
  FORWARD_NOTICE,
  approveAriaLabel,
  approveLabel,
  decisionMessage,
  denyAriaLabel,
  emptyQueue,
  formatCountdown,
  isExpired,
  kindHeading,
  kindNotice,
  nothingUnlessApproved,
  queuePosition,
  queueReducer,
  requestSentence,
  secondsLeft,
} from "../../src/approvals/lib/approvalQueue";
import { bodyStats, displayBody, visibleText } from "@rightkit/mail-ui/lib/visibleText";
import type { ApprovalKind, PendingApproval } from "@rightkit/mail-ui/lib/ipc/agentApprovals";

function item(id: string, created: number, kind: ApprovalKind = "send"): PendingApproval {
  return {
    id,
    kind,
    agent_name: "bot",
    account: "a@x.com",
    created_at: created,
    expires_at: created + APPROVAL_TIMEOUT_MS,
  };
}

test("replace sorts oldest first and drops duplicates", () => {
  const s = queueReducer(emptyQueue, {
    type: "replace",
    items: [item("b", 20), item("a", 10), item("b", 20)],
  });
  assert.deepEqual(
    s.items.map((i) => i.id),
    ["a", "b"],
  );
});

test("replace is authoritative: items the app no longer lists disappear", () => {
  let s = queueReducer(emptyQueue, {
    type: "replace",
    items: [item("a", 1), item("b", 2)],
  });
  s = queueReducer(s, { type: "replace", items: [item("b", 2)] });
  assert.deepEqual(
    s.items.map((i) => i.id),
    ["b"],
  );
});

test("decided removes only that approval and keeps the order", () => {
  let s = queueReducer(emptyQueue, {
    type: "replace",
    items: [item("a", 1), item("b", 2), item("c", 3)],
  });
  s = queueReducer(s, { type: "decided", id: "a" });
  assert.deepEqual(
    s.items.map((i) => i.id),
    ["b", "c"],
  );
  assert.equal(queueReducer(s, { type: "decided", id: "zzz" }).items.length, 2);
});

test("expire drops approvals at and past their expiry", () => {
  const s = queueReducer(emptyQueue, {
    type: "replace",
    items: [item("a", 0), item("b", 1000)],
  });
  const before = queueReducer(s, { type: "expire", now: APPROVAL_TIMEOUT_MS - 1 });
  assert.equal(before.items.length, 2);
  const at = queueReducer(s, { type: "expire", now: APPROVAL_TIMEOUT_MS });
  assert.deepEqual(
    at.items.map((i) => i.id),
    ["b"],
  );
  assert.equal(queueReducer(s, { type: "expire", now: 1e13 }).items.length, 0);
});

test("the exact expiry moment counts as expired, like the app", () => {
  assert.equal(isExpired(1000, 999), false);
  assert.equal(isExpired(1000, 1000), true);
});

test("countdown rounds up, clamps at zero and formats as m:ss", () => {
  assert.equal(secondsLeft(10_000, 0), 10);
  assert.equal(secondsLeft(10_001, 0), 11);
  assert.equal(secondsLeft(10_000, 10_000), 0);
  assert.equal(secondsLeft(10_000, 99_999), 0);
  assert.equal(formatCountdown(0), "0:00");
  assert.equal(formatCountdown(9), "0:09");
  assert.equal(formatCountdown(65), "1:05");
  assert.equal(formatCountdown(900), "15:00");
  assert.equal(formatCountdown(-5), "0:00");
});

test("queue position shows a count only when several are waiting", () => {
  assert.equal(queuePosition(1), "");
  assert.equal(queuePosition(3), "1 of 3");
});

test("decision messages never claim a send that did not happen", () => {
  assert.match(decisionMessage({ status: "sent", reason: null }) ?? "", /^The message was sent/);
  assert.equal(decisionMessage({ status: "denied", reason: "user" }), null);
  for (const reason of [
    "draft_changed",
    "unknown_outcome",
    "no_credentials",
    "not_permitted",
    "fetch_failed",
  ]) {
    const m = decisionMessage({ status: "failed", reason }) ?? "";
    assert.ok(m.length > 0, reason);
    assert.doesNotMatch(m, /message was sent/, reason);
  }
  assert.match(
    decisionMessage({ status: "denied", reason: "timeout" }) ?? "",
    /Nothing was sent/,
  );
});

test("visibleText marks bidi and invisible characters but keeps newline and tab", () => {
  assert.equal(visibleText("a\u202Eb"), "a\u27E6U+202E\u27E7b");
  assert.equal(
    visibleText("x\u200By\uFEFFz\u00ADw"),
    "x\u27E6U+200B\u27E7y\u27E6U+FEFF\u27E7z\u27E6U+00AD\u27E7w",
  );
  assert.equal(
    visibleText("\u061C\u2066\u2069\u2060"),
    "\u27E6U+061C\u27E7\u27E6U+2066\u27E7\u27E6U+2069\u27E7\u27E6U+2060\u27E7",
  );
  assert.equal(visibleText("a\u0007b\u0085c"), "a\u27E6U+0007\u27E7b\u27E6U+0085\u27E7c");
  assert.equal(visibleText("line\n\tindent caf\u00E9"), "line\n\tindent caf\u00E9");
});

test("bodyStats counts characters and lines", () => {
  assert.deepEqual(bodyStats("ab\r\ncd"), { chars: 5, lines: 2 });
  assert.deepEqual(bodyStats(""), { chars: 0, lines: 0 });
});

test("displayBody folds three or more blank lines and marks hidden text", () => {
  assert.equal(displayBody("a\n\n\n\n\nb"), "a\n[4 blank lines]\nb");
  assert.equal(displayBody("a\n\n\n\nb"), "a\n[3 blank lines]\nb");
  assert.equal(displayBody("a\n\n\nb"), "a\n\n\nb");
  assert.equal(displayBody("a\u202Eb"), "a\u27E6U+202E\u27E7b");
});

const KINDS: ApprovalKind[] = ["send", "forward", "delete"];

test("each kind has its own heading and an Approve button that names the action", () => {
  assert.equal(kindHeading("send"), "Send");
  assert.equal(kindHeading("forward"), "Forward");
  assert.equal(kindHeading("delete"), "Move to Trash");
  for (const k of KINDS) assert.equal(approveLabel(k), kindHeading(k));
  assert.equal(new Set(KINDS.map(kindHeading)).size, 3);
  assert.equal(new Set(KINDS.map(requestSentence)).size, 3);
  assert.equal(new Set(KINDS.map(nothingUnlessApproved)).size, 3);
  // Nothing in a button or label says "Approve and send" for a forward or a delete.
  for (const k of ["forward", "delete"] as ApprovalKind[]) {
    assert.doesNotMatch(approveAriaLabel(k, "a@x.com"), /send/i, k);
    assert.doesNotMatch(denyAriaLabel(k, "bot"), /send/i, k);
  }
});

test("a forward and a delete show their distinct notice line, a send shows none", () => {
  assert.equal(kindNotice("send"), null);
  assert.equal(
    kindNotice("forward"),
    "This forwards an existing message, including its attachments, to the recipients above.",
  );
  assert.equal(kindNotice("forward"), FORWARD_NOTICE);
  assert.equal(
    kindNotice("delete"),
    "This moves the conversation to Trash. You can restore it from Trash for 30 days.",
  );
  assert.equal(kindNotice("delete"), DELETE_NOTICE);
});

test("the queue keeps the kind of every item and the order across kinds", () => {
  const s = queueReducer(emptyQueue, {
    type: "replace",
    items: [item("c", 3, "delete"), item("a", 1, "send"), item("b", 2, "forward")],
  });
  assert.deepEqual(
    s.items.map((i) => [i.id, i.kind]),
    [
      ["a", "send"],
      ["b", "forward"],
      ["c", "delete"],
    ],
  );
});

test("a forward is never reported as a plain send, and a delete never as sent", () => {
  assert.match(decisionMessage({ status: "sent", reason: null }, "forward") ?? "", /forwarded/);
  assert.match(decisionMessage({ status: "trashed", reason: null }, "delete") ?? "", /moved to Trash/);
  for (const reason of [
    "draft_changed",
    "unknown_outcome",
    "no_credentials",
    "not_permitted",
    "fetch_failed",
  ]) {
    const m = decisionMessage({ status: "failed", reason }, "forward") ?? "";
    assert.ok(m.length > 0, reason);
    assert.match(m, /forward/i, reason);
    assert.doesNotMatch(m, /message was (sent|forwarded)/, reason);
  }
  for (const reason of [
    "unknown_outcome",
    "no_credentials",
    "not_permitted",
    "thread_missing",
    "gmail_error",
  ]) {
    const m = decisionMessage({ status: "failed", reason }, "delete") ?? "";
    assert.ok(m.length > 0, reason);
    assert.match(m, /Trash/, reason);
    assert.doesNotMatch(m, /conversation was moved to Trash/, reason);
  }
  assert.equal(
    decisionMessage({ status: "failed", reason: "draft_changed" }, "delete"),
    "This conversation changed after you opened this request. Nothing was moved.",
  );
  assert.equal(decisionMessage({ status: "denied", reason: "user" }, "delete"), null);
  assert.match(
    decisionMessage({ status: "denied", reason: "timeout" }, "delete") ?? "",
    /Nothing was moved to Trash/,
  );
  assert.match(
    decisionMessage({ status: "denied", reason: "timeout" }, "forward") ?? "",
    /Nothing was forwarded/,
  );
});
