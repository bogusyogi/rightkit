import { beforeEach, describe, expect, it } from "vitest";

import type { Attachment } from "../lib/daemon";
import { useComposerStore } from "./composerStore";
import { useReferenceStore } from "./referenceStore";

function resetComposerStore() {
  useComposerStore.setState({
    composer: "",
    promptHistory: [],
    promptHistoryIndex: null,
    promptHistoryDraft: "",
    attachments: [],
  });
}

describe("composer store", () => {
  beforeEach(() => {
    resetComposerStore();
    useReferenceStore.setState({ references: [], restoreError: null });
  });

  it("deduplicates remembered prompts and keeps the newest prompt first", () => {
    const store = useComposerStore.getState();

    store.rememberPrompt(" first prompt ");
    store.rememberPrompt("second prompt");
    store.rememberPrompt("first prompt");

    expect(useComposerStore.getState().promptHistory).toEqual(["first prompt", "second prompt"]);
  });

  it("restores the draft after navigating back out of prompt history", () => {
    const store = useComposerStore.getState();
    store.rememberPrompt("older");
    store.rememberPrompt("newer");
    store.changeComposer("draft in progress");

    useComposerStore.getState().navigateHistory(-1);
    expect(useComposerStore.getState()).toMatchObject({
      composer: "newer",
      promptHistoryIndex: 0,
      promptHistoryDraft: "draft in progress",
    });

    useComposerStore.getState().navigateHistory(-1);
    expect(useComposerStore.getState()).toMatchObject({
      composer: "older",
      promptHistoryIndex: 1,
    });

    useComposerStore.getState().navigateHistory(1);
    useComposerStore.getState().navigateHistory(1);
    expect(useComposerStore.getState()).toMatchObject({
      composer: "draft in progress",
      promptHistoryIndex: null,
      promptHistoryDraft: "",
    });
  });

  it("adds and removes attachments without disturbing composer text", () => {
    const first: Attachment = { type: "image", data: "one", mimeType: "image/png" };
    const second: Attachment = { type: "image", data: "two", mimeType: "image/jpeg" };
    const store = useComposerStore.getState();

    store.changeComposer("keep me");
    store.addAttachments([first, second]);
    useComposerStore.getState().removeAttachment(0);

    expect(useComposerStore.getState()).toMatchObject({
      composer: "keep me",
      attachments: [second],
    });
  });

  it("clears typed references with the composer payload", () => {
    useReferenceStore.getState().addReferences([
      {
        id: "skill:audit",
        kind: "skill",
        destination: "@audit",
        provenance: {
          source: "composer",
          createdAt: "2026-07-21T00:00:00Z",
          trust: "workspaceTrusted",
        },
      },
    ]);
    useComposerStore.getState().changeComposer("run it");

    useComposerStore.getState().clear();

    expect(useComposerStore.getState().composer).toBe("");
    expect(useReferenceStore.getState().references).toEqual([]);
  });
});
