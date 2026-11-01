import test from "node:test";
import assert from "node:assert/strict";
import {
  isQuerySaved,
  toggleSavedSearch,
  removeSavedSearch,
  SAVED_SEARCHES_KEY,
} from "../src/lib/savedSearches";
import { getPref, initPreferences, setPref } from "../src/lib/preferences";
import type { SavedSearch } from "../src/store/types";

// An in-memory host with the settings contract (revision, null removes): the preferences read the document once
// at boot and answer from memory, so a test boots them over this host.
async function bootPrefs() {
  let revision = 0;
  const values: Record<string, unknown> = {};
  const snapshot = () => ({ values: { ...values }, revision });
  await initPreferences({
    load: async () => snapshot(),
    patch: async (patch) => {
      for (const [k, v] of Object.entries(patch)) {
        if (v === null) delete values[k];
        else values[k] = v;
      }
      revision += 1;
      return snapshot();
    },
  });
}

test("save -> list -> run -> remove round trip persists as one preference key", async () => {
  await bootPrefs();

  // 1. save: no saved searches yet, save "from:boss is:unread".
  let list: SavedSearch[] = getPref(SAVED_SEARCHES_KEY);
  assert.deepEqual(list, []);
  list = toggleSavedSearch(list, "from:boss is:unread");
  setPref(SAVED_SEARCHES_KEY, list);

  // 2. list: re-reading from the settings store returns the saved entry.
  const reloaded = getPref(SAVED_SEARCHES_KEY);
  assert.deepEqual(reloaded, [{ name: "from:boss is:unread", query: "from:boss is:unread" }]);
  assert.equal(isQuerySaved(reloaded, "from:boss is:unread"), true);

  // 3. run: re-running is just reading the stored query string back out —
  // assert the persisted query is exactly what a click handler would re-run.
  assert.equal(reloaded[0].query, "from:boss is:unread");

  // 4. remove: delete it, persist, and confirm it's gone from a fresh read.
  const afterRemove = removeSavedSearch(reloaded, "from:boss is:unread");
  setPref(SAVED_SEARCHES_KEY, afterRemove);
  const finalList = getPref(SAVED_SEARCHES_KEY);
  assert.deepEqual(finalList, []);
  assert.equal(isQuerySaved(finalList, "from:boss is:unread"), false);
});

test("toggleSavedSearch is idempotent per query — saving twice then toggling once removes it", () => {
  let list: SavedSearch[] = [];
  list = toggleSavedSearch(list, "has:attachment");
  assert.equal(list.length, 1);
  list = toggleSavedSearch(list, "has:attachment"); // second call un-saves
  assert.deepEqual(list, []);
});

test("toggleSavedSearch no-ops on a blank/whitespace-only query", () => {
  const list: SavedSearch[] = [{ name: "x", query: "x" }];
  assert.deepEqual(toggleSavedSearch(list, ""), list);
  assert.deepEqual(toggleSavedSearch(list, "   "), list);
});

test("multiple saved searches persist and remove independently", async () => {
  await bootPrefs();
  let list: SavedSearch[] = [];
  list = toggleSavedSearch(list, "is:starred");
  list = toggleSavedSearch(list, "has:attachment after:2026/01/01");
  setPref(SAVED_SEARCHES_KEY, list);

  const reloaded = getPref(SAVED_SEARCHES_KEY);
  assert.equal(reloaded.length, 2);

  const afterRemoveOne = removeSavedSearch(reloaded, "is:starred");
  setPref(SAVED_SEARCHES_KEY, afterRemoveOne);
  const finalList = getPref(SAVED_SEARCHES_KEY);
  assert.deepEqual(finalList, [
    { name: "has:attachment after:2026/01/01", query: "has:attachment after:2026/01/01" },
  ]);
});
