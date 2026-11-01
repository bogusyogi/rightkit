import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import DiffView, { looksLikeDiff, parseDiffRows } from "@rightkit/agent-ui-hooks/components/workbench/DiffView";
import * as diffModule from "@rightkit/agent-ui-hooks/components/workbench/DiffView";

describe("DiffView", () => {
  it("tracks old and new line numbers through a unified diff hunk", () => {
    expect(
      parseDiffRows(
        ["@@ -10,2 +10,3 @@", " context", "-old line", "+new line", "+extra line"].join("\n"),
      ),
    ).toMatchObject([
      { kind: "hunk", text: "@@ -10,2 +10,3 @@" },
      { kind: "ctx", oldN: 10, newN: 10, text: "context" },
      { kind: "del", oldN: 11, newN: null, text: "old line" },
      { kind: "add", oldN: null, newN: 11, text: "new line" },
      { kind: "add", oldN: null, newN: 12, text: "extra line" },
    ]);
  });

  it("renders additions and deletions as visible signed rows", () => {
    const html = renderToStaticMarkup(
      <DiffView text={["@@ -1,1 +1,1 @@", "-before", "+after"].join("\n")} />,
    );

    expect(looksLikeDiff("@@ -1,1 +1,1 @@\n-before\n+after")).toBe(true);
    expect(html).toContain("before");
    expect(html).toContain("after");
    expect(html).toContain("text-danger");
    expect(html).toContain("text-accent-2");
  });

  it("groups unified diff hunks with file headers for stage decisions", () => {
    const parseDiffHunks = (
      diffModule as {
        parseDiffHunks?: (text: string) => Array<{
          filePath: string | null;
          header: string;
          index: number;
          patch: string;
          rows: Array<{ kind: string; text: string }>;
        }>;
      }
    ).parseDiffHunks;

    expect(parseDiffHunks).toBeTypeOf("function");

    const hunks = parseDiffHunks?.(
      [
        "diff --git a/src/example.ts b/src/example.ts",
        "index 1111111..2222222 100644",
        "--- a/src/example.ts",
        "+++ b/src/example.ts",
        "@@ -10,2 +10,2 @@",
        " keep",
        "-old value",
        "+new value",
        "@@ -20,1 +20,2 @@",
        "-remove me",
        "+replace me",
        "+and add this",
      ].join("\n"),
    );

    expect(hunks).toHaveLength(2);
    expect(hunks?.[0]).toMatchObject({
      filePath: "src/example.ts",
      header: "@@ -10,2 +10,2 @@",
      index: 0,
      rows: [
        { kind: "hunk", text: "@@ -10,2 +10,2 @@" },
        { kind: "ctx", text: "keep" },
        { kind: "del", text: "old value" },
        { kind: "add", text: "new value" },
      ],
    });
    expect(hunks?.[0]?.patch).toBe(
      [
        "diff --git a/src/example.ts b/src/example.ts",
        "index 1111111..2222222 100644",
        "--- a/src/example.ts",
        "+++ b/src/example.ts",
        "@@ -10,2 +10,2 @@",
        " keep",
        "-old value",
        "+new value",
      ].join("\n"),
    );
  });

  it("builds a daemon hunk-stage payload from the selected hunk", () => {
    const buildStageDiffHunkPayload = (
      diffModule as {
        buildStageDiffHunkPayload?: (
          toolCallId: string,
          hunk: {
            id: string;
            index: number;
            filePath: string | null;
            header: string;
            patch: string;
          },
          decision: "approve" | "reject",
        ) => unknown;
      }
    ).buildStageDiffHunkPayload;

    expect(buildStageDiffHunkPayload).toBeTypeOf("function");
    expect(
      buildStageDiffHunkPayload?.(
        "tool-7",
        {
          id: "src/example.ts:0",
          index: 0,
          filePath: "src/example.ts",
          header: "@@ -10,2 +10,2 @@",
          patch: "@@ -10,2 +10,2 @@\n-old\n+new",
        },
        "approve",
      ),
    ).toEqual({
      toolCallId: "tool-7",
      decision: "approve",
      hunkId: "src/example.ts:0",
      hunkIndex: 0,
      filePath: "src/example.ts",
      hunkHeader: "@@ -10,2 +10,2 @@",
      patch: "@@ -10,2 +10,2 @@\n-old\n+new",
    });
  });

  it("keeps triple sign content lines inside a hunk", () => {
    expect(
      parseDiffRows(
        ["@@ -1,2 +1,2 @@", "----not a file header", "++++not a file header"].join("\n"),
      ),
    ).toMatchObject([
      { kind: "hunk", text: "@@ -1,2 +1,2 @@" },
      { kind: "del", text: "---not a file header" },
      { kind: "add", text: "+++not a file header" },
    ]);
  });

  it("renders a Copy button in the diff header", () => {
    const html = renderToStaticMarkup(
      <DiffView text={["@@ -1,1 +1,1 @@", "-before", "+after"].join("\n")} />,
    );

    expect(html).toContain('data-testid="diff-copy-button"');
    expect(html).toContain('aria-label="Copy diff"');
  });
});
