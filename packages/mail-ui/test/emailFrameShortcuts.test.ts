import test, { before } from "node:test";
import assert from "node:assert/strict";
import { initPreferences } from "../src/lib/preferences";

// The store reads its preferences at creation, so they are loaded (empty, in memory) before
// the module under test is imported.
let createEmailFrameShortcuts: typeof import("../src/lib/emailFrameShortcuts").createEmailFrameShortcuts;
before(async () => {
  await initPreferences({
    load: async () => ({ values: {}, revision: 0 }),
    patch: async () => ({ values: {}, revision: 1 }),
  });
  ({ createEmailFrameShortcuts } = await import("../src/lib/emailFrameShortcuts"));
});

// The registry attached to an email's iframe document must hold Find and nothing
// else: bare keys such as e, # and ! would otherwise act on mail from inside email
// content. No DOM here, so events are plain objects.
const target = { tagName: "BODY", closest: () => null };
const ev = (key: string, ctrlKey = false, metaKey = false, shiftKey = false) =>
  ({ key, code: "", ctrlKey, metaKey, altKey: false, shiftKey, target, isComposing: false, defaultPrevented: false, preventDefault() {} }) as unknown as KeyboardEvent;

test("the iframe registry holds only the find shortcut", () => {
  const registry = createEmailFrameShortcuts(() => {}, () => true);
  assert.deepEqual(registry.list().map((s) => s.id), ["findInPage"]);
});

test("archive, trash, spam and other bare keys do nothing inside the iframe", () => {
  let opened = 0;
  const registry = createEmailFrameShortcuts(() => void opened++, () => true);
  for (const key of ["e", "#", "!", "s", "j", "k", "r", "f", "?", "/"]) {
    assert.equal(registry.handle(ev(key)), false, key);
  }
  assert.equal(opened, 0);
});

test("Ctrl+F and Cmd+F open find; a blocked guard does not", () => {
  let opened = 0;
  let allowed = true;
  const registry = createEmailFrameShortcuts(() => void opened++, () => allowed);
  assert.equal(registry.handle(ev("f", true)), true);
  assert.equal(registry.handle(ev("f", false, true)), true);
  assert.equal(opened, 2);
  allowed = false;
  assert.equal(registry.handle(ev("f", true)), false);
  assert.equal(opened, 2);
});
