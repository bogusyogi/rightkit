import {
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
  type Ref,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { EditorState, Plugin, PluginKey, TextSelection } from "prosemirror-state";
import { toggleMark } from "prosemirror-commands";
import { history, redo, undo } from "prosemirror-history";
import { keymap } from "prosemirror-keymap";
import { baseKeymap } from "prosemirror-commands";
import { EditorView } from "prosemirror-view";
import { useBlockDrafts } from "@rightkit/document-core/app/BlockDraftContext";
import type { BlockDraftIdentity, BlockDraftSnapshot } from "@rightkit/document-core/app/blockDrafts";
import { htmlVisualSchema, parseHtmlToPm } from "./htmlPmParse.js";
import { SYNTHESISED_FRAGMENT_MAP } from "./sourceMap.js";
import {
  projectHtmlForVisual,
  sanitizeHtmlPasteFragment,
} from "./htmlVisualSanitize.js";
import { trackHtmlProseMirror } from "./htmlResourceLifecycle.js";
import {
  htmlTextOffsetFromPoint,
  htmlWordSelectionFromPoint,
  type HtmlInlineStyle,
} from "./sourceMap.js";
import type { RichBlockOp } from "./htmlOperationPlanner.js";
import {
  EMPTY_HTML_TEXT_STYLE,
  hasHtmlTextStyle,
  safeHtmlTextStyleValue,
  type HtmlTextStyleAttrs,
  type HtmlTextStyleProperty,
} from "./htmlTextStyle.js";
import {
  editorTypographyForTag,
  type HtmlEditorToolbarState,
  htmlInlineSchema,
  initialTextSelection,
  innerFromDoc,
  isInlineTargetTag,
  markdownBlockRulesPlugin,
  parseInframeInline,
  positionForTextOffset,
  restoreTargetSnapshot,
  serializeInframeInline,
  syncSelectionFromIframeDom,
  toolbarStateFor,
  visualAttrsForTag,
  visualNodeForTag,
  wrapped,
} from "./htmlPmBlockHelpers.js";

// The pure ProseMirror helpers moved to ./htmlPmBlockHelpers. Re-export the pieces other
// modules already import from this file so its public surface is unchanged.
export {
  editorTypographyForTag,
  innerFromDoc,
  resolveMarkdownBlockRule,
  resolveOpaqueEditorBackground,
} from "./htmlPmBlockHelpers.js";
export type { HtmlEditorToolbarState } from "./htmlPmBlockHelpers.js";

/**
 * Narrow parent-facing controls for the transient rich-edit session. This is
 * the seam the document top toolbar drives for HTML rich editing; it
 * intentionally never exposes the raw ProseMirror view.
 */
export type HtmlSafeBlockEditorHandle = {
  /** Focus the transient editor without moving its selection. */
  focus: () => void;
  /** Rename the edited block, same path as the floating block-type select. */
  changeBlockType: (nextTag: string) => void;
  /** Toggle an inline mark on the current selection, same path as B/I/code/S. */
  toggleMark: (name: "strong" | "em" | "code" | "strike" | "underline") => void;
  /** Set or clear one safe inline CSS property without changing other text-style properties. */
  applyTextStyle: (property: HtmlTextStyleProperty, value: string | null) => void;
  /** Open the add-link prompt for the current selection; errors if it is empty. */
  requestAddLink: () => void;
  /** Open the edit-link prompt prefilled with the given href. */
  requestEditLink: (href: string) => void;
  /** Open the replace-image prompt prefilled with the given src. */
  requestReplaceImage: (src: string) => void;
  /** Open the image-insert prompt for this block. */
  requestInsertImage: () => void;
  /** Open HTML-local Properties for one arbitrary source-backed attribute. */
  requestProperties: () => void;
  /** Commit the draft, same path as Done. */
  done: () => Promise<void>;
  /** Discard the draft and close, same path as Cancel/Escape. */
  cancel: () => Promise<void>;
};

export interface HtmlPropertyAttribute {
  name: string;
  value: string;
}

export function HtmlSafeBlockEditor({
  ref,
  identity,
  snapshot,
  visualStyle,
  onClose,
  onRichOp,
  onToolbarStateChange,
  propertyTargetTag,
  propertyAttributes,
  onEditSource,
  iframeTarget,
  mountMode,
  iframeGeneration,
  onIframeTargetChange,
}: {
  /** Parent-facing session controls (the rich-edit session seam). */
  ref?: Ref<HtmlSafeBlockEditorHandle>;
  identity: BlockDraftIdentity;
  snapshot: BlockDraftSnapshot;
  visualStyle?: HtmlInlineStyle;
  onClose: () => void;
  /** Structural/attribute actions on the selected block or its inline anchor. */
  onRichOp?: (op: RichBlockOp) => void;
  /** Selection-derived mark state for document chrome; this remains a transient PM seam. */
  onToolbarStateChange?: (state: HtmlEditorToolbarState) => void;
  /** Inline source target for Properties, e.g. an image inside an editable paragraph. */
  propertyTargetTag?: string;
  /** Authored attributes for the exact target, resolved from canonical source. */
  propertyAttributes?: readonly HtmlPropertyAttribute[];
  /** Advanced escape hatch: reveal this exact target in Source. */
  onEditSource?: () => void | Promise<void>;
  /** CE-5 in-flow mount: transient editor mounts inside Safe iframe's target element. */
  iframeTarget?: HTMLElement | null;
  /** CE-5 in-flow mount. When absent, editor uses its standalone test/document seam. */
  mountMode?: "inframe";
  /** Monotonic iframe document generation. Changes destroy the old EditorView so a stale controller never dispatches into a replacement document. */
  iframeGeneration?: string | number;
  /** Reports an outer target replacement back to HtmlDocument without remounting EditorView. */
  onIframeTargetChange?: (target: HTMLElement) => void;
}) {
  const drafts = useBlockDrafts();
  const host = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const toolbarStateChangeRef = useRef(onToolbarStateChange);
  toolbarStateChangeRef.current = onToolbarStateChange;
  const tag = (snapshot.targetTag ?? snapshot.blockKind).replace(/^html:/, "") || "p";
  const currentTagRef = useRef(tag);
  currentTagRef.current = tag;
  // Improvement #4: latest changeBlockType closure, read by the markdown input-rules plugin
  // (built once per mount effect run) so it always dispatches through the current render's path.
  const changeBlockTypeRef = useRef<(nextTag: string) => void>(() => {});
  const mountedTagRef = useRef(tag);
  const mountedElementRef = useRef<HTMLElement | null>(null);
  const [mountVersion, setMountVersion] = useState(0);
  const propertiesTag = propertyTargetTag ?? tag;
  const snapshotCurrentTextRef = useRef(snapshot.currentText);
  snapshotCurrentTextRef.current = snapshot.currentText;
  const snapshotBlockKindRef = useRef(snapshot.blockKind);
  snapshotBlockKindRef.current = snapshot.blockKind;
  const targetTagRef = useRef<string | undefined>(snapshot.targetTag);
  targetTagRef.current = snapshot.targetTag;
  const targetElementRef = useRef<HTMLElement | null>(null);
  const targetGenerationRef = useRef<string | number | undefined>(iframeGeneration);
  const lifecycleGenerationRef = useRef<string | number | undefined>(iframeGeneration);
  lifecycleGenerationRef.current = iframeGeneration;
  const originalTargetRef = useRef<{
    tagName: string;
    attributes: Array<[string, string]>;
    innerHTML: string;
  } | null>(null);
  const onIframeTargetChangeRef = useRef(onIframeTargetChange);
  onIframeTargetChangeRef.current = onIframeTargetChange;

  const inframeMount = mountMode === "inframe" && iframeTarget ? iframeTarget : null;
  const isInframe = !!inframeMount;
  // Standalone unit callers may omit mountMode; keep that seam flow-mounted for focused editor
  // tests. Production HtmlDocument always passes mountMode="inframe" and never reaches this path.
  const isStandalone = mountMode === undefined;

  // Target replacement (heading rename or iframe rebuild) must not remount the editor. Keep current
  // DOM identity in a ref; detached old targets are replaced by HtmlDocument's fresh resolution.
  if (isInframe && inframeMount) {
    if (
      targetGenerationRef.current !== iframeGeneration ||
      !targetElementRef.current ||
      !targetElementRef.current.isConnected
    ) {
      targetGenerationRef.current = iframeGeneration;
      targetElementRef.current = inframeMount;
      originalTargetRef.current = null;
    }
    if (!originalTargetRef.current) {
      originalTargetRef.current = {
        tagName: inframeMount.tagName.toLowerCase(),
        attributes: Array.from(inframeMount.attributes).map((attribute) => [
          attribute.name,
          attribute.value,
        ]),
        innerHTML: inframeMount.innerHTML,
      };
    }
  }

  // Inject minimal editor CSS into the iframe document when in-frame so
  // ProseMirror's contentEditable area is visible without relying on parent CSS.
  // iframeGeneration intentionally invalidates the style when Safe iframe DOM is replaced.
  // biome-ignore lint/correctness/useExhaustiveDependencies: generation is an explicit iframe lifecycle key
  useEffect(() => {
    // mountVersion is an explicit remount signal after authored root tag replacement.
    void mountVersion;
    if (!isInframe || !inframeMount) return;
    const doc = inframeMount.ownerDocument;
    if (!doc) return;
    // Re-ensure style exists; if generation changes we may have a new document without the style.
    if (doc.getElementById("viewright-html-inframe-style")) return;
    const style = doc.createElement("style");
    style.id = "viewright-html-inframe-style";
    style.textContent = `.ProseMirror{outline:none;white-space:pre-wrap;word-wrap:break-word;min-height:1em}.ProseMirror p{margin:0}`;
    doc.head?.appendChild(style);
    return () => {
      // Do not remove on unmount if document is already replaced; style lives with its document.
      if (style.parentNode) style.remove();
    };
  }, [isInframe, inframeMount, iframeGeneration]);

  // Generation invalidation: iframe document changes destroy old EditorView. A same-generation
  // heading rename replaces the authored host, then this effect remounts the same EditorState so
  // local history/selection survive the tag change.
  // biome-ignore lint/correctness/useExhaustiveDependencies: mountVersion intentionally retriggers root remount after tag replacement
  useEffect(() => {
    // CE-5 — when an iframe target is provided, mount directly into its
    // actual document flow slot (contract 14 §6.3 Option A: mount onto the targetElement
    // preserving outer tag/id/class/data/aria, width/margins, typography, background/border).
    const mount = targetElementRef.current ?? inframeMount ?? (isStandalone ? host.current : null);
    if (!mount) return;
    if (isInframe) {
      Object.assign(
        mount.style,
        editorTypographyForTag(
          tag,
          snapshotBlockKindRef.current.replace(/^html:/, "").toLowerCase(),
          visualStyle,
        ),
      );
    }
    const priorView = viewRef.current;
    const mountedTag = mountedTagRef.current;
    if (priorView && isInframe && mountedElementRef.current === mount) return;
    const preservedState =
      priorView && isInframe && mountedElementRef.current !== mount ? priorView.state : null;
    if (priorView && preservedState) {
      priorView.destroy();
      viewRef.current = null;
      mountedElementRef.current = null;
      trackHtmlProseMirror(-1);
    }
    // Inline in-frame targets use a fragment doc; container targets keep block semantics.
    // Content holding locked islands (script, custom elements) needs the block schema, which can
    // carry raw_html_island nodes; the inline schema cannot, and would lose those bytes.
    const hasLockedIsland = projectHtmlForVisual(snapshotCurrentTextRef.current).includes(
      "data-vr-raw-island",
    );
    const inlineFragment =
      isInframe && !hasLockedIsland && isInlineTargetTag(tag, snapshotCurrentTextRef.current);
    const editorSchema = inlineFragment ? htmlInlineSchema : htmlVisualSchema;
    // The authored cell is already the editor host. Parsing it through a table wrapper would
    // flatten its block children into the visual table schema's inline-only cell nodes.
    const blockCell = isInframe && (tag === "td" || tag === "th") && !inlineFragment;
    const parseBlockFragment = (fragment: string, revision: number) =>
      parseHtmlToPm(
        blockCell ? fragment : wrapped(tag, fragment),
        revision,
        SYNTHESISED_FRAGMENT_MAP,
      );
    const initial = inlineFragment
      ? parseInframeInline(snapshotCurrentTextRef.current, Number(snapshot.originalRevision) || 0)
      : parseBlockFragment(snapshotCurrentTextRef.current, Number(snapshot.originalRevision) || 0);
    const markdownRules = markdownBlockRulesPlugin(currentTagRef, changeBlockTypeRef);
    const paste = new Plugin({
      key: new PluginKey("htmlSafeBlockPaste"),
      props: {
        handlePaste(view, event) {
          const html = event.clipboardData?.getData("text/html");
          if (!html) return false;
          event.preventDefault();
          const pasted = sanitizeHtmlPasteFragment(html);
          const parsed = inlineFragment
            ? parseInframeInline(pasted, 0)
            : parseBlockFragment(pasted, 0);
          view.dispatch(view.state.tr.replaceSelection(parsed.slice(0, parsed.content.size)));
          return true;
        },
      },
    });
    const nextState =
      preservedState && preservedState.schema === editorSchema
        ? preservedState
        : EditorState.create({
            schema: editorSchema,
            doc: initial,
            plugins: [
              history(),
              keymap({ "Mod-z": undo, "Mod-y": redo, "Mod-Shift-z": redo }),
              keymap(baseKeymap),
              markdownRules,
              paste,
            ],
          });
    const previewImageSources = new Map<string, string>();
    for (const image of Array.from(mount.querySelectorAll("img[data-vr-authored-src]"))) {
      previewImageSources.set(
        image.getAttribute("data-vr-authored-src") ?? "",
        image.getAttribute("src") ?? "",
      );
    }
    const view = new EditorView(
      { mount },
      {
        state: nextState,
        nodeViews: {
          image(node) {
            const image = mount.ownerDocument.createElement("img");
            image.setAttribute("data-vr-authored-src", node.attrs.src);
            image.setAttribute("src", previewImageSources.get(node.attrs.src) ?? node.attrs.src);
            if (node.attrs.alt) image.setAttribute("alt", node.attrs.alt);
            if (node.attrs.title) image.setAttribute("title", node.attrs.title);
            return { dom: image };
          },
        },
        dispatchTransaction(transaction) {
          if (view.isDestroyed) return;
          const next = view.state.apply(transaction);
          view.updateState(next);
          toolbarStateChangeRef.current?.(toolbarStateFor(view));
          if (transaction.docChanged)
            drafts.update({
              identity,
              currentText: inlineFragment
                ? serializeInframeInline(next.doc as ReturnType<typeof parseInframeInline>)
                : innerFromDoc(
                    (targetTagRef.current ?? snapshotBlockKindRef.current).replace(/^html:/, "") ||
                      "p",
                    next.doc,
                  ),
              targetTag: targetTagRef.current,
            });
        },
      },
    );
    trackHtmlProseMirror(1);
    viewRef.current = view;
    mountedElementRef.current = mount;
    mountedTagRef.current = tag;
    if (!preservedState) {
      const selected = initialTextSelection(view.state, snapshot.selectionBookmark);
      if (selected) view.dispatch(view.state.tr.setSelection(selected));
    }
    toolbarStateChangeRef.current?.(toolbarStateFor(view));
    const onSelectionChange = () => {
      if (viewRef.current !== view) return;
      syncSelectionFromIframeDom(view);
      toolbarStateChangeRef.current?.(toolbarStateFor(view));
    };
    mount.ownerDocument.addEventListener("selectionchange", onSelectionChange);
    let pointerAnchor: number | null = null;
    const applyPointerSelection = (anchor: number, head: number) => {
      const next = TextSelection.between(
        view.state.doc.resolve(positionForTextOffset(view.state.doc, anchor)),
        view.state.doc.resolve(positionForTextOffset(view.state.doc, head)),
      );
      if (!next.eq(view.state.selection)) view.dispatch(view.state.tr.setSelection(next));
      toolbarStateChangeRef.current?.(toolbarStateFor(view));
    };
    const onPointerDown = (event: MouseEvent) => {
      pointerAnchor = htmlTextOffsetFromPoint(view.dom, event.clientX, event.clientY) ?? null;
    };
    const onPointerUp = (event: MouseEvent) => {
      const anchor = pointerAnchor;
      pointerAnchor = null;
      const head = htmlTextOffsetFromPoint(view.dom, event.clientX, event.clientY);
      if (anchor === null || head === undefined || anchor === head) return;
      applyPointerSelection(anchor, head);
    };
    const onDoubleClick = (event: MouseEvent) => {
      const word = htmlWordSelectionFromPoint(view.dom, event.clientX, event.clientY);
      if (word) applyPointerSelection(word.from, word.to);
    };
    view.dom.addEventListener("mousedown", onPointerDown, true);
    view.dom.addEventListener("mouseup", onPointerUp, true);
    view.dom.addEventListener("dblclick", onDoubleClick, true);
    view.focus();
    if (preservedState && !preservedState.selection.empty) {
      view.dispatch(view.state.tr.setSelection(preservedState.selection));
    }
    return () => {
      mount.ownerDocument.removeEventListener("selectionchange", onSelectionChange);
      view.dom.removeEventListener("mousedown", onPointerDown, true);
      view.dom.removeEventListener("mouseup", onPointerUp, true);
      view.dom.removeEventListener("dblclick", onDoubleClick, true);
      if (currentTagRef.current !== mountedTag) return;
      // Parent target replacement in same iframe generation keeps current view alive. A changed
      // generation still destroys old view even when fresh target has already been resolved.
      if (
        isInframe &&
        lifecycleGenerationRef.current === iframeGeneration &&
        targetElementRef.current !== mount
      ) {
        return;
      }
      view.destroy();
      viewRef.current = null;
      if (mountedElementRef.current === mount) mountedElementRef.current = null;
      trackHtmlProseMirror(-1);
      // On cancel/discard, restore original DOM if document is still same generation. On Done the
      // source commits & iframe rebuilds from canonical, so restoration is moot.
      const original = originalTargetRef.current;
      const currentTarget = targetElementRef.current;
      if (isInframe && original && currentTarget?.isConnected) {
        try {
          const restored = restoreTargetSnapshot(currentTarget, original);
          targetElementRef.current = restored;
          if (restored !== currentTarget) onIframeTargetChangeRef.current?.(restored);
        } catch {
          // Doc may be unmounted; ignore.
        }
      }
    };
  }, [
    drafts,
    identity,
    snapshot.originalRevision,
    tag,
    mountVersion,
    inframeMount,
    iframeGeneration,
    isInframe,
    isStandalone,
    visualStyle,
  ]);

  // A dependency change during heading replacement intentionally leaves old lifecycle cleanup
  // pending until replacement view mounts. Component unmount still needs unconditional teardown.
  useEffect(() => {
    return () => {
      const view = viewRef.current;
      if (!view) return;
      view.destroy();
      viewRef.current = null;
      mountedElementRef.current = null;
      trackHtmlProseMirror(-1);
      const original = originalTargetRef.current;
      const currentTarget = targetElementRef.current;
      if (isInframe && original && currentTarget?.isConnected) {
        try {
          const restored = restoreTargetSnapshot(currentTarget, original);
          targetElementRef.current = restored;
          if (restored !== currentTarget) onIframeTargetChangeRef.current?.(restored);
        } catch {
          // Doc may be unmounted; ignore.
        }
      }
    };
    // This effect is deliberately mount-scoped: refs hold current view/target across replacements.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isInframe]);

  // A block rename is a draft-only operation until Done. Update the existing PM node in place so
  // its rendered tag/heading level changes immediately without remounting the editor or losing the
  // user's selection/caret. The canonical HTML source is touched only by the registry commit path.
  useEffect(() => {
    const view = viewRef.current;
    if (isInframe) {
      const current = targetElementRef.current;
      if (!current || current.tagName.toLowerCase() === tag) return;
      const replacement = current.ownerDocument.createElement(tag);
      const original = originalTargetRef.current;
      if (original) {
        for (const [name, value] of original.attributes) replacement.setAttribute(name, value);
      } else {
        for (const attribute of Array.from(current.attributes)) {
          replacement.setAttribute(attribute.name, attribute.value);
        }
      }
      current.replaceWith(replacement);
      targetElementRef.current = replacement;
      onIframeTargetChangeRef.current?.(replacement);
      // EditorView cannot change its mounted root tag. The lifecycle effect remounts the same
      // EditorState on this replacement, preserving history & logical selection.
      setMountVersion((version) => version + 1);
      return;
    }
    if (!view) return;
    const first = view.state.doc.firstChild;
    const nodeType = visualNodeForTag(tag);
    if (!first || !nodeType) return;
    const attrs = visualAttrsForTag(tag, first.attrs as Record<string, unknown>);
    const sameType = first.type === nodeType;
    const sameHeadingLevel =
      !/^h[1-6]$/.test(tag) || Number(first.attrs.level) === Number(attrs.level);
    if (sameType && sameHeadingLevel) return;
    view.dispatch(view.state.tr.setNodeMarkup(0, nodeType, attrs));
    toolbarStateChangeRef.current?.(toolbarStateFor(view));
  }, [isInframe, tag]);

  const discard = async () => {
    const result = await drafts.resolve(identity, "view-change", "discard");
    if (result.outcome === "discarded" || result.outcome === "missing") onClose();
  };
  const done = async () => {
    const view = viewRef.current;
    if (view)
      drafts.update({
        identity,
        currentText:
          isInlineTargetTag(tag) || (isInframe && isInlineTargetTag(tag, snapshot.originalText))
            ? serializeInframeInline(view.state.doc as ReturnType<typeof parseInframeInline>)
            : innerFromDoc(tag, view.state.doc),
        targetTag: targetTagRef.current,
      });
    const result = await drafts.resolve(identity, "manual-save", "done");
    if (result.outcome === "done") onClose();
    else if (result.outcome === "failed") setError(result.reason);
  };
  const changeBlockType = (nextTag: string) => {
    const view = viewRef.current;
    const currentInner = view
      ? isInframe && isInlineTargetTag(tag, snapshot.originalText)
        ? serializeInframeInline(view.state.doc as ReturnType<typeof parseInframeInline>)
        : innerFromDoc(tag, view.state.doc)
      : snapshot.currentText;
    targetTagRef.current = nextTag;
    drafts.update({ identity, currentText: currentInner, targetTag: nextTag });
  };
  changeBlockTypeRef.current = changeBlockType;
  const mark = (name: "strong" | "em" | "code" | "strike" | "underline") => {
    const view = viewRef.current;
    const type = view?.state.schema.marks[name];
    if (view) {
      syncSelectionFromIframeDom(view);
    }
    if (view && type) {
      toggleMark(type)(view.state, view.dispatch, view);
    }
  };
  const applyTextStyle = (property: HtmlTextStyleProperty, rawValue: string | null) => {
    const view = viewRef.current;
    const type = view?.state.schema.marks.textStyle;
    if (!view || !type) return;
    syncSelectionFromIframeDom(view);
    const value = safeHtmlTextStyleValue(property, rawValue);
    const { from, to, empty } = view.state.selection;
    let tr = view.state.tr;
    if (empty) {
      const marks = view.state.storedMarks ?? view.state.selection.$from.marks();
      const existing = type.isInSet(marks);
      const attrs: HtmlTextStyleAttrs = {
        ...EMPTY_HTML_TEXT_STYLE,
        ...(existing?.attrs as Partial<HtmlTextStyleAttrs> | undefined),
        [property]: value,
      };
      tr = tr.removeStoredMark(type);
      if (hasHtmlTextStyle(attrs)) tr = tr.addStoredMark(type.create(attrs));
    } else {
      view.state.doc.nodesBetween(from, to, (node, pos) => {
        if (!node.isText) return;
        const segmentFrom = Math.max(from, pos);
        const segmentTo = Math.min(to, pos + node.nodeSize);
        if (segmentFrom >= segmentTo) return;
        const existing = type.isInSet(node.marks);
        const attrs: HtmlTextStyleAttrs = {
          ...EMPTY_HTML_TEXT_STYLE,
          ...(existing?.attrs as Partial<HtmlTextStyleAttrs> | undefined),
          [property]: value,
        };
        tr = tr.removeMark(segmentFrom, segmentTo, type);
        if (hasHtmlTextStyle(attrs)) tr = tr.addMark(segmentFrom, segmentTo, type.create(attrs));
      });
    }
    view.dispatch(tr);
    view.focus();
    toolbarStateChangeRef.current?.(toolbarStateFor(view));
  };

  // Selection captured when the "Add link" prompt opens: focusing the prompt input moves DOM
  // focus off the editor, so the range is stored here and applied on submit. The new <a> is a
  // ProseMirror mark that serializes into the block's inner HTML on Done and commits through the
  // normal replaceText transaction — no separate source path, no FragmentSourceMap needed.
  const pendingLinkRange = useRef<{ from: number; to: number } | null>(null);
  const applyLinkMark = (href: string) => {
    const view = viewRef.current;
    const type = view?.state.schema.marks.link;
    const range = pendingLinkRange.current;
    pendingLinkRange.current = null;
    if (!view || !type || !range || range.from >= range.to || !href) return;
    view.dispatch(view.state.tr.addMark(range.from, range.to, type.create({ href })));
    view.focus();
  };

  const [prompt, setPrompt] = useState<{
    kind: "link" | "addLink" | "image" | "imageInsert";
    initial: string;
  } | null>(null);
  const [promptValue, setPromptValue] = useState("");
  const promptInputRef = useRef<HTMLInputElement>(null);
  const [propertiesOpen, setPropertiesOpen] = useState(false);
  const [attributeName, setAttributeName] = useState("");
  const [attributeValue, setAttributeValue] = useState("");
  const attributeNameRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (prompt) promptInputRef.current?.focus();
  }, [prompt]);
  useEffect(() => {
    if (propertiesOpen) attributeNameRef.current?.focus();
  }, [propertiesOpen]);
  const openPrompt = (kind: "link" | "addLink" | "image" | "imageInsert", initial = "") => {
    setPrompt({ kind, initial });
    setPromptValue(initial);
  };
  const requestAddLink = () => {
    const view = viewRef.current;
    if (view) syncSelectionFromIframeDom(view);
    if (!view || view.state.selection.empty) {
      setError("Select some text first, then add a link.");
      return;
    }
    const { from, to } = view.state.selection;
    pendingLinkRange.current = { from, to };
    openPrompt("addLink", "https://");
  };
  useImperativeHandle(ref, () => ({
    focus: () => viewRef.current?.focus(),
    changeBlockType,
    toggleMark: mark,
    applyTextStyle,
    requestAddLink,
    requestEditLink: (href) => openPrompt("link", href),
    requestReplaceImage: (src) => openPrompt("image", src),
    requestInsertImage: () => openPrompt("imageInsert", ""),
    requestProperties: () => {
      setError(null);
      setAttributeName("");
      setAttributeValue("");
      setPropertiesOpen(true);
    },
    done,
    cancel: discard,
  }));
  const submitPrompt = () => {
    if (!prompt) return;
    if (prompt.kind === "addLink") {
      applyLinkMark(promptValue.trim());
      setPrompt(null);
      return;
    }
    if (!onRichOp) return;
    if (prompt.kind === "link") onRichOp({ kind: "setLink", href: promptValue.trim() });
    else if (prompt.kind === "image") onRichOp({ kind: "imageReplace", src: promptValue.trim() });
    else if (prompt.kind === "imageInsert")
      onRichOp({ kind: "imageInsert", src: promptValue.trim(), position: "end" });
    setPrompt(null);
  };
  const submitProperties = () => {
    const name = attributeName.trim().toLowerCase();
    if (!name) {
      setError("Attribute needs a name.");
      return;
    }
    onRichOp?.({ kind: "setAttribute", name, value: attributeValue });
    setPropertiesOpen(false);
  };
  const dismissOverlayOnEscape = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "Escape" || event.defaultPrevented) return;
    event.preventDefault();
    event.stopPropagation();
    if (propertiesOpen) setPropertiesOpen(false);
    else setPrompt(null);
    viewRef.current?.focus();
  };

  const originalTag = snapshot.blockKind.replace(/^html:/, "").toLowerCase();
  const editorStyle = editorTypographyForTag(tag, originalTag, visualStyle);

  // A stale draft (source changed on disk while it was open) keeps its text and shows the registry
  // reason through the same alert as a failed Done, matching the Markdown block editors' marker.
  const alertText =
    error ??
    (snapshot.status === "stale"
      ? (snapshot.reason ?? "Block changed outside this editor.")
      : null);

  // CE-5 in-frame path: editor lives inside the safe iframe's target element
  // at its document position (spatial continuity, SC-2). Parent only shows
  // prompts/properties chrome; no fixed overlay, no geometry translation.
  if (isInframe) {
    return (
      <>
        <div
          ref={host}
          style={{ display: "none" }}
          aria-hidden="true"
          data-testid="html-safe-block-editor-inframe-placeholder"
        />
        {alertText && (
          <p
            role="alert"
            data-testid="html-block-draft-status"
            className="absolute left-1/2 top-2 z-20 -translate-x-1/2 rounded bg-panel px-2 py-1 text-xs text-red-300 shadow-xl"
          >
            {alertText}
          </p>
        )}
        {prompt && (
          <div
            className="absolute left-1/2 top-2 z-20 flex -translate-x-1/2 items-center gap-1 rounded border border-copper bg-panel-2 p-1 text-xs shadow-xl"
            data-testid="html-rich-edit-prompt"
            onKeyDown={dismissOverlayOnEscape}
          >
            <span className="px-1 text-muted">
              {prompt.kind === "link" || prompt.kind === "addLink" ? "Link URL" : "Image URL"}
            </span>
            <input
              ref={promptInputRef}
              aria-label={
                prompt.kind === "link" || prompt.kind === "addLink" ? "Link URL" : "Image URL"
              }
              value={promptValue}
              onChange={(event) => setPromptValue(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  submitPrompt();
                } else if (event.key === "Escape") {
                  event.preventDefault();
                  event.stopPropagation();
                  setPrompt(null);
                  viewRef.current?.focus();
                }
              }}
              onMouseDown={(event) => event.stopPropagation()}
              className="w-52 rounded border border-line bg-ink px-2 py-1 text-paper outline-none focus:border-copper"
            />
            <button
              type="button"
              aria-label="Apply prompt"
              className="vr-press rounded border border-line px-2 py-1 text-muted hover:text-paper"
              onMouseDown={(event) => event.preventDefault()}
              onClick={submitPrompt}
            >
              Apply
            </button>
            <button
              type="button"
              aria-label="Cancel prompt"
              className="vr-press rounded border border-line px-2 py-1 text-muted hover:text-paper"
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => setPrompt(null)}
            >
              Cancel
            </button>
          </div>
        )}
        {propertiesOpen && (
          <div
            className="absolute left-1/2 top-2 z-20 w-72 -translate-x-1/2 rounded border border-copper bg-panel-2 p-2 text-xs shadow-xl"
            role="group"
            aria-label="HTML properties"
            onKeyDown={dismissOverlayOnEscape}
            onMouseDown={(event) => event.stopPropagation()}
            onClick={(event) => event.stopPropagation()}
          >
            <div className="mb-2 text-muted">
              <span className="text-paper">Properties</span>
              <span className="ml-2 font-mono">&lt;{propertiesTag}&gt;</span>
            </div>
            <div className="mb-2 space-y-1" data-testid="html-authored-attributes">
              <div className="text-chrome-xs uppercase tracking-wide text-muted">
                Authored attributes
              </div>
              {propertyAttributes?.length ? (
                propertyAttributes.map((attribute) => (
                  <div
                    key={attribute.name}
                    className="grid grid-cols-[76px_minmax(0,1fr)_42px] items-center gap-1"
                  >
                    <span className="truncate font-mono text-muted" title={attribute.name}>
                      {attribute.name}
                    </span>
                    <input
                      aria-label={`Attribute value: ${attribute.name}`}
                      defaultValue={attribute.value}
                      className="min-w-0 rounded border border-line bg-ink px-2 py-1 text-paper outline-none focus:border-copper"
                      onKeyDown={(event) => {
                        if (event.key === "Enter") {
                          event.preventDefault();
                          onRichOp?.({
                            kind: "setAttribute",
                            name: attribute.name,
                            value: event.currentTarget.value,
                          });
                          setPropertiesOpen(false);
                        }
                      }}
                    />
                    <button
                      type="button"
                      aria-label={`Remove attribute: ${attribute.name}`}
                      className="vr-press rounded border border-line px-1 py-1 text-muted hover:text-paper"
                      onMouseDown={(event) => event.preventDefault()}
                      onClick={() => {
                        onRichOp?.({ kind: "removeAttribute", name: attribute.name });
                        setPropertiesOpen(false);
                      }}
                    >
                      Remove
                    </button>
                  </div>
                ))
              ) : (
                <div className="text-muted">No authored attributes</div>
              )}
            </div>
            <div className="mb-1 text-chrome-xs uppercase tracking-wide text-muted">Add attribute</div>
            <div className="grid grid-cols-[88px_minmax(0,1fr)] items-center gap-1.5">
              <label htmlFor="html-property-name-inframe" className="text-muted">
                Name
              </label>
              <input
                ref={attributeNameRef}
                id="html-property-name-inframe"
                aria-label="Attribute name"
                value={attributeName}
                onChange={(event) => setAttributeName(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Escape") {
                    event.preventDefault();
                    event.stopPropagation();
                    setPropertiesOpen(false);
                  }
                }}
                placeholder="data-label"
                className="min-w-0 rounded border border-line bg-ink px-2 py-1 text-paper outline-none focus:border-copper"
              />
              <label htmlFor="html-property-value-inframe" className="text-muted">
                Value
              </label>
              <input
                id="html-property-value-inframe"
                aria-label="Attribute value"
                value={attributeValue}
                onChange={(event) => setAttributeValue(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    submitProperties();
                  } else if (event.key === "Escape") {
                    event.preventDefault();
                    event.stopPropagation();
                    setPropertiesOpen(false);
                  }
                }}
                placeholder="value"
                className="min-w-0 rounded border border-line bg-ink px-2 py-1 text-paper outline-none focus:border-copper"
              />
            </div>
            <div className="mt-2 flex justify-end gap-1">
              <button
                type="button"
                aria-label="Edit target source"
                className="vr-press mr-auto rounded border border-line px-2 py-1 text-muted hover:text-paper"
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => {
                  setPropertiesOpen(false);
                  void onEditSource?.();
                }}
              >
                Edit target source
              </button>
              <button
                type="button"
                aria-label="Cancel properties"
                className="vr-press rounded border border-line px-2 py-1 text-muted hover:text-paper"
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => setPropertiesOpen(false)}
              >
                Cancel
              </button>
              <button
                type="button"
                aria-label="Apply properties"
                className="vr-press rounded border border-line px-2 py-1 text-muted hover:text-paper"
                onMouseDown={(event) => event.preventDefault()}
                onClick={submitProperties}
              >
                Apply
              </button>
            </div>
          </div>
        )}
      </>
    );
  }

  // A Safe target may briefly be unresolved while iframe DOM is loading. Do not mount any parent
  // overlay during that gap; render only after in-frame target resolution.
  if (!isInframe && !isStandalone) return null;

  // Click elsewhere must NOT silently commit: no document-level click handler commits.
  // Only Done commits; Cancel/Escape discard; focus loss is ignored.
  // Standalone test seam only; production always mounts in Safe iframe flow.
  return (
    <section
      className="relative rounded-sm border border-copper shadow-[0_0_0_2px_rgba(184,112,72,0.22)]"
      data-testid="html-safe-block-editor"
      role="group"
      aria-label="Inline HTML editor"
      style={editorStyle}
      onKeyDown={(event) => {
        if (event.defaultPrevented) return;
        if (event.key === "Escape") {
          event.preventDefault();
          if (prompt) setPrompt(null);
          else if (propertiesOpen) setPropertiesOpen(false);
          else void discard();
        } else if (event.altKey && (event.key === "ArrowUp" || event.key === "ArrowDown")) {
          // Sanctioned "minus drag-and-drop" reorder shortcut; the editor owns Alt+arrow
          // while a block session is open. Delegates to the same onRichOp path the
          // toolbar's Move up/down buttons use — no separate mutation authority.
          event.preventDefault();
          onRichOp?.({ kind: event.key === "ArrowUp" ? "moveUp" : "moveDown" });
        }
      }}
      onMouseDown={(event) => event.stopPropagation()}
      onClick={(event) => event.stopPropagation()}
    >
      <div
        ref={host}
        className="min-h-[inherit] [&_.ProseMirror]:min-h-[inherit] [&_.ProseMirror]:outline-none [&_.ProseMirror>*:first-child]:mt-0 [&_.ProseMirror>*:last-child]:mb-0"
        style={editorStyle}
      />
      {alertText && (
        <p
          role="alert"
          data-testid="html-block-draft-status"
          className="absolute left-0 top-full mt-1 rounded bg-panel px-2 py-1 text-xs text-red-300 shadow-xl"
        >
          {alertText}
        </p>
      )}
      {prompt && (
        <div
          className="absolute left-0 top-full z-20 mt-1 flex items-center gap-1 rounded border border-copper bg-panel-2 p-1 text-xs shadow-xl"
          onKeyDown={dismissOverlayOnEscape}
        >
          <span className="px-1 text-muted">
            {prompt.kind === "link" || prompt.kind === "addLink" ? "Link URL" : "Image URL"}
          </span>
          <input
            ref={promptInputRef}
            aria-label={
              prompt.kind === "link" || prompt.kind === "addLink" ? "Link URL" : "Image URL"
            }
            value={promptValue}
            onChange={(event) => setPromptValue(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                submitPrompt();
              } else if (event.key === "Escape") {
                event.preventDefault();
                event.stopPropagation();
                setPrompt(null);
              }
            }}
            onMouseDown={(event) => event.stopPropagation()}
            className="w-52 rounded border border-line bg-ink px-2 py-1 text-paper outline-none focus:border-copper"
          />
          <button
            type="button"
            aria-label="Apply prompt"
            className="vr-press rounded border border-line px-2 py-1 text-muted hover:text-paper"
            onMouseDown={(event) => event.preventDefault()}
            onClick={submitPrompt}
          >
            Apply
          </button>
          <button
            type="button"
            aria-label="Cancel prompt"
            className="vr-press rounded border border-line px-2 py-1 text-muted hover:text-paper"
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => setPrompt(null)}
          >
            Cancel
          </button>
        </div>
      )}
      {propertiesOpen && (
        <div
          className="absolute left-0 top-full z-20 mt-1 w-72 rounded border border-copper bg-panel-2 p-2 text-xs shadow-xl"
          role="group"
          aria-label="HTML properties"
          onKeyDown={dismissOverlayOnEscape}
          onMouseDown={(event) => event.stopPropagation()}
          onClick={(event) => event.stopPropagation()}
        >
          <div className="mb-2 text-muted">
            <span className="text-paper">Properties</span>
            <span className="ml-2 font-mono">&lt;{propertiesTag}&gt;</span>
          </div>
          <div className="mb-2 space-y-1" data-testid="html-authored-attributes">
            <div className="text-chrome-xs uppercase tracking-wide text-muted">
              Authored attributes
            </div>
            {propertyAttributes?.length ? (
              propertyAttributes.map((attribute) => (
                <div
                  key={attribute.name}
                  className="grid grid-cols-[76px_minmax(0,1fr)_42px] items-center gap-1"
                >
                  <span className="truncate font-mono text-muted" title={attribute.name}>
                    {attribute.name}
                  </span>
                  <input
                    aria-label={`Attribute value: ${attribute.name}`}
                    defaultValue={attribute.value}
                    className="min-w-0 rounded border border-line bg-ink px-2 py-1 text-paper outline-none focus:border-copper"
                    onKeyDown={(event) => {
                      if (event.key === "Enter") {
                        event.preventDefault();
                        onRichOp?.({
                          kind: "setAttribute",
                          name: attribute.name,
                          value: event.currentTarget.value,
                        });
                        setPropertiesOpen(false);
                      }
                    }}
                  />
                  <button
                    type="button"
                    aria-label={`Remove attribute: ${attribute.name}`}
                    className="vr-press rounded border border-line px-1 py-1 text-muted hover:text-paper"
                    onMouseDown={(event) => event.preventDefault()}
                    onClick={() => {
                      onRichOp?.({ kind: "removeAttribute", name: attribute.name });
                      setPropertiesOpen(false);
                    }}
                  >
                    Remove
                  </button>
                </div>
              ))
            ) : (
              <div className="text-muted">No authored attributes</div>
            )}
          </div>
          <div className="mb-1 text-chrome-xs uppercase tracking-wide text-muted">Add attribute</div>
          <div className="grid grid-cols-[88px_minmax(0,1fr)] items-center gap-1.5">
            <label htmlFor="html-property-name" className="text-muted">
              Name
            </label>
            <input
              ref={attributeNameRef}
              id="html-property-name"
              aria-label="Attribute name"
              value={attributeName}
              onChange={(event) => setAttributeName(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Escape") {
                  event.preventDefault();
                  event.stopPropagation();
                  setPropertiesOpen(false);
                }
              }}
              placeholder="data-label"
              className="min-w-0 rounded border border-line bg-ink px-2 py-1 text-paper outline-none focus:border-copper"
            />
            <label htmlFor="html-property-value" className="text-muted">
              Value
            </label>
            <input
              id="html-property-value"
              aria-label="Attribute value"
              value={attributeValue}
              onChange={(event) => setAttributeValue(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  submitProperties();
                } else if (event.key === "Escape") {
                  event.preventDefault();
                  event.stopPropagation();
                  setPropertiesOpen(false);
                }
              }}
              placeholder="value"
              className="min-w-0 rounded border border-line bg-ink px-2 py-1 text-paper outline-none focus:border-copper"
            />
          </div>
          <div className="mt-2 flex justify-end gap-1">
            <button
              type="button"
              aria-label="Edit target source"
              className="vr-press mr-auto rounded border border-line px-2 py-1 text-muted hover:text-paper"
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => {
                setPropertiesOpen(false);
                void onEditSource?.();
              }}
            >
              Edit target source
            </button>
            <button
              type="button"
              aria-label="Cancel properties"
              className="vr-press rounded border border-line px-2 py-1 text-muted hover:text-paper"
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => setPropertiesOpen(false)}
            >
              Cancel
            </button>
            <button
              type="button"
              aria-label="Apply properties"
              className="vr-press rounded border border-line px-2 py-1 text-muted hover:text-paper"
              onMouseDown={(event) => event.preventDefault()}
              onClick={submitProperties}
            >
              Apply
            </button>
          </div>
        </div>
      )}
    </section>
  );
}
