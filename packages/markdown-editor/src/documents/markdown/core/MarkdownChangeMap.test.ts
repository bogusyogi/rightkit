import { describe, expect, it } from "vitest";
import { ChangeSet } from "@codemirror/state";
import { MarkdownChangeMap } from "./MarkdownChangeMap";

describe("MarkdownChangeMap — position mapping", () => {
  it("agrees with CodeMirror ChangeSet on LF-only randomized cases", () => {
    const random = mulberry32(1);
    const oldText = "abcdefghijklmnop";
    const insertText = randomInsert(random);
    const from = Math.floor(random() * oldText.length);
    const to = from + Math.floor(random() * (oldText.length - from));
    const changes = { from, to, insert: insertText };
    const map = new MarkdownChangeMap(ChangeSet.of(changes, oldText.length));
    const codeMirror = ChangeSet.of(changes, oldText.length);
    for (let pos = 0; pos <= oldText.length; pos++) {
      expect(map.mapPos(pos, -1)).toBe(codeMirror.mapPos(pos, -1));
      expect(map.mapPos(pos, 1)).toBe(codeMirror.mapPos(pos, 1));
    }
  });
});

function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function randomInsert(random: () => number): string {
  const len = 1 + Math.floor(random() * 5);
  let out = "";
  for (let i = 0; i < len; i++) out += String.fromCharCode(97 + Math.floor(random() * 26));
  return out;
}
