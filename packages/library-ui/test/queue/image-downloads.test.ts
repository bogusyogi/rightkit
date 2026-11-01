import assert from "node:assert/strict";
import { test } from "node:test";
import {
  canSaveImages,
  imageCountLabel,
  imageCountForRow,
  imageJobMeta,
  imageSpecForInputs,
  isImageRow,
  isProfileUrl,
  needsAuthPlatform,
  outputLabelForJob,
  splitImageLines,
  supportsImageAction,
} from "../../src/queue/lib/mediaSelection.ts";

test("Save images is offered for Instagram, TikTok and X but not Threads or YouTube", () => {
  assert.equal(supportsImageAction("https://www.instagram.com/p/DZ123/"), true);
  assert.equal(supportsImageAction("https://www.tiktok.com/@someone/photo/7300"), true);
  assert.equal(supportsImageAction("https://x.com/someone/status/1"), true);
  assert.equal(supportsImageAction("https://twitter.com/someone/status/1"), true);
  assert.equal(supportsImageAction("https://www.threads.net/@someone/post/abc"), false);
  assert.equal(supportsImageAction("https://www.youtube.com/watch?v=abc"), false);
  assert.equal(supportsImageAction("C:\Users\me\photo.jpg"), false);
});

test("Save images needs every pasted line to be image-capable", () => {
  assert.equal(canSaveImages(""), false);
  assert.equal(canSaveImages("https://www.instagram.com/p/A/\nhttps://x.com/a/status/2"), true);
  assert.equal(canSaveImages("https://www.instagram.com/p/A/\nhttps://www.youtube.com/watch?v=b"), false);
  assert.deepEqual(imageSpecForInputs("https://x.com/a/status/2"), { action: "images" });
  assert.equal(imageSpecForInputs("https://www.youtube.com/watch?v=b"), undefined);
});

test("profile URLs are detected, posts and feeds are not", () => {
  assert.equal(isProfileUrl("https://www.instagram.com/someone/"), true);
  assert.equal(isProfileUrl("https://www.tiktok.com/@someone"), true);
  assert.equal(isProfileUrl("https://x.com/someone"), true);
  assert.equal(isProfileUrl("https://www.instagram.com/p/DZ123/"), false);
  assert.equal(isProfileUrl("https://www.instagram.com/reel/abc/"), false);
  assert.equal(isProfileUrl("https://x.com/someone/status/1"), false);
  assert.equal(isProfileUrl("https://www.tiktok.com/@someone/photo/7300"), false);
  assert.equal(isProfileUrl("https://www.threads.net/@someone"), false);
});

test("image count label reads 1 image, N images and 0 images", () => {
  assert.equal(imageCountLabel(0), "0 images");
  assert.equal(imageCountLabel(1), "1 image");
  assert.equal(imageCountLabel(3), "3 images");
});

test("image rows are recognised from media_action and media_meta, defensively", () => {
  const meta = '{"kind":"images","count":2,"bytes":10,"files":["/a.jpg","/b.jpg"]}';
  assert.equal(isImageRow({ media_action: "download_images", media_meta: null }), true);
  assert.equal(isImageRow({ media_action: null, media_meta: meta }), true);
  assert.equal(imageCountForRow({ media_meta: meta }), 2);
  assert.equal(imageJobMeta("not json"), null);
  assert.equal(imageJobMeta('{"kind":"video"}'), null);
  assert.equal(isImageRow({ media_action: "download_video", media_meta: null }), false);
  assert.equal(imageCountForRow({ media_meta: "{broken" }), 0);
});

test("Save images falls back per line: image links save, others transcribe", () => {
  const draft = [
    "https://www.instagram.com/p/DZ123/",
    "https://www.youtube.com/watch?v=abc",
    "https://x.com/someone/status/2",
  ].join("\n");
  assert.deepEqual(splitImageLines(draft), {
    image: ["https://www.instagram.com/p/DZ123/", "https://x.com/someone/status/2"],
    other: ["https://www.youtube.com/watch?v=abc"],
  });
  assert.deepEqual(splitImageLines("https://www.youtube.com/watch?v=abc"), {
    image: [],
    other: ["https://www.youtube.com/watch?v=abc"],
  });
  assert.deepEqual(splitImageLines(""), { image: [], other: [] });
});

test("needs_auth errors carry the platform for the sign-in prompt", () => {
  assert.equal(needsAuthPlatform("[needs_auth] Instagram"), "Instagram");
  assert.equal(needsAuthPlatform("[needs_auth] TikTok"), "TikTok");
  assert.equal(needsAuthPlatform("[profile_busy] a listing is running"), null);
  assert.equal(needsAuthPlatform("[needs_auth] "), null);
});

test("a queued images job is labelled Images", () => {
  assert.equal(outputLabelForJob({ media: { action: "images" } }), "Images");
});
