import { describe, expect, it } from "vitest";
import {
  buildPasteToken,
  droppedFileInlineRef,
  expandPasteTokens,
  extractDroppedFiles,
  insertAtSelection,
  parsePasteTokenSeqs,
  partitionDroppedFiles,
  singlePasteTokenRe,
  toDaemonAttachments,
  type DroppedFile,
  type StagedAttachment,
} from "./useAttachmentStaging.js";

describe("attachment staging", () => {
  it("preserves OpenCovibe's anchored paste token at the selected position", () => {
    const first = buildPasteToken("Pasted #1 · 120 lines");
    const second = buildPasteToken("Pasted #2 · 4 lines");
    expect(insertAtSelection("before after", 7, 7, first)).toBe(`before ${first} after`);
    expect(parsePasteTokenSeqs(`${first} ${second} ${first}`)).toEqual([1, 2, 1]);
    expect(singlePasteTokenRe(1).test(first)).toBe(true);
    expect(singlePasteTokenRe(1).test(second)).toBe(false);
    expect(singlePasteTokenRe(1).test(buildPasteToken("Pasted #10 · 120 lines"))).toBe(false);
    expect(insertAtSelection("abcd", 1, 3, "XY")).toBe("aXYd");
    expect(insertAtSelection("abc", 3, 3, "Z")).toBe("abcZ");
    expect(expandPasteTokens(`x ${first} y`, [{ seq: 1, text: "FULL" }])).toEqual({
      text: "x FULL y",
      usedSeqs: new Set([1]),
    });
    expect(expandPasteTokens(`${second} then ${first}`, [
      { seq: 1, text: "FULL-A" },
      { seq: 2, text: "FULL-B" },
    ])).toEqual({ text: "FULL-B then FULL-A", usedSeqs: new Set([1, 2]) });
    expect(expandPasteTokens(`x ${buildPasteToken("Pasted #9 · missing")}`, [])).toEqual({
      text: "x ",
      usedSeqs: new Set(),
    });
    expect(parsePasteTokenSeqs("plain text")).toEqual([]);
  });

  it("keeps Hermes' origin partition and line refs", () => {
    const os: DroppedFile = { file: new File(["x"], "note.txt"), path: "/tmp/note.txt" };
    const inApp: DroppedFile = { path: "src/app.ts", line: 10, lineEnd: 20 };
    expect(partitionDroppedFiles([os, inApp])).toEqual({ osDrops: [os], inAppRefs: [inApp] });
    expect(droppedFileInlineRef(inApp, "/workspace")).toBe("@line:src/app.ts:10-20");
  });

  it("extracts a native file once when browsers expose it through items and files", () => {
    const file = new File(["x"], "note.txt", { type: "text/plain" });
    const transfer = {
      getData: () => "",
      items: [{ kind: "file", getAsFile: () => file }],
      files: { length: 1, item: () => file },
    } as unknown as DataTransfer;
    const result = extractDroppedFiles(transfer);
    expect(result).toHaveLength(1);
    expect(result[0]?.file).toBe(file);
    expect(partitionDroppedFiles(result).osDrops).toHaveLength(1);
  });

  it("maps only ready image bytes to the existing daemon Attachment wire shape", () => {
    const item: StagedAttachment = {
      id: "image-1",
      name: "shot.png",
      size: 4,
      mimeType: "image/png",
      kind: "image",
      state: "ready",
      data: "AQIDBA==",
      origin: "local",
    };
    expect(toDaemonAttachments([item])).toEqual([{
      type: "image",
      data: "AQIDBA==",
      mimeType: "image/png",
      id: "image-1",
      filename: "shot.png",
      sizeBytes: 4,
    }]);
    expect(toDaemonAttachments([{ ...item, state: "lost" }])).toEqual([]);
  });
});
