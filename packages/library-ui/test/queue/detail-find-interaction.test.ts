import assert from "node:assert/strict";
import { test } from "node:test";
import {
  countEntryMatches,
  countMatches,
  FIND_HIGHLIGHT_CAP,
  findSegments,
  findTextForMarkdown,
  normalizeFindQuery,
  parseSrt,
  srtTimeToSeconds,
  stampToSeconds,
  transcriptEntries,
} from "../../src/queue/views/detailFind";

test("find ignores queries shorter than two characters", () => {
  assert.equal(normalizeFindQuery(""), "");
  assert.equal(normalizeFindQuery("  a  "), "");
  assert.equal(normalizeFindQuery(" ab "), "ab");
  assert.equal(countMatches("a a a a", "a"), 0);
  assert.deepEqual(findSegments("a a", "a"), []);
});

test("count is case-insensitive and non-overlapping", () => {
  assert.equal(countMatches("Hello hello HELLO", "hello"), 3);
  assert.equal(countMatches("aaaa", "aa"), 2);
  assert.equal(countMatches("nothing here", "zebra"), 0);
});

test("segments reproduce the text losslessly with sequential match indexes", () => {
  const text = "one Foo two foo three";
  const segs = findSegments(text, "FOO");
  assert.equal(segs.map((s) => s.value).join(""), text);
  const matches = segs.filter((s) => s.type === "match");
  assert.deepEqual(
    matches.map((m) => (m.type === "match" ? m.index : -1)),
    [0, 1],
  );
  assert.equal(matches.length, countMatches(text, "foo"));
});

test("highlight cap keeps the text intact and stops marking past the cap", () => {
  const text = Array.from({ length: 5 }, () => "word").join(" ");
  const segs = findSegments(text, "word", 2);
  assert.equal(segs.filter((s) => s.type === "match").length, 2);
  assert.equal(segs.map((s) => s.value).join(""), text);
  assert.ok(FIND_HIGHLIGHT_CAP >= 1000, "default cap stays large enough for real transcripts");
});

test("markdown find counts the stripped text the tab renders", () => {
  const md = "# Title\n\n**Hello** world, hello again";
  // The raw markdown has a `**` marker in the way; the stripped text does not.
  assert.equal(countMatches(findTextForMarkdown(md), "hello"), 2);
  assert.equal(findTextForMarkdown(md).includes("**"), false);
});

test("timestamped entries count matches per entry and never count the stamps", () => {
  const detail = {
    text: "",
    timed_text: "[00:12] hello there\n[01:05] say hello\nplain hello line",
  };
  const entries = transcriptEntries(detail, true);
  assert.equal(entries.length, 3);
  assert.equal(entries[0]?.seconds, 12);
  assert.equal(countEntryMatches(entries, "hello"), 3);
  // "12" appears in the stamp "[00:12]" but not in any rendered entry text.
  assert.equal(countEntryMatches(entries, "12"), 0);
});

test("untimed view splits on blank lines", () => {
  const entries = transcriptEntries({ text: "first block\n\nsecond block\n\n", timed_text: null }, false);
  assert.deepEqual(
    entries.map((e) => e.text),
    ["first block", "second block"],
  );
});

test("stamps and SRT timecodes convert to seconds", () => {
  assert.equal(stampToSeconds("01:02"), 62);
  assert.equal(stampToSeconds("1:00:05"), 3605);
  assert.equal(stampToSeconds("bad"), null);
  assert.equal(srtTimeToSeconds("00:00:01,500"), 1.5);
  assert.equal(srtTimeToSeconds("00:01:00.250"), 60.25);
});

test("SRT parsing yields timed cues and skips malformed blocks", () => {
  const srt = [
    "1",
    "00:00:01,000 --> 00:00:03,000",
    "Hello",
    "there",
    "",
    "not a cue",
    "",
    "2",
    "00:00:04,000 --> 00:00:05,000",
    "Bye",
  ].join("\n");
  const cues = parseSrt(srt);
  assert.equal(cues.length, 2);
  assert.equal(cues[0]?.startS, 1);
  assert.equal(cues[0]?.endS, 3);
  assert.equal(cues[0]?.text, "Hello there");
  assert.equal(cues[1]?.text, "Bye");
});
