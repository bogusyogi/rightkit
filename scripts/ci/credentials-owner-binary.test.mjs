// Settled (Adrian, 2026-10-22, rightkit#884 closed as not planned): RightKit never widens who can
// read a keychain item. On macOS an item stays readable only by the binary that created it; an
// app's other binaries (engine, sidecar, helper, CLI) get secrets from that owner binary over the
// app's own IPC (rightkit#886; HeardRight's pattern). Team-wide or any-app keychain access would
// let every app signed by the same team read another app's secrets. This test fails if any such
// API appears in any RightKit crate; changing it needs Adrian's explicit decision, not a review.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const cratesDir = fileURLToPath(new URL("../../crates/", import.meta.url));

const FORBIDDEN = [
  ["SecAccessCreate", /\bSecAccessCreate\w*\b/],
  ["SecACLSetContents", /\bSecACLSetContents\b/],
  ["SecKeychainItemSetAccess", /\bSecKeychainItemSetAccess\w*\b/],
  ["SecTrustedApplicationCreate", /\bSecTrustedApplicationCreate\w*\b/],
  ["access group", /\bkSecAttrAccessGroup\b|keychain-access-groups/],
  ["shared-with-team option", /share[d]?_?with_?team/i],
];

// The synced-credentials Swift helper client lives inside one app's bundle and uses that app's
// own access group (iCloud Keychain); it is not cross-binary sharing of a local item.
const EXEMPT = new Set(["rightkit-credentials/src/apple_helper.rs: access group"]);

function sourceFiles(dir) {
  const out = [];
  for (const name of readdirSync(dir)) {
    if (name === "target" || name === "node_modules") continue;
    const full = path.join(dir, name);
    if (statSync(full).isDirectory()) out.push(...sourceFiles(full));
    else if (/\.(rs|swift|m|mm|h|c|plist|entitlements|toml)$/.test(name)) out.push(full);
  }
  return out;
}

test("RightKit keeps keychain items owner-binary-only (#884, settled)", () => {
  const hits = [];
  for (const file of sourceFiles(cratesDir)) {
    const text = readFileSync(file, "utf8");
    const rel = path.relative(cratesDir, file).replace(/\\/g, "/");
    for (const [label, pattern] of FORBIDDEN) {
      const hit = `${rel}: ${label}`;
      if (!EXEMPT.has(hit) && pattern.test(text)) hits.push(hit);
    }
  }
  assert.deepEqual(hits, [], "cross-binary keychain sharing is settled as never (rightkit#884); serve other binaries from the owner over IPC (rightkit#886)");
});
