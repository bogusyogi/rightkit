import test from "node:test";
import assert from "node:assert/strict";
import {
  SNIPPET_MATCH_START,
  SNIPPET_MATCH_END,
  splitHighlightSentinels,
} from "../src/lib/highlightSnippet";

// L11's snippet() output wraps match spans in / sentinels
// (src-tauri/src/db/search/snippet.rs). splitHighlightSentinels is the ONLY
// place that parses them — this asserts the split is correct AND that no
// sentinel control character ever survives into a rendered segment, which is
// the actual security/rendering property the C6 spec requires (no raw
// control chars in output, no dangerouslySetInnerHTML).

test("plain text with no sentinels returns a single unmatched segment", () => {
  const segs = splitHighlightSentinels("hello world");
  assert.deepEqual(segs, [{ text: "hello world", matched: false }]);
});

test("a single match span splits into before/matched/after segments", () => {
  const raw = `see ${SNIPPET_MATCH_START}invoice${SNIPPET_MATCH_END} attached`;
  const segs = splitHighlightSentinels(raw);
  assert.deepEqual(segs, [
    { text: "see ", matched: false },
    { text: "invoice", matched: true },
    { text: " attached", matched: false },
  ]);
});

test("multiple match spans in one snippet all get marked", () => {
  const raw = `${SNIPPET_MATCH_START}foo${SNIPPET_MATCH_END} bar ${SNIPPET_MATCH_START}baz${SNIPPET_MATCH_END}`;
  const segs = splitHighlightSentinels(raw);
  assert.deepEqual(segs, [
    { text: "foo", matched: true },
    { text: " bar ", matched: false },
    { text: "baz", matched: true },
  ]);
});

test("empty string returns no segments", () => {
  assert.deepEqual(splitHighlightSentinels(""), []);
});

test("no segment ever contains a raw sentinel byte, for any output", () => {
  const cases = [
    `${SNIPPET_MATCH_START}x${SNIPPET_MATCH_END}`,
    `${SNIPPET_MATCH_START}unterminated`,
    `stray${SNIPPET_MATCH_END}end`,
    `${SNIPPET_MATCH_START}${SNIPPET_MATCH_START}nested${SNIPPET_MATCH_END}`,
    `a${SNIPPET_MATCH_START}b${SNIPPET_MATCH_START}c${SNIPPET_MATCH_END}d`,
  ];
  for (const raw of cases) {
    for (const seg of splitHighlightSentinels(raw)) {
      assert.ok(!seg.text.includes(SNIPPET_MATCH_START) && !seg.text.includes(SNIPPET_MATCH_END), `segment leaked a sentinel byte: ${JSON.stringify(seg)}`);
    }
  }
});

test("an unterminated start sentinel degrades to unmatched text, not a crash", () => {
  const segs = splitHighlightSentinels(`before ${SNIPPET_MATCH_START}dangling`);
  assert.deepEqual(segs, [
    { text: "before ", matched: false },
    { text: "dangling", matched: false },
  ]);
});

test("a stray end sentinel with no preceding start degrades to unmatched text", () => {
  const segs = splitHighlightSentinels(`stray${SNIPPET_MATCH_END}end`);
  assert.deepEqual(segs, [{ text: "strayend", matched: false }]);
});
