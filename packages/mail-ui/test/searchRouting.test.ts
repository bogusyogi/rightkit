import test from "node:test";
import assert from "node:assert/strict";
import {
  searchAccountId,
  visibleThreadMatches,
} from "../src/lib/searchRouting";

test("visibleThreadMatches keeps obvious loaded rows available without a DB search", () => {
  const rows = [
    {
      from_name: "Example Co Website",
      subject: "Contact Form: SHIPPING",
      snippet: "New message Example Co",
      participants: ["forms@example.com"],
    },
    {
      from_name: "Google Maps",
      subject: "Your appeal was approved",
      snippet: "Account restrictions have been removed",
      participants: [],
    },
  ];

  assert.deepEqual(visibleThreadMatches(rows, "example"), [rows[0]]);
  assert.deepEqual(visibleThreadMatches(rows, "appeal"), [rows[1]]);
  assert.deepEqual(visibleThreadMatches(rows, "  "), []);
});

test("searchAccountId searches every linked account only for All inboxes", () => {
  assert.equal(searchAccountId("all", "me@example.com", null), null);
  assert.equal(searchAccountId("inbox", "me@example.com", null), "me@example.com");
  assert.equal(searchAccountId("sent", "me@example.com", null), "me@example.com");
  assert.equal(searchAccountId("all", "me@example.com", "Label_123"), "me@example.com");
});
