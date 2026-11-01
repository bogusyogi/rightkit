// @vitest-environment jsdom
import { renderHook, act } from "@testing-library/react";
import { useRef, useState } from "react";
import { describe, expect, it, vi } from "vitest";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import { useDocumentTabs } from "./useDocumentTabs.js";
import { useDocumentWatcher } from "./useDocumentWatcher.js";
import { fileFingerprint } from "@rightkit/document-core/state/tauriBridge";

const FILE_A = "D:\\docs\\a.txt";

vi.mock("@rightkit/document-core/state/tauriBridge", () => ({
  fileFingerprint: vi.fn(async () => null),
  readDocument: vi.fn(async (path: string) => ({
    path,
    displayName: path,
    format: "txt" as const,
    content: "disk content",
    encoding: "UTF-8",
    fingerprint: { mtimeMs: 9, size: 12 },
  })),
  revokePath: vi.fn(async () => {}),
}));

function useHarness(
  pendingSavePaths: ReadonlySet<string> = new Set(),
  synchronousPendingSavePaths: ReadonlySet<string> = new Set(),
) {
  const tabs = useDocumentTabs();
  const [openDocs, setOpenDocsState] = useState<LoadedDocument[]>([]);
  const setOpenDocs: typeof tabs.setOpenDocs = (updater) => {
    setOpenDocsState((prev) =>
      typeof updater === "function"
        ? (updater as (p: LoadedDocument[]) => LoadedDocument[])(prev)
        : updater,
    );
  };
  const openDocsRef = useRef<LoadedDocument[]>(openDocs);
  openDocsRef.current = openDocs;
  const activationOrderRef = useRef<string[]>([]);
  const [activePath, setActivePath] = useState<string | null>(null);
  const [confirmClose, setConfirmClose] = useState<string | null>(null);
  const [conflict, setConflict] = useState<string | null>(null);
  const [, setHtmlConfirm] = useState<{ entryPath: string; mode: "save" | "close" } | null>(null);
  const dirtyRef = useRef(false);
  const isDirty = vi.fn(() => dirtyRef.current);
  const openLiveUrl = vi.fn(async () => {});
  const activate = vi.fn(async () => {});
  const open = vi.fn(async () => {});
  const fingerprints = useRef<Map<string, { mtimeMs: number; size: number }>>(new Map());
  const selfSaved = useRef<Set<string>>(new Set());
  const synchronousPendingSavePathsRef = useRef<ReadonlySet<string>>(synchronousPendingSavePaths);
  const reloadHtmlSubpage = vi.fn();
  const onSave = vi.fn(async () => null);
  const onDocxSave = vi.fn(async () => {});
  const docxSaver = useRef<(() => Promise<ArrayBuffer | null>) | null>(null);
  const docxSaversRef = useRef<Map<string, () => Promise<ArrayBuffer | null>>>(new Map());
  const onDocxSaveAs = vi.fn(async () => null);
  const withTabKey = <T extends LoadedDocument>(doc: T, existingKey?: string): T => ({
    ...doc,
    tabKey: existingKey ?? doc.tabKey ?? "tab:1",
  });

  const watcher = useDocumentWatcher({
    openDocs,
    openDocsRef,
    activationOrderRef,
    setOpenDocs,
    setActivePath,
    activePath,
    writeEdits: tabs.writeEdits,
    setDocxDirty: tabs.setDocxDirty,
    setPdfDirty: tabs.setPdfDirty,
    htmlPagesRef: tabs.htmlPagesRef,
    writeHtmlPages: tabs.writeHtmlPages,
    setHtmlActivePage: tabs.setHtmlActivePage,
    mdxGrantsRef: tabs.mdxGrantsRef,
    clearImageDirty: vi.fn(),
    isDirty,
    openLiveUrl,
    activate,
    open,
    fingerprints,
    selfSaved,
    pendingSavePaths,
    synchronousPendingSavePathsRef,
    reloadHtmlSubpage,
    setHtmlConfirm,
    setConfirmClose,
    confirmClose,
    setConflict,
    onSave,
    onDocxSave,
    docxSaver,
    docxSaversRef,
    onDocxSaveAs,
    withTabKey,
  });

  return {
    watcher,
    tabs,
    openDocs,
    activePath,
    isDirty,
    dirtyRef,
    fingerprints,
    selfSaved,
    synchronousPendingSavePathsRef,
    reloadHtmlSubpage,
    conflict,
    setOpenDocs: setOpenDocsState,
  };
}

describe("useDocumentWatcher", () => {
  it("defers own watcher event until pending save completes", async () => {
    const pending = new Set([FILE_A]);
    const { result, rerender } = renderHook(
      ({ pendingSavePaths }) => useHarness(pendingSavePaths),
      { initialProps: { pendingSavePaths: pending } },
    );
    act(() => {
      result.current.setOpenDocs([
        {
          path: FILE_A,
          displayName: "a.txt",
          format: "txt",
          content: "edited",
        },
      ]);
    });
    result.current.dirtyRef.current = true;
    result.current.selfSaved.current.add(FILE_A);
    act(() => result.current.watcher.reload(FILE_A));
    expect(result.current.conflict).toBeNull();

    await act(async () => {
      rerender({ pendingSavePaths: new Set() });
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(result.current.conflict).toBeNull();
  });
  // Two-writer repro: our save stamped fingerprint {1,10}; an external editor wrote {2,11} inside
  // the 500 ms self-save echo window. The watcher must not mistake it for our own echo.
  const seedSavedDoc = (result: ReturnType<typeof useHarness>) => {
    act(() => {
      result.setOpenDocs([{ path: FILE_A, displayName: "a.txt", format: "txt", content: "mine" }]);
    });
    result.fingerprints.current.set(FILE_A, { mtimeMs: 1, size: 10 });
    result.selfSaved.current.add(FILE_A);
  };

  it("surfaces an external write that lands inside the self-save echo window (dirty tab)", async () => {
    vi.mocked(fileFingerprint).mockResolvedValueOnce({ mtimeMs: 2, size: 11 });
    const { result } = renderHook(() => useHarness());
    seedSavedDoc(result.current);
    result.current.dirtyRef.current = true;
    await act(async () => {
      result.current.watcher.reload(FILE_A);
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(result.current.conflict).toBe(FILE_A);
  });
});
