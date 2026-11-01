// @vitest-environment jsdom
import { renderHook, act } from "@testing-library/react";
import { useRef, useState } from "react";
import { describe, expect, it, vi } from "vitest";
import type { LoadedDocument, ViewMode } from "@rightkit/document-core/documents/types";
import type { ImageSaveFunction } from "@rightkit/media-ui";
import type { PdfSavePlan } from "@rightkit/document-core/documents/pdf/pdfEditModel";
import { useDocumentSaver } from "./useDocumentSaver";
import {
  copyDocument,
  fileFingerprint,
  saveFileDialog,
  writeDocument,
  writeDocumentBytes,
} from "@rightkit/document-core/state/tauriBridge";

const FILE = "D:\\docs\\note.txt";

vi.mock("@rightkit/document-core/state/tauriBridge", () => ({
  readDocument: vi.fn(async (path: string) => ({
    path,
    displayName: "note.txt",
    format: "txt" as const,
    content: "hello",
    encoding: "UTF-8",
    fingerprint: { mtimeMs: 2, size: 5 },
  })),
  writeDocument: vi.fn(async () => "UTF-8"),
  writeDocumentBytes: vi.fn(async () => {}),
  copyDocument: vi.fn(async () => {}),
  saveFileDialog: vi.fn(async () => "D:\\docs\\note-saved.txt"),
  regrantSavedTarget: vi.fn(async (target: string) => target),
  // refreshFingerprint() (called inside onSave/onSaveAs) invokes this; without it the mock returns
  // undefined and the save catch-block swallows the TypeError into a null result.
  fileFingerprint: vi.fn(async () => ({ mtimeMs: 2, size: 5 })),
  parseHostError: (error: unknown) =>
    error && typeof error === "object" && "code" in error
      ? { code: String((error as { code: unknown }).code), message: String(error) }
      : { code: "unknown", message: String(error) },
}));

vi.mock("../state/library", () => ({
  recordRecent: vi.fn(async () => {}),
}));

function useHarness() {
  const editsRef = useRef<Record<string, string>>({ [FILE]: "hello" });
  const writeEdits = vi.fn();
  const [, setDocxDirtyState] = useState<Set<string>>(new Set());
  const setDocxDirty = vi.fn((updater: unknown) => setDocxDirtyState(updater as never));
  const [, setPdfDirtyState] = useState<Set<string>>(new Set());
  const pdfDirtyRef = useRef<Set<string>>(new Set());
  // Mirrors useDocumentTabs.ts's real setPdfDirty: keeps the ref in sync alongside state so
  // useDocumentSaver's "was this doc already dirty before the attempt" read stays correct.
  const setPdfDirty = vi.fn((updater: unknown) => {
    setPdfDirtyState((prev) => {
      const next =
        typeof updater === "function"
          ? (updater as (p: Set<string>) => Set<string>)(prev)
          : (updater as Set<string>);
      pdfDirtyRef.current = next;
      return next;
    });
  });
  const [openDocs, setOpenDocs] = useState<LoadedDocument[]>([
    { path: FILE, displayName: "note.txt", format: "txt", content: "hello", encoding: "UTF-8" },
  ]);
  const [activePath, setActivePath] = useState<string | null>(null);
  const [views, setViews] = useState<Record<string, ViewMode>>({});
  const [error, setError] = useState<string | null>(null);
  const [conflict, setConflict] = useState<string | null>(null);
  const fingerprints = useRef<Map<string, { mtimeMs: number; size: number }>>(new Map());
  const refreshLibrary = vi.fn(async () => {});
  const imageSaver = useRef<((output: never) => Promise<{ buffer: ArrayBuffer } | null>) | null>(
    null,
  );
  const imageSaversRef = useRef<Map<string, ImageSaveFunction>>(new Map());
  const docxSaver = useRef<(() => Promise<ArrayBuffer | null>) | null>(null);
  const docxSaversRef = useRef<Map<string, () => Promise<ArrayBuffer | null>>>(new Map());
  const pdfSaversRef = useRef<Map<string, () => Promise<Uint8Array | null>>>(new Map());
  const pdfSavePlansRef = useRef<Map<string, () => Promise<PdfSavePlan>>>(new Map());
  const clearImageDirty = vi.fn();
  const writeImagePayload = vi.fn(async () => {});
  const releaseReplacedBackingPath = vi.fn();
  const withTabKey = <T extends LoadedDocument>(doc: T, existingKey?: string): T => ({
    ...doc,
    tabKey: existingKey ?? doc.tabKey ?? "tab:1",
  });

  const saver = useDocumentSaver({
    editsRef,
    writeEdits,
    setDocxDirty,
    setPdfDirty,
    pdfDirtyRef,
    setOpenDocs,
    setActivePath,
    setViews,
    setError,
    setConflict,
    fingerprints,
    refreshLibrary,
    imageSaver,
    imageSaversRef,
    docxSaver,
    docxSaversRef,
    pdfSaversRef,
    pdfSavePlansRef,
    clearImageDirty,
    writeImagePayload,
    releaseReplacedBackingPath,
    withTabKey,
  });

  return {
    saver,
    openDocs,
    activePath,
    views,
    error,
    conflict,
    imageSaversRef,
    docxSaversRef,
    pdfSaversRef,
    pdfSavePlansRef,
    pdfDirtyRef,
  };
}

