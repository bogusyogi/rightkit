import test from "node:test";
import assert from "node:assert/strict";
import { searchCoverageBannerText } from "../src/lib/searchCoverage";

test("search-coverage banner shows counts and progress state when the archive is incomplete", () => {
  const text = searchCoverageBannerText({
    total: 500,
    with_body: 120,
    active: true,
    full_archive: false,
  });
  assert.match(text!, /Searched the local archive/);
  assert.match(text!, /120 of 500 messages downloaded/);
  assert.match(text!, /download in progress/);
  assert.match(text!, /Older mail may not appear yet\./);
});

test("search-coverage banner omits the in-progress clause when the download is paused", () => {
  const text = searchCoverageBannerText({
    total: 500,
    with_body: 120,
    active: false,
    full_archive: false,
  });
  assert.match(text!, /120 of 500 messages downloaded\. Older mail/);
  assert.doesNotMatch(text!, /download in progress/);
});

test("search-coverage banner renders nothing once the archive is complete", () => {
  assert.equal(
    searchCoverageBannerText({
      total: 500,
      with_body: 500,
      active: false,
      full_archive: true,
    }),
    null,
  );
});

test("search-coverage banner renders nothing when progress hasn't loaded yet", () => {
  assert.equal(searchCoverageBannerText(null), null);
});
