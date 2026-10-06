import type { ImageEdit } from "./imageEdit.js";

export interface ImageEditSnapshot {
  readonly edit: ImageEdit;
  readonly undo: readonly ImageEdit[];
  readonly redo: readonly ImageEdit[];
}

const snapshots = new Map<string, ImageEditSnapshot>();

export function getImageEditSnapshot(path: string): ImageEditSnapshot | undefined {
  const entry = snapshots.get(path);
  return entry ? { edit: entry.edit, undo: [...entry.undo], redo: [...entry.redo] } : undefined;
}

export function setImageEditSnapshot(path: string, snapshot: ImageEditSnapshot): void {
  snapshots.delete(path);
  snapshots.set(path, {
    edit: snapshot.edit,
    undo: [...snapshot.undo],
    redo: [...snapshot.redo],
  });
}

export function clearImageEditSnapshot(path: string): void {
  snapshots.delete(path);
}

export function resetImageEditStoreForTests(): void {
  snapshots.clear();
}
