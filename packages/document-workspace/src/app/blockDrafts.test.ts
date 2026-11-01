// @vitest-environment jsdom
import { describe, expect, it, vi } from "vitest";
import { createBlockDraftRegistry, type BlockDraftIdentity } from "@rightkit/document-core/app/blockDrafts";

const identity = (documentPath: string, pagePath = documentPath): BlockDraftIdentity => ({
  documentPath,
  pagePath,
});

function begin(
  registry: ReturnType<typeof createBlockDraftRegistry>,
  id: BlockDraftIdentity,
  originalText = "original",
  overrides: Partial<Parameters<typeof registry.begin>[0]> = {},
) {
  return registry.begin({
    identity: id,
    blockKind: "paragraph",
    originalRevision: "r1",
    originalText,
    commit: vi.fn(async () => ({ ok: true as const })),
    discard: vi.fn(),
    ...overrides,
  });
}

describe("block draft registry", () => {
  it("deduplicates concurrent Done calls", async () => {
    const registry = createBlockDraftRegistry();
    let finish!: (value: { ok: true }) => void;
    const commit = vi.fn(
      () =>
        new Promise<{ ok: true }>((resolve) => {
          finish = resolve;
        }),
    );
    begin(registry, identity("doc.md"), "one", { commit });
    const first = registry.resolve(identity("doc.md"), "manual-save", "done");
    const second = registry.resolve(identity("doc.md"), "manual-save", "done");
    expect(second).toBe(first);
    await Promise.resolve();
    finish({ ok: true });
    await expect(first).resolves.toEqual({ outcome: "done" });
    expect(commit).toHaveBeenCalledTimes(1);
  });

  it("keeps a modified committing draft visible and refuses discard until commit settles", async () => {
    const registry = createBlockDraftRegistry();
    const id = identity("doc.md");
    let finish!: (value: { ok: true }) => void;
    const commit = vi.fn(
      () =>
        new Promise<{ ok: true }>((resolve) => {
          finish = resolve;
        }),
    );
    const discard = vi.fn();
    begin(registry, id, "one", { commit, discard });
    registry.update(id, "two");

    const pending = registry.resolve(id, "manual-save", "done");
    await Promise.resolve();
    expect(registry.snapshotModified()).toHaveLength(1);
    expect(registry.snapshotModified()[0]).toMatchObject({
      status: "committing",
      currentText: "two",
    });
    expect(registry.discard(id)).toBe(false);
    await expect(registry.resolve(id, "close", "discard")).resolves.toMatchObject({
      outcome: "failed",
      reason: "Block draft is already committing.",
    });
    expect(discard).not.toHaveBeenCalled();

    finish({ ok: true });
    await expect(pending).resolves.toEqual({ outcome: "done" });
    expect(commit).toHaveBeenCalledTimes(1);
    expect(registry.get(id)).toBeNull();
  });
});
