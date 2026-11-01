/**
 * Convenience hook for components that need to subscribe to a
 * {@link MarkdownDocumentCore}. The hook is no-op when no core is
 * supplied, so components that don't yet use the core keep their
 * legacy behavior.
 */

import { useEffect, useState } from "react";
import type { MarkdownDocumentCore } from "./MarkdownDocumentCore";
import type { MarkdownCoreSnapshot } from "./MarkdownDocumentTypes";

/**
 * Returns the latest snapshot from `core`, or `null` when no core is
 * supplied. Re-renders the consumer whenever the core commits.
 */
export function useMarkdownCoreSubscription(
  core: MarkdownDocumentCore | undefined,
): MarkdownCoreSnapshot | null {
  const [snapshot, setSnapshot] = useState<MarkdownCoreSnapshot | null>(
    core ? core.snapshot() : null,
  );
  useEffect(() => {
    if (!core) return;
    setSnapshot(core.snapshot());
    const unsubscribe = core.subscribe(0, () => {
      setSnapshot(core.snapshot());
    });
    return unsubscribe;
  }, [core]);
  return snapshot;
}
