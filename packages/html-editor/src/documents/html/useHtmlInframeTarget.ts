import { useEffect, useMemo, useRef, useState, type RefObject } from "react";
import type { BlockDraftSnapshot } from "@rightkit/document-core/app/blockDrafts";
import { sourceChecksum, type HtmlSafeBlockTarget } from "./sourceMap.js";

interface HtmlInframeTargetDeps {
  safeIframeRef: RefObject<HTMLIFrameElement | null>;
  safeTarget: HtmlSafeBlockTarget | null;
  mode: "interactive" | "preview";
  blockSnapshot: BlockDraftSnapshot | null | undefined;
  activePath: string;
  /** The applied snapshot the preview currently renders. */
  applied: string;
}

/**
 * ProseMirror block-editor target resolution for the CE-5 in-frame mount: resolves the live
 * `safeTarget.childPath` element inside the Safe iframe's contentDocument (re-identifying by
 * authored tag/id/classes when sanitization shifted the path), re-resolving across iframe
 * document generations, and dropping targets detached by a generation swap. Extracted from
 * HtmlDocument verbatim — no behavior change.
 */
export function useHtmlInframeTarget({
  safeIframeRef,
  safeTarget,
  mode,
  blockSnapshot,
  activePath,
  applied,
}: HtmlInframeTargetDeps) {
  // Resolve the live iframeTarget element for in-frame mounting (safe mode). The element is
  // looked up from safeTarget.childPath inside safeIframeRef's contentDocument. Growth pushes
  // following sibling in normal flow — no geometry translation.
  const [inframeTarget, setInframeTarget] = useState<HTMLElement | null>(null);
  const safeDocumentRef = useRef<Document | null>(null);
  const [safeDocumentGeneration, setSafeDocumentGeneration] = useState(0);
  const iframeGeneration = useMemo(
    () => `${activePath}:${sourceChecksum(applied)}:${safeDocumentGeneration}`,
    [activePath, applied, safeDocumentGeneration],
  );
  // iframeGeneration/applied intentionally re-resolve target after Safe iframe DOM replacement.
  // biome-ignore lint/correctness/useExhaustiveDependencies: iframe lifecycle keys trigger DOM re-resolution
  useEffect(() => {
    if (!safeTarget || mode !== "preview" || !blockSnapshot) {
      setInframeTarget(null);
      return;
    }
    const resolve = () => {
      const doc = safeIframeRef.current?.contentDocument;
      if (!doc) return null;
      if (safeDocumentRef.current !== doc) {
        safeDocumentRef.current = doc;
        setSafeDocumentGeneration((generation) => generation + 1);
      }
      const path = safeTarget.childPath;
      // Safe DOM flattens locked custom elements, so recounting children drifts from source; the
      // viewer stamped each editable element with its source-equivalent path before sanitizing.
      const stamped = doc.body.querySelector<HTMLElement>(
        `[data-vr-edit-path="${path.join(".")}"]`,
      );
      if (stamped) return stamped;
      let cur: Element | null = doc.body;
      for (let i = 0; i < path.length; i++) {
        if (!cur) return null;
        const els: Element[] = Array.from(cur.children).filter((el: Element) => !!el.tagName);
        cur = els[path[i]] ?? null;
      }
      if (cur) return cur as HTMLElement;
      // Sanitization can remove nodes that existed in Interactive (scripts/comments are common),
      // shifting child paths. Re-identify by stable authored tag/id/classes before giving up.
      const tag = safeTarget.tagName?.toLowerCase();
      const candidates = Array.from(doc.body.querySelectorAll<HTMLElement>(tag ? tag : "*"));
      return (
        candidates.find((candidate) => {
          if (safeTarget.id && candidate.id !== safeTarget.id) return false;
          if (
            safeTarget.classNames?.length &&
            !safeTarget.classNames.every((name) => candidate.classList.contains(name))
          )
            return false;
          const text = (candidate.textContent ?? "").replace(/\s+/g, " ").trim();
          return !safeTarget.text || text.includes(safeTarget.text.slice(0, 80));
        }) ?? null
      );
    };
    // Initial attempt (doc may already be loaded). The ref is populated by HtmlViewer after this
    // effect's first render, so poll briefly across the srcdoc commit/load race.
    let attempts = 0;
    let resolveRaf = 0;
    const resolveWhenReady = () => {
      const next = resolve();
      setInframeTarget(next);
      if (!next && attempts++ < 120) resolveRaf = window.requestAnimationFrame(resolveWhenReady);
    };
    resolveWhenReady();
    const iframe = safeIframeRef.current;
    const onLoad = () => setInframeTarget(resolve());
    // Re-resolve when iframe reloads (generation change) or after next paint.
    iframe?.addEventListener("load", onLoad);
    return () => {
      iframe?.removeEventListener("load", onLoad);
      if (resolveRaf) window.cancelAnimationFrame(resolveRaf);
    };
  }, [safeTarget, mode, blockSnapshot, iframeGeneration, applied]);
  // When generation changes, stale EditorView is destroyed via iframeGeneration prop; clear target
  // if the document no longer contains it.
  // biome-ignore lint/correctness/useExhaustiveDependencies: iframe generation invalidates target DOM
  useEffect(() => {
    if (!inframeTarget) return;
    const currentDocument = safeIframeRef.current?.contentDocument;
    const isInCurrentDocument = currentDocument
      ? Boolean(currentDocument.documentElement?.contains(inframeTarget))
      : true;
    if (
      !isInCurrentDocument ||
      (currentDocument && inframeTarget.ownerDocument !== currentDocument)
    ) {
      // Clear stale detached targets immediately; EditorView must never dispatch into old iframe.
      setInframeTarget(null);
    }
  }, [iframeGeneration, inframeTarget]);

  return { inframeTarget, setInframeTarget, iframeGeneration };
}