describe("useDocumentSaver", () => {
  it("coalesces concurrent Save As requests for the same document", async () => {
    vi.mocked(saveFileDialog).mockClear();
    vi.mocked(writeDocument).mockClear();
    const { result } = renderHook(() => useHarness());
    const doc = result.current.openDocs[0];
    await act(async () => {
      await Promise.all([result.current.saver.onSaveAs(doc), result.current.saver.onSaveAs(doc)]);
    });
    expect(saveFileDialog).toHaveBeenCalledTimes(1);
    expect(writeDocument).toHaveBeenCalledTimes(1);
  });

  it("copies a clean PDF byte-for-byte on Save As without invoking a serializer", async () => {
    vi.mocked(copyDocument).mockClear();
    vi.mocked(writeDocumentBytes).mockClear();
    vi.mocked(saveFileDialog).mockResolvedValueOnce("D:\\docs\\report-copy.pdf");
    const { result } = renderHook(() => useHarness());
    const pdf = {
      path: "D:\\docs\\report.pdf",
      displayName: "report.pdf",
      format: "pdf" as const,
      content: "",
    };

    let saved: string | null = null;
    await act(async () => {
      saved = await result.current.saver.onSaveAs(pdf);
    });

    expect(saved).toBe("D:\\docs\\report-copy.pdf");
    expect(copyDocument).toHaveBeenCalledWith(pdf.path, "D:\\docs\\report-copy.pdf");
    expect(writeDocumentBytes).not.toHaveBeenCalled();
    expect(result.current.activePath).toBe("D:\\docs\\report-copy.pdf");
  });

  it("keeps dirty PDF edits when no serializer is registered", async () => {
    const { result } = renderHook(() => useHarness());
    const pdf = {
      path: "D:\\docs\\report.pdf",
      displayName: "report.pdf",
      format: "pdf" as const,
      content: "",
    };
    result.current.pdfDirtyRef.current = new Set([pdf.path]);

    let saved: string | null = null;
    await act(async () => {
      saved = await result.current.saver.onSave(pdf);
    });

    expect(saved).toBeNull();
    expect(result.current.pdfDirtyRef.current.has(pdf.path)).toBe(true);
    expect(result.current.error).toBe("PDF edits could not be serialized.");
  });

  it("keeps newer PDF edits dirty & avoids Save As tab replacement", async () => {
    vi.mocked(saveFileDialog).mockResolvedValueOnce("D:\\docs\\report-copy.pdf");
    const { result } = renderHook(() => useHarness());
    const pdf = {
      path: "D:\\docs\\report.pdf",
      displayName: "report.pdf",
      format: "pdf" as const,
      content: "",
    };
    const commit = vi.fn(async () => ({
      committedPath: "D:\\docs\\report-copy.pdf",
      committedSha256: "saved-sha",
      affectedPages: [1],
      revision: { documentSha256: "saved-sha", pageCount: 1, pages: [] },
      preservedUnsavedEdits: true,
    }));
    result.current.pdfSavePlansRef.current.set(pdf.path, async () => ({
      commit,
      didCommit: vi.fn(async () => {}),
    }));

    let saved: string | null = null;
    await act(async () => {
      saved = await result.current.saver.onSaveAs(pdf);
    });

    expect(saved).toBeNull();
    expect(result.current.pdfDirtyRef.current.has(pdf.path)).toBe(true);
    expect(result.current.activePath).toBeNull();
    expect(result.current.openDocs.some((doc) => doc.path === "D:\\docs\\report-copy.pdf")).toBe(
      false,
    );
  });

  it.each(["save", "same-path-save-as"])(
    "suppresses watcher echo for preserved %s PDF commits",
    async (mode) => {
      vi.mocked(fileFingerprint).mockClear();
      const { result } = renderHook(() => useHarness());
      const pdf = {
        path: "D:\\docs\\report.pdf",
        displayName: "report.pdf",
        format: "pdf" as const,
        content: "",
      };
      result.current.pdfSavePlansRef.current.set(pdf.path, async () => ({
        commit: vi.fn(async () => ({
          committedPath: pdf.path,
          committedSha256: "saved-sha",
          affectedPages: [1],
          revision: { documentSha256: "saved-sha", pageCount: 1, pages: [] },
          preservedUnsavedEdits: true,
        })),
        didCommit: vi.fn(async () => {}),
      }));

      await act(async () => {
        if (mode === "same-path-save-as") {
          vi.mocked(saveFileDialog).mockResolvedValueOnce(pdf.path);
          await result.current.saver.onSaveAs(pdf);
        } else {
          await result.current.saver.onSave(pdf);
        }
      });

      expect(fileFingerprint).toHaveBeenCalledWith(pdf.path);
      expect(result.current.pdfDirtyRef.current.has(pdf.path)).toBe(true);
    },
  );
});
