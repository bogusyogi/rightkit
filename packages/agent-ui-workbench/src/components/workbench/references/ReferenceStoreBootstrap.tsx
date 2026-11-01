import { useEffect } from "react";

import {
  buildInlineReferenceResolver,
  setInlineReferenceHostAction,
  setInlineReferenceResolver,
} from "./referenceRegistry";
import { useReferenceStore } from "@rightkit/agent-ui-core/state/referenceStore";
import { performReferenceAction } from "./referenceActions";

// D2 — startup-only bridge component.
//
// Calls `useReferenceStore.restorePersisted()` once on mount so the chip
// strip / inline resolver reflect the metadata persisted across the last
// restart. Also installs the inline-reference resolver into the markdown
// registry so any path-shaped text the transcript re-renders upgrades to a
// typed link when it matches a known reference.
//
// Owned by Agent 3 (D2). Agent 4 should mount this near the root of the
// workbench tree (NOT inside `AppBody`/`WorkbenchContent` — both FORBIDDEN).
// Mounting twice is safe: the second mount is a no-op for restorePersisted
// (Zustand writes the same value back) and replaces the resolver with the
// same reference.

export function ReferenceStoreBootstrap() {
  const restorePersisted = useReferenceStore((s) => s.restorePersisted);
  const references = useReferenceStore((s) => s.references);

  useEffect(() => {
    restorePersisted();
  }, [restorePersisted]);

  useEffect(() => {
    setInlineReferenceResolver(buildInlineReferenceResolver(references));
    setInlineReferenceHostAction((id, action) => {
      const reference = useReferenceStore.getState().references.find((item) => item.id === id);
      if (reference) void performReferenceAction(reference, action);
    });
    return () => {
      setInlineReferenceResolver(null);
      setInlineReferenceHostAction(null);
    };
  }, [references]);

  return null;
}

export default ReferenceStoreBootstrap;
