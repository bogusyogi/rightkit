const activity = new Map<string, boolean>();
const listeners = new Map<string, Set<() => void>>();

function key(documentPath?: string): string {
  return documentPath || "markdown:anonymous";
}

export function isMarkdownDocumentActive(documentPath?: string): boolean {
  return activity.get(key(documentPath)) ?? true;
}

/** Published by each mounted Markdown tab; defaults to active for standalone renderers. */
export function publishMarkdownDocumentActivity(
  documentPath: string | undefined,
  active: boolean,
): void {
  const id = key(documentPath);
  if (activity.get(id) === active) return;
  activity.set(id, active);
  for (const listener of listeners.get(id) ?? []) listener();
}

export function subscribeMarkdownDocumentActivity(
  documentPath: string | undefined,
  listener: () => void,
): () => void {
  const id = key(documentPath);
  const bucket = listeners.get(id) ?? new Set<() => void>();
  bucket.add(listener);
  listeners.set(id, bucket);
  return () => {
    bucket.delete(listener);
    if (bucket.size === 0) listeners.delete(id);
  };
}

export function resetMarkdownDocumentActivityForTests(): void {
  activity.clear();
  listeners.clear();
}
