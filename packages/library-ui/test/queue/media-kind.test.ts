import assert from "node:assert/strict";
import { test } from "node:test";
import { savedMediaKind } from "../../src/queue/lib/mediaSelection";

test("a top-level audio kind in media_meta reads as audio", () => {
  assert.equal(savedMediaKind('{"bytes":1,"kind":"audio","quality":"best"}', "/x/a.m4a"), "audio");
});

test("a video container extension wins over an audio label", () => {
  assert.equal(savedMediaKind('{"kind":"audio"}', "/x/a.mp4"), "video");
});

test("malformed or missing meta falls back to video", () => {
  assert.equal(savedMediaKind("not json \"kind\":\"audio\"", "/x/a.m4a"), "video");
  assert.equal(savedMediaKind(null, "/x/a.m4a"), "video");
  assert.equal(savedMediaKind(undefined, undefined), "video");
});

test("a nested kind does not count, only the top-level one", () => {
  assert.equal(savedMediaKind('{"meta":{"kind":"audio"}}', "/x/a.m4a"), "video");
});

test("the picker and the pasted-input checks share one extension table", async () => {
  const { MEDIA_IMAGE_PDF_FILTERS } = await import("../../src/queue/lib/fileFilters");
  const picker = MEDIA_IMAGE_PDF_FILTERS[0].extensions;
  for (const ext of ["ogg", "opus", "avi", "aac", "wma", "tif", "pdf"]) {
    assert.ok(picker.includes(ext), `picker is missing ${ext}`);
  }
  const { mediaKindForInput } = await import("../../src/queue/lib/mediaSelection");
  assert.equal(mediaKindForInput("https://cdn.example.com/a.opus").kind, "audio");
  assert.equal(mediaKindForInput("https://cdn.example.com/a.avi").kind, "video");
});
