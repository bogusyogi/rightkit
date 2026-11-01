// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { renderHook, act, cleanup } from "@testing-library/react";
import { useRef, useState } from "react";
import { describe, it, expect, vi, afterEach } from "vitest";
import { createBlockDraftRegistry, type BlockDraftIdentity } from "@rightkit/document-core/app/blockDrafts";
import { useDocumentSaver } from "./useDocumentSaver.js";
import type { LoadedDocument, ViewMode } from "@rightkit/document-core/documents/types";

const md = (path: string): LoadedDocument => ({
  path,
  displayName: path.split(/[\\/]/).pop() ?? path,
  format: path.endsWith(".md") ? "md" : path.endsWith(".html") ? "html" : "txt",
  content: "original content",
  encoding: "UTF-8",
});

vi.mock("@rightkit/document-core/state/tauriBridge", () => ({
  readDocument: vi.fn(async (path: string) => ({
    path,
    displayName: path.split(/[\\/]/).pop() ?? path,
    format: "txt" as const,
    content: "disk content",
    encoding: "UTF-8",
    fingerprint: { mtimeMs: 9, size: 12 },
  })),
  writeDocument: vi.fn(async () => "UTF-8"),
  writeDocumentBytes: vi.fn(async () => {}),
  saveFileDialog: vi.fn(async () => "D:\\docs\\saved.txt"),
  regrantSavedTarget: vi.fn(async (target: string) => target),
  fileFingerprint: vi.fn(async () => ({ mtimeMs: 2, size: 5 })),
  revokePath: vi.fn(async () => {}),
  getInitialOpenPaths: vi.fn(async () => []),
  onAppEvent: vi.fn(async () => () => {}),
  grantKnownFileToken: vi.fn(async () => null),
  saveViewState: vi.fn(async () => {}),
  clearSession: vi.fn(async () => {}),
  saveSession: vi.fn(async () => {}),
  loadSession: vi.fn(async () => []),
  saveSetupRecovery: vi.fn(async () => {}),
  loadSetupRecovery: vi.fn(async () => null),
  clearSetupRecovery: vi.fn(async () => {}),
}));

vi.mock("../state/library.js", () => ({
  listRecent: vi.fn(async () => []),
  listPins: vi.fn(async () => []),
  togglePin: vi.fn(async () => true),
  togglePinDir: vi.fn(async () => true),
  recordRecent: vi.fn(async () => {}),
  saveViewState: vi.fn(async () => {}),
  isPinnedPath: vi.fn(async () => false),
  loadSession: vi.fn(async () => []),
  saveSession: vi.fn(async () => {}),
  clearSession: vi.fn(async () => {}),
}));

vi.mock("@rightkit/platform-ui/headless", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@rightkit/platform-ui/headless")>()),
  toast: vi.fn(),
}));

vi.mock("@rightkit/document-core/state/diagnostics", () => ({
  logEvent: vi.fn(),
  logPerf: vi.fn(),
  logError: vi.fn(),
}));

// Settings mock for useDocuments autosave scheduler
vi.mock("@rightkit/document-core/settings/settings", () => ({
  useSettings: () => ({
    settings: {
      autoSave: false,
      restoreSession: false,
      themeId: "light",
      primaryAccentId: null,
      customPrimary: null,
      autoCheckUpdates: false,
      wordWrap: false,
      fontSizePx: 16,
      sourceFontSizePx: 12,
    },
    update: vi.fn(),
  }),
}));

// --- helpers
function beginModifiedDraft(
  registry: ReturnType<typeof createBlockDraftRegistry>,
  identity: BlockDraftIdentity,
  originalText = "original content",
  currentText = "edited content",
  opts: Partial<Parameters<typeof registry.begin>[0]> = {},
) {
  const commit = opts.commit ?? vi.fn(async () => ({ ok: true as const }));
  const discard = opts.discard ?? vi.fn();
  registry.begin({
    identity,
    blockKind: "paragraph",
    originalRevision: "r1",
    sourceRange: { start: 0, end: originalText.length },
    sourceChecksum: "chk",
    originalText,
    commit,
    discard,
    ...opts,
  });
  registry.update(identity, currentText);
  return {
    commit: commit as ReturnType<typeof vi.fn>,
    discard: discard as ReturnType<typeof vi.fn>,
  };
}

