import { describe, expect, it } from "vitest";
import { assertDocxWithinBudget, DocxBudgetError, DOCX_MAX_ENTRIES } from "./docxBudget";

describe("docx budget", () => {
  it("leaves non-ZIP input to the editor", () => {
    expect(() => assertDocxWithinBudget(new ArrayBuffer(10))).not.toThrow();
    expect(() => assertDocxWithinBudget(new TextEncoder().encode("not a zip at all, just text").buffer)).not.toThrow();
  });
  it("rejects a ZIP that declares too many entries", () => {
    const buf = new ArrayBuffer(22);
    const view = new DataView(buf);
    view.setUint32(0, 0x06054b50, true);
    view.setUint16(10, DOCX_MAX_ENTRIES + 1, true);
    expect(() => assertDocxWithinBudget(buf)).toThrow(DocxBudgetError);
  });
});
