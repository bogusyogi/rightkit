import { describe, expect, it } from "vitest";
import { policyGateDocuments } from "./policyDocuments";

describe("policyGateDocuments", () => {
  it("maps rendered defaults to inline markdown gate documents", () => {
    const docs = policyGateDocuments([{ title: "App Terms", content: "# Terms\n" }]);
    expect(docs).toEqual([{ title: "App Terms", content: "# Terms\n", contentFormat: "markdown" }]);
  });

  it("adds a url only when one is supplied for the title", () => {
    const docs = policyGateDocuments(
      [{ title: "A", content: "a" }, { title: "B", content: "b" }],
      { A: "https://example.app/a/" },
    );
    expect(docs[0]?.url).toBe("https://example.app/a/");
    expect("url" in (docs[1] ?? {})).toBe(false);
  });
});
