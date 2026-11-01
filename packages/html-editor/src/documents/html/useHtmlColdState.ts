import {
  useEffect,
  useRef,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import { sourceChecksum } from "./sourceMap";
import { htmlColdStateCache } from "./htmlResourceLifecycle";

interface HtmlColdStateDeps {
  entryPath: string;
  activePath: string;
  /** Live, editable source for the active page. */
  activeBuffer: string;
  /** The applied snapshot the preview currently renders. */
  applied: string;
  renderedRevisionRef: MutableRefObject<string | null>;
  setApplied: Dispatch<SetStateAction<string>>;
  setInitialAppliedReady: Dispatch<SetStateAction<boolean>>;
}

/**
 * Cold-tab handoff: on remount, restore the last coherent rendered bytes + revision so the preview
 * avoids a boot blank; on unmount, park them in the cold-state cache. Source/page/scroll/drafts stay
 * owned by the existing app stores. Extracted from HtmlDocument verbatim — no behavior change.
 */
export function useHtmlColdState({
  entryPath,
  activePath,
  activeBuffer,
  applied,
  renderedRevisionRef,
  setApplied,
  setInitialAppliedReady,
}: HtmlColdStateDeps) {
  // Cold tabs destroy their iframe/editor work. Preserve only coherent rendered bytes + revision so
  // remount can avoid a boot blank; source/page/scroll/drafts remain owned by existing app stores.
  const coldRestoredRef = useRef(false);
  const coldStateRef = useRef({ activePath, applied });
  coldStateRef.current = { activePath, applied };
  // biome-ignore lint/correctness/useExhaustiveDependencies: refs/setters arrive as stable hook params
  useEffect(() => {
    if (coldRestoredRef.current) return;
    coldRestoredRef.current = true;
    const cached = htmlColdStateCache.get(`${entryPath}\u0000${activePath}`);
    if (!cached || cached.renderedRevision !== sourceChecksum(activeBuffer)) return;
    renderedRevisionRef.current = cached.renderedRevision;
    setApplied(cached.applied);
    setInitialAppliedReady(true);
  }, [activeBuffer, activePath, entryPath]);
  // biome-ignore lint/correctness/useExhaustiveDependencies: refs/setters arrive as stable hook params
  useEffect(
    () => () => {
      const { activePath: currentPath, applied: currentApplied } = coldStateRef.current;
      const revision = renderedRevisionRef.current;
      if (revision && currentApplied)
        htmlColdStateCache.set(`${entryPath}\u0000${currentPath}`, {
          applied: currentApplied,
          renderedRevision: revision,
        });
    },
    [entryPath],
  );
}
