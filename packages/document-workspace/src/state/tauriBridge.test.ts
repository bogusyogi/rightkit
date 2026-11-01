// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

const core = vi.hoisted(() => ({
  invoke: vi.fn(),
  convertFileSrc: vi.fn((path: string, protocol?: string) => `${protocol ?? "asset"}://${path}`),
}));

vi.mock("@tauri-apps/api/core", () => core);

import { commitPdfSave, readDocument, writeDocument } from "@rightkit/document-core/state/tauriBridge";

describe("readDocument binary stream URLs", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      configurable: true,
      value: {},
    });
  });

  it("uses the opaque stream token instead of the file path for binary asset URLs", async () => {
    core.invoke.mockResolvedValueOnce({
      path: "D:\\docs\\sample.pdf",
      content: "",
      stream_token: "stream-1",
      mtime_ms: 1,
      size: 10,
    });

    const doc = await readDocument("D:\\docs\\sample.pdf");

    expect(core.convertFileSrc).toHaveBeenCalledWith("stream-1", "viewright-stream");
    expect(core.convertFileSrc).not.toHaveBeenCalledWith("D:\\docs\\sample.pdf");
    expect(doc.assetUrl).toMatch(/^viewright-stream:\/\/stream-1\?t=/);
  });
});

describe("writeDocument source identity", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      configurable: true,
      value: {},
    });
  });

  it("forwards expected source SHA for guarded in-place saves", async () => {
    core.invoke.mockResolvedValueOnce({ encoding: "UTF-8", source_sha256: "next" });

    await expect(writeDocument("D:\\docs\\note.txt", "edited", "UTF-8", "before")).resolves.toEqual(
      {
        encoding: "UTF-8",
        sourceSha256: "next",
      },
    );
    expect(core.invoke).toHaveBeenCalledWith("write_document", {
      path: "D:\\docs\\note.txt",
      content: "edited",
      encoding: "UTF-8",
      expectedSourceSha256: "before",
    });
  });
});

describe("commitPdfSave", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
  });

  it("propagates a WriteConflict rejection (expired/replayed/mismatched token) unmodified", async () => {
    core.invoke.mockRejectedValueOnce({
      code: "write_conflict",
      message: "target changed before save",
    });

    await expect(commitPdfSave("stale-token")).rejects.toEqual({
      code: "write_conflict",
      message: "target changed before save",
    });
  });
});
