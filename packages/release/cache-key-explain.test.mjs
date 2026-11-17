import { test } from "node:test";
import assert from "node:assert/strict";

test("a row whose key ends in a suffix with colons is still read entry by entry: a changed member is a content change, not an unexplained miss", async () => {
  const { explainKeyChange } = await import("./deterministic-scans.mjs");
  // The shape of a real tsc row's key (Dell, 2026-11-17, tsc:rightkit-legal): JSON, then ":<environment digest>:tscws=<digest>".
  const key = (release, suffix = `${"7c".repeat(32)}:tscws=${"4e".repeat(32)}`) => `${JSON.stringify({ id: "tsc:rightkit-legal", kind: "npm", members: [["rightkit-ci", "packages/ci", "5d".repeat(32)], ["rightkit-release", "packages/release", release]], shared: [["package.json", "db".repeat(20)], ["biome.json", null]] })}:${suffix}`;
  const edited = explainKeyChange({ inputs: key("12".repeat(32)) }, { inputs: key("ab".repeat(32)) });
  assert.equal(edited.contentOnly, true, "only a member's content digest changed");
  assert.equal(edited.inputsChanged, 1);
  assert.equal(edited.inputs.includes("inputs"), false, "the key was read, not treated as one opaque value");
  // A changed suffix (the environment, or the workspace's member list) is not a content change and stays unexplained.
  const env = explainKeyChange({ inputs: key("12".repeat(32)) }, { inputs: key("12".repeat(32), `${"99".repeat(32)}:tscws=${"4e".repeat(32)}`) });
  assert.equal(env.contentOnly, false); assert.deepEqual(env.inputs, ["(key suffix)"]);
  // The one-part suffix of every other row still reads as before.
  const one = explainKeyChange({ inputs: `${JSON.stringify({ files: [["a.rs", "aa".repeat(20)]] })}:${"7c".repeat(32)}` }, { inputs: `${JSON.stringify({ files: [["a.rs", "bb".repeat(20)]] })}:${"7c".repeat(32)}` });
  assert.equal(one.contentOnly, true); assert.deepEqual(one.inputs, ["files/a.rs"]);
});
