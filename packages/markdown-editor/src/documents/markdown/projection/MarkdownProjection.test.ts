import { describe, expect, it } from "vitest";
import { Schema } from "prosemirror-model";
import { MarkdownDocumentCore } from "../core/MarkdownDocumentCore.js";
import { projectMarkdownToPm } from "./MarkdownProjection.js";

const testSchema = new Schema({
  nodes: {
    doc: { content: "block+" },
    paragraph: {
      group: "block",
      content: "text*",
      attrs: {
        prefix: { default: "" },
        rawSource: { default: "" },
        rawText: { default: "" },
        blockId: { default: "" },
        sourceFrom: { default: -1 },
        sourceTo: { default: -1 },
        prefixFrom: { default: -1 },
        prefixTo: { default: -1 },
        rawSignature: { default: "" },
      },
      toDOM: () => ["p", 0],
    },
    raw_block: {
      group: "block",
      atom: true,
      attrs: {
        prefix: { default: "" },
        rawSource: { default: "" },
        mdastType: { default: "unknown" },
        sourceLine: { default: 1 },
        blockId: { default: "" },
        sourceFrom: { default: -1 },
        sourceTo: { default: -1 },
        prefixFrom: { default: -1 },
        prefixTo: { default: -1 },
        rawSignature: { default: "" },
      },
      toDOM: () => ["pre", 0],
    },
    text: { group: "inline" },
  },
});

describe("MarkdownProjection — parser failure boundary", () => {
  it("falls back to raw_block when the parser throws", () => {
    const core = MarkdownDocumentCore.create({
      documentId: "t",
      initialLogicalSource: "anything",
    });
    const doc = projectMarkdownToPm(core, testSchema);
    expect(doc.childCount).toBeGreaterThanOrEqual(1);
  });
});