describe("lifecycle routes — shared block-draft contract", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  // ── Manual save resolves Done ───────────────────────────────────────────
  describe("save / manual save resolves Done", () => {
    it("manual save resolves target draft before canonical write; failed/cancelled stops write", async () => {
      const { writeDocument } = await import("@rightkit/document-core/state/tauriBridge");
      const registry = createBlockDraftRegistry();
      const id = { documentPath: "D:\\docs\\note.md", pagePath: "D:\\docs\\note.md" };
      const failingCommit = vi.fn(async () => ({
        ok: false as const,
        kind: "error" as const,
        reason: "validation failed",
      }));
      beginModifiedDraft(registry, id, "original", "edited", { commit: failingCommit });

      // Harness for useDocumentSaver
      const { result } = renderHook(() => {
        const editsRef = useRef<Record<string, string>>({ [id.documentPath]: "original" });
        const writeEdits = vi.fn();
        const [, setDocxDirty] = useState<Set<string>>(new Set());
        const setDocxDirtyFn = vi.fn((u: unknown) => setDocxDirty(u as never));
        const [, setOpenDocs] = useState<LoadedDocument[]>([md(id.documentPath)]);
        const [, setActivePath] = useState<string | null>(id.documentPath);
        const [, setViews] = useState<Record<string, ViewMode>>({});
        const [, setError] = useState<string | null>(null);
        const fingerprints = useRef<Map<string, { mtimeMs: number; size: number }>>(new Map());
        const refreshLibrary = vi.fn(async () => {});
        const imageSaver = useRef(null);
        const imageSaversRef = useRef(new Map());
        const docxSaver = useRef(null);
        const docxSaversRef = useRef(new Map());
        const clearImageDirty = vi.fn();
        const writeImagePayload = vi.fn(async () => {});
        const releaseReplacedBackingPath = vi.fn();
        const withTabKey = <T extends LoadedDocument>(doc: T, existingKey?: string): T => ({
          ...doc,
          tabKey: existingKey ?? "tab:1",
        });
        const saver = useDocumentSaver({
          editsRef,
          writeEdits,
          setDocxDirty: setDocxDirtyFn,
          setOpenDocs,
          setActivePath,
          setViews,
          setError,
          fingerprints,
          refreshLibrary,
          imageSaver,
          imageSaversRef,
          docxSaver,
          docxSaversRef,
          clearImageDirty,
          writeImagePayload,
          releaseReplacedBackingPath,
          withTabKey,
          blockDrafts: registry,
        });
        return { saver };
      });

      vi.mocked(writeDocument).mockClear();
      let saved: string | null = null;
      await act(async () => {
        saved = await result.current.saver.onSave(md(id.documentPath));
      });
      expect(saved).toBeNull();
      expect(failingCommit).toHaveBeenCalledTimes(1);
      expect(writeDocument).not.toHaveBeenCalled();
      expect(registry.get(id)).not.toBeNull();
      expect(registry.get(id)?.status).toBe("modified");

      // Now succeed: replace commit with success and retry
      const successCommit = vi.fn(async () => ({ ok: true as const }));
      registry.discard(id);
      beginModifiedDraft(registry, id, "original", "edited", { commit: successCommit });
      vi.mocked(writeDocument).mockClear();
      await act(async () => {
        saved = await result.current.saver.onSave(md(id.documentPath));
      });
      expect(saved).toBe(id.documentPath);
      expect(successCommit).toHaveBeenCalledTimes(1);
      expect(writeDocument).toHaveBeenCalledTimes(1);
      expect(registry.get(id)).toBeNull();
    });
  });

  // ── External change marks stale ─────────────────────────────────────────
  describe("external / source change marks stale", () => {
    it("marks draft stale on external file change while preserving draft text and blocking Done", async () => {
      const registry = createBlockDraftRegistry();
      const id = { documentPath: "D:\\docs\\note.md", pagePath: "D:\\docs\\note.md" };
      beginModifiedDraft(registry, id, "original source", "local edit");
      expect(registry.get(id)?.status).toBe("modified");
      expect(registry.get(id)?.currentText).toBe("local edit");

      // Simulate external change: markStale
      registry.markStale(id, "Source changed on disk while this block was open.");
      const stale = registry.get(id);
      expect(stale?.status).toBe("stale");
      expect(stale?.currentText).toBe("local edit"); // kept
      expect(stale?.reason).toBe("Source changed on disk while this block was open.");

      // Done must fail while stale
      registry.update(id, "local edit"); // even editing keeps stale
      expect(registry.get(id)?.status).toBe("stale");

      // Commit validates revision/checksum and returns stale failure.
      // Registry sets status to "committing" before invoking commit, so snapshot.status
      // is "committing" not "stale". Stale is tracked via wasStale flag and commit
      // must return stale on revision/checksum mismatch — simulate that here.
      const staleCommit = vi.fn(async () => ({
        ok: false as const,
        kind: "stale" as const,
        reason: "Source changed while this block was open.",
      }));
      // Replace draft commit with stale-aware one
      registry.discard(id);
      registry.begin({
        identity: id,
        blockKind: "paragraph",
        originalRevision: "r1",
        originalText: "original source",
        commit: staleCommit,
        discard: vi.fn(),
      });
      registry.update(id, "local edit");
      registry.markStale(id, "Source changed on disk while this block was open.");
      const result = await registry.resolve(id, "external-change", "done");
      expect(result.outcome).toBe("failed");
      if (result.outcome === "failed") expect(result.reason).toMatch(/Source changed/);
      expect(registry.get(id)?.status).toBe("stale");
    });
  });
});
