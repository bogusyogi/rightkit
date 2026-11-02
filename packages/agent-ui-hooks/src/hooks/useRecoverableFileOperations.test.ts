// @vitest-environment happy-dom
import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import {
  pushDeletedBatch,
  type DeletedWorkspaceEntry,
  type RecoverableFileOperationsClient,
  useRecoverableFileOperations,
} from "./useRecoverableFileOperations.js";

const file = { name: "notes.md", path: "notes.md", kind: "file" as const };
const receipt: DeletedWorkspaceEntry = {
  name: "notes.md",
  path: "notes.md",
  originalPath: "notes.md",
  trashPath: ".coderight/trash/notes.md",
  kind: "file",
};

function client(overrides: Partial<RecoverableFileOperationsClient> = {}): RecoverableFileOperationsClient {
  return {
    createFile: vi.fn().mockResolvedValue(file),
    createDirectory: vi.fn().mockResolvedValue({ name: "src", path: "src", kind: "directory" }),
    renameEntry: vi.fn().mockResolvedValue({ ...file, name: "readme.md", path: "readme.md" }),
    trashEntry: vi.fn().mockResolvedValue(receipt),
    restoreDeletedEntries: vi.fn().mockResolvedValue([file]),
    revealEntry: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  };
}

describe("useRecoverableFileOperations", () => {
  it("retains multiple delete batches and restores the newest batch first", async () => {
    const daemon = client();
    const { result } = renderHook(() =>
      useRecoverableFileOperations({
        config: { url: "http://127.0.0.1:9999", token: "token" },
        workspaceRoot: "/repo",
        client: daemon,
      }),
    );

    await act(async () => {
      await result.current.trashEntries([file]);
      await result.current.trashEntries([{ ...file, name: "other.md", path: "other.md" }]);
    });

    expect(result.current.undoCount).toBe(2);
    await act(async () => {
      await result.current.undo();
    });
    expect(daemon.restoreDeletedEntries).toHaveBeenCalledTimes(1);
    expect(result.current.undoCount).toBe(1);
  });

  it("puts a failed restore back on the undo stack", async () => {
    const daemon = client({
      restoreDeletedEntries: vi.fn().mockRejectedValue(new Error("daemon unavailable")),
    });
    const { result } = renderHook(() =>
      useRecoverableFileOperations({
        config: { url: "http://127.0.0.1:9999", token: "token" },
        workspaceRoot: "/repo",
        client: daemon,
      }),
    );

    await act(async () => {
      await result.current.trashEntries([file]);
      await result.current.undo();
    });

    expect(result.current.undoCount).toBe(1);
    expect(result.current.error).toContain("daemon unavailable");
  });

  it("caps the stack at the donor's twenty recoverable batches", () => {
    const stack: DeletedWorkspaceEntry[][] = [];
    for (let index = 0; index < 21; index += 1) pushDeletedBatch(stack, [{ ...receipt, path: `${index}.md` }]);
    expect(stack).toHaveLength(20);
    expect(stack[0][0].path).toBe("1.md");
  });
});
