import assert from "node:assert/strict";
import test from "node:test";
import {
  MAX_DATA_IMAGE_B64_CHARS,
  emailDomain,
  isSafeDataImageSrc,
  remoteImageSrc,
  resolveImageRule,
  trackerNoteFor,
} from "../src/lib/imageTrust.ts";

function b64Binary(s: string): string {
  return Buffer.from(s, "binary").toString("base64");
}

test("safe data images require an allowed MIME and matching magic bytes", () => {
  assert.equal(
    isSafeDataImageSrc(`data:image/png;base64,${b64Binary("\x89PNG\r\n\x1a\n")}`),
    true
  );
  assert.equal(isSafeDataImageSrc(`data:image/png;base64,${b64Binary("GIF89a")}`), false);
  assert.equal(isSafeDataImageSrc(`data:image/svg+xml;base64,${b64Binary("<svg></svg>")}`), false);
  assert.equal(isSafeDataImageSrc(`data:image/png;base64,${"A".repeat(MAX_DATA_IMAGE_B64_CHARS + 1)}`), false);
});

test("sender image rules override domain rules and normalize sender keys", () => {
  assert.equal(emailDomain("Alice <Alice@Example.COM>,"), "example.com");
  assert.equal(
    resolveImageRule(
      "Alice <Alice@Example.COM>",
      { "alice <alice@example.com>": "block" },
      { "example.com": "show" }
    ),
    "block"
  );
  assert.equal(resolveImageRule("news@example.com", {}, { "example.com": "show" }), "show");
  assert.equal(resolveImageRule("news@example.com", {}, {}), null);
});

test("remote image src keeps QA data URLs inline and hashes on the app protocol", () => {
  const dataUrl = "data:image/png;base64,AAAA";
  assert.equal(remoteImageSrc(dataUrl), dataUrl);
  assert.match(remoteImageSrc("qa-remote-image-hash"), /mailright-img/i);
});

test("tracker-blocked note renders only when images are on and a message has a positive count", () => {
  // Images off — never show the note, regardless of count.
  assert.equal(trackerNoteFor(false, 3), null);
  // Images on, no blocks yet (undefined or zero) — nothing to show.
  assert.equal(trackerNoteFor(true, undefined), null);
  assert.equal(trackerNoteFor(true, 0), null);
  // Images on, one block — singular wording.
  assert.equal(trackerNoteFor(true, 1), "1 tracker blocked");
  // Images on, multiple blocks — plural wording.
  assert.equal(trackerNoteFor(true, 4), "4 trackers blocked");
});
