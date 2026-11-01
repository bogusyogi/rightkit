// @vitest-environment node
import { describe, expect, it } from "vitest";
import { HtmlMutationError, commitHtmlMutation } from "../htmlMutationEngine";
import { deleteNodePlan, moveSiblingPlan } from "../htmlOperationPlanner";
import { sourceChecksum } from "../sourceMap";
import { buildHtmlBlockIndex, findBlockIndexViolations } from "./htmlBlockIndex";

const FIXTURE = [
  "<!doctype html><html><head><title>t</title></head><body>",
  '<h1 id="a">Title</h1>',
  "<p>One <b>bold</b> line</p>",
  '<script>window.keep = "<b>raw</b>";</script>',
  "<ul><li>x</li><li>y</li></ul>",
  '<svg width="10" height="10"><circle r="4"/></svg>',
  "<x-widget data-k=\"1\">custom <i>inner</i></x-widget>",
  "<section><p>in group</p><style>.k{color:red}</style><hr></section>",
  "Loose text between blocks",
  "<table><div>fostered</div><tr><td>cell</td></tr></table>",
  "<blockquote><p>quoted</p></blockquote>",
  '<form action="/x"><input name="n"></form>',
  '<figure><img src="a.png" alt=""><figcaption>cap</figcaption></figure>',
  "<pre><code>a  b</code></pre>",
  "<p>Last</p>",
  "</body></html>",
].join("\n");

describe("HTML block index", () => {
  it("classifies every body element, keeps ranges disjoint, and preserves locked bytes through random move/delete commits", () => {
    const index = buildHtmlBlockIndex(FIXTURE);
    expect(findBlockIndexViolations(index)).toEqual([]);
    expect(index.unplaced).toEqual([]);
    const reasons = new Set(index.flat.map((b) => b.locked).filter(Boolean));
    for (const reason of ["source-only-tag", "custom-element", "mixed-inline", "unmapped"])
      expect(reasons).toContain(reason);
    expect(index.flat.filter((b) => !b.locked).every((b) => b.kind !== "raw")).toBe(true);

    const lockedTexts = (source: string) => {
      const idx = buildHtmlBlockIndex(source);
      return idx.lockedRanges.map((r) => source.slice(r.start, r.end)).sort();
    };
    let seed = 20261017;
    const next = (n: number) => {
      seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
      return seed % n;
    };
    let source = FIXTURE;
    let committed = 0;
    for (let i = 0; i < 80; i++) {
      const idx = buildHtmlBlockIndex(source);
      const targets = idx.blocks.filter((b) => b.origin === "element");
      const target = targets[next(targets.length)];
      const revision = sourceChecksum(source);
      const deleting = !target.locked && next(3) === 0;
      const before = lockedTexts(source);
      let result: ReturnType<typeof commitHtmlMutation>;
      let plan: ReturnType<typeof deleteNodePlan> | null = null;
      try {
        plan = deleting
          ? deleteNodePlan(source, target.range, revision)
          : moveSiblingPlan(source, target.range, next(2) === 0 ? "up" : "down", revision);
        result = commitHtmlMutation(source, plan, { lockedRanges: idx.lockedRanges });
      } catch (error) {
        // Planner/engine refusals are fail-closed and fine. A locked-violation refusal is correct
        // only when the plan really endangers locked bytes: a patch that cuts into a locked range
        // (e.g. a move unit that splits a foster-parented cluster) or a deletion that would remove a
        // locked range nested inside the deleted typed block. Any other locked-violation is a defect.
        if (error instanceof HtmlMutationError && error.code === "locked-violation") {
          const endangers = (plan?.patches ?? []).some((p) =>
            idx.lockedRanges.some((l) => {
              const intersects = p.range.start < l.end && l.start < p.range.end;
              if (!intersects) return false;
              const wholly = p.range.start <= l.start && p.range.end >= l.end;
              return !wholly || (p.text === "" && deleting);
            }),
          );
          expect(endangers).toBe(true);
        }
        continue;
      }
      committed += 1;
      expect(findBlockIndexViolations(buildHtmlBlockIndex(result.source))).toEqual([]);
      const after = lockedTexts(result.source);
      if (!deleting) expect(after).toEqual(before);
      else {
        // Deleting a typed block never changes a locked block's bytes.
        for (const text of before) expect(after).toContain(text);
      }
      source = result.source;
    }
    expect(committed).toBeGreaterThan(0);

    // An engine plan that rewrites bytes inside a locked block is refused unless exempted.
    const base = buildHtmlBlockIndex(FIXTURE);
    const script = base.flat.find((b) => b.tagName === "script")!;
    const at = FIXTURE.indexOf("keep", script.range.start);
    const bad = {
      operation: "bad",
      expectedRevision: sourceChecksum(FIXTURE),
      mutationBoundary: script.range,
      impactBoundary: script.range,
      patches: [{ range: { start: at, end: at + 4 }, expectedText: "keep", text: "KEEP" }],
    };
    expect(() => commitHtmlMutation(FIXTURE, bad, { lockedRanges: base.lockedRanges })).toThrowError(
      expect.objectContaining({ code: "locked-violation" }),
    );
    expect(
      commitHtmlMutation(FIXTURE, bad, {
        lockedRanges: base.lockedRanges,
        lockedExempt: [script.range],
      }).source,
    ).toContain("KEEP");
  });
});
