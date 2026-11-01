import test from "node:test";
import assert from "node:assert/strict";
import {
  SEARCH_PAGE_SIZE,
  appendPage,
  hasMorePages,
  nextSearchCursor,
} from "../src/lib/searchPaging";

const row = (i: number) => ({
  account_id: "a",
  thread_id: `t${i}`,
  last_message_at: 1000 - i,
});

test("a short page is the last page and yields no cursor", () => {
  const page = Array.from({ length: SEARCH_PAGE_SIZE - 1 }, (_, i) => row(i));
  assert.equal(hasMorePages(page), false);
  assert.equal(nextSearchCursor(page), null);
  assert.equal(nextSearchCursor([]), null);
});

test("a full page yields a cursor at its last row", () => {
  const page = Array.from({ length: SEARCH_PAGE_SIZE }, (_, i) => row(i));
  assert.equal(hasMorePages(page), true);
  assert.deepEqual(nextSearchCursor(page), {
    last_message_at: 1000 - (SEARCH_PAGE_SIZE - 1),
    account_id: "a",
    thread_id: `t${SEARCH_PAGE_SIZE - 1}`,
  });
});

test("appendPage skips threads that are already listed", () => {
  const out = appendPage([row(0), row(1)], [row(1), row(2)]);
  assert.deepEqual(
    out.map((t) => t.thread_id),
    ["t0", "t1", "t2"],
  );
});
