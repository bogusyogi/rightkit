import { describe, expect, it } from "vitest";
import { assembleRenderJob, beginProgress, initialProgress, reduceFrame } from "./progress.js";

describe("engine progress reducer", () => {
  it("starts a run for one tag", () => {
    const s = beginProgress("t1", "a1");
    expect(s.running).toBe(true);
    expect(s.tag).toBe("t1");
    expect(initialProgress.running).toBe(false);
    expect(assembleRenderJob("a1")).toBe("assemble-a1-render");
  });
  it("ignores frames for another tag", () => {
    const s = beginProgress("t1", null);
    expect(reduceFrame(s, { id: 1, tag: "other", event: { event: "done" } })).toBe(s);
  });
});
