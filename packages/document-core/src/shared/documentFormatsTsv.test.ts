import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import documentFormatsTsv from "./documentFormatsTsv.js";

describe("documentFormatsTsv", () => {
  it("is byte-equal to document_formats.tsv, the table the Rust side reads", () => {
    expect(documentFormatsTsv).toBe(readFileSync(new URL("./document_formats.tsv", import.meta.url), "utf8"));
  });
});
