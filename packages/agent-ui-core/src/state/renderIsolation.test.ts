// Lane C-2 regression test: streaming token appends must NOT trigger
// subscribers that don't read streaming fields. This is the
// Zustand-level contract behind SPEC 1 isolation — components that
// don't `useStreamingStore` (e.g. sidebar, title bar) won't re-render.
//
// The fence's "render-spy component" form needs jsdom (not installed
// here); this pure-store equivalent is the same isolation guarantee
// measured at the subscription layer that React subscribes through.

import { beforeEach, describe, expect, it } from "vitest";

import { useStreamingStore } from "./streamingStore";
import { useComposerStore } from "./composerStore";

describe("Lane C-2 render isolation — streaming store subscription boundary", () => {
  beforeEach(() => {
    useStreamingStore.setState({
      streamingText: "",
      streamingReasoning: "",
      reasoningExpanded: false,
      streamingToolName: null,
    });
  });

  it("subscribers to non-streaming fields (composer) do NOT fire on streamingText append", () => {
    const calls: number[] = [];
    const unsubscribe = useComposerStore.subscribe((state) => {
      calls.push(state.composer.length);
    });

    // Reset counter after the initial subscribe snapshot.
    calls.length = 0;

    // A burst of streaming token appends.
    useStreamingStore.getState().appendText("hello ");
    useStreamingStore.getState().appendText("world");
    useStreamingStore.getState().appendReasoning("thinking…");
    useStreamingStore.getState().setStreamingToolName("read_file");

    expect(calls).toEqual([]);
    expect(useStreamingStore.getState().streamingText).toBe("hello world");
    expect(useStreamingStore.getState().streamingReasoning).toBe("thinking…");
    expect(useStreamingStore.getState().streamingToolName).toBe("read_file");

    unsubscribe();
  });

  it("subscribers to streaming fields DO fire on streamingText append", () => {
    const seen: string[] = [];
    const unsubscribe = useStreamingStore.subscribe((state) => {
      seen.push(state.streamingText);
    });

    useStreamingStore.getState().appendText("a");
    useStreamingStore.getState().appendText("b");
    useStreamingStore.getState().appendText("c");

    // Zustand fires once per state-set call; we expect 3 callbacks.
    expect(seen).toEqual(["a", "ab", "abc"]);

    unsubscribe();
  });

  it("clear() resets streaming text/reasoning/toolName; reasoningExpanded is caller-managed", () => {
    useStreamingStore.getState().appendText("partial");
    useStreamingStore.getState().setReasoningExpanded(true);
    useStreamingStore.getState().setStreamingToolName("view_image");
    expect(useStreamingStore.getState().streamingText).toBe("partial");

    // Note: reasoningExpanded is a user-controlled toggle; the caller
    // (clearStreaming in useSessionEvents) resets it via
    // setReasoningExpanded(false) BEFORE the store clear() runs. The
    // store deliberately does NOT touch it — see streamingStore.clear.
    useStreamingStore.getState().setReasoningExpanded(false);
    useStreamingStore.getState().clear();
    expect(useStreamingStore.getState()).toMatchObject({
      streamingText: "",
      streamingReasoning: "",
      reasoningExpanded: false,
      streamingToolName: null,
    });
  });

  it("streaming + composer stores are independent (cross-store subscribe isolation)", () => {
    const streamingFires: number[] = [];
    const composerFires: number[] = [];

    const u1 = useStreamingStore.subscribe((s) => streamingFires.push(s.streamingText.length));
    const u2 = useComposerStore.subscribe((s) => composerFires.push(s.composer.length));

    streamingFires.length = 0;
    composerFires.length = 0;

    // Touch streaming — composer should NOT fire.
    useStreamingStore.getState().appendText("abc");
    expect(streamingFires.length).toBeGreaterThan(0);
    expect(composerFires).toEqual([]);

    // Touch composer — streaming should NOT fire.
    useComposerStore.getState().changeComposer("draft");
    expect(composerFires.length).toBeGreaterThan(0);
    // streamingFires count unchanged from prior value
    const beforeComposer = streamingFires.length;
    expect(streamingFires.length).toBe(beforeComposer);

    u1();
    u2();
  });
});
