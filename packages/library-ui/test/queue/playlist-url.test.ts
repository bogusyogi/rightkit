import assert from "node:assert/strict";
import { test } from "node:test";
import { isPlaylistUrl, playlistQueuedNotice } from "../../src/queue/lib/playlistUrl";

test("playlist and channel links are detected", () => {
  for (const url of [
    "https://www.youtube.com/playlist?list=PL123",
    "https://www.youtube.com/@someone",
    "https://youtube.com/@someone/videos",
    "https://m.youtube.com/channel/UCabc123",
    "https://www.youtube.com/c/Name/shorts",
    "https://vimeo.com/showcase/123456",
    "https://soundcloud.com/artist/sets/album",
    "https://www.dailymotion.com/playlist/x7abc",
  ]) {
    assert.equal(isPlaylistUrl(url), true, url);
  }
});

test("single videos and other pages are not playlists", () => {
  for (const url of [
    "https://www.youtube.com/watch?v=abc&list=PL123",
    "https://youtu.be/abc?list=PL123",
    "https://www.youtube.com/shorts/abc",
    "https://www.youtube.com/playlist",
    "https://www.youtube.com/@someone/community",
    "https://example.com/playlist?list=1",
    "https://vimeo.com/123456",
    "C:\\clips\\a.mp4",
    "",
  ]) {
    assert.equal(isPlaylistUrl(url), false, url);
  }
});

test("the queued notice counts videos", () => {
  assert.equal(playlistQueuedNotice({ accepted: 12, rejected: [] }), "Queued 12 videos");
  assert.equal(playlistQueuedNotice({ accepted: 1, rejected: [] }), "Queued 1 video");
  assert.equal(playlistQueuedNotice({ accepted: 3, rejected: ["full"] }), "Queued 3 videos (1 skipped)");
});
