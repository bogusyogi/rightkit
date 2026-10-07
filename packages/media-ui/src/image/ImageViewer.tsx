import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type SyntheticEvent,
  type WheelEvent as ReactWheelEvent,
} from "react";
import { acquireResource, useMediaAdapters, type MediaAdapters } from "../adapters.js";
import type { ImageSaveFunction } from "./imageSave.js";
import { defaultCrop } from "./cropMath.js";
import { ImageViewerShell } from "./ImageViewerShell.js";
import type { ImageTool } from "./ImageViewerToolbar.js";
import {
  exportImageEdit,
  hasImageEdits,
  initialImageEdit,
  sameImageEdit,
  cropSourceSize,
  type ImageAdjustments,
  type CropRect,
  type ImageEdit,
  type TextLayer,
  type ExportResize,
} from "./imageEdit.js";
import {
  actualSizeTransform,
  canPan,
  clampOffset,
  fitTransform,
  initialImageTransform,
  rotateTransform,
  zoomAtPoint,
  type ImageSize,
  type ImageTransform,
} from "./imageViewerState.js";
import { DEFAULT_IMAGE_FONT_ID } from "./imageFonts.js";
import { useDecodedImageSwap } from "./useDecodedImageSwap.js";
import { useImageSiblingPaging } from "./useImageSiblingPaging.js";
import { getImageEditSnapshot, setImageEditSnapshot } from "./imageEditStore.js";

const CENTER_ZOOM_STEP = 1.2;

export interface ImageViewerProps {
  /** Stable document identity — keys the in-memory edit-recipe/history store. */
  path: string;
  /** Display name used for the stage label and save-name suggestions. */
  displayName?: string;
  /** Loadable image URL (already resolved for the webview, e.g. asset:// or stream URL). */
  src: string;
  /** Optional display-optimized variant; export/save always renders from `src`. */
  displaySrc?: string;
  active?: boolean;
  /** Called whenever the edit recipe or its history changes. */
  onChange?: () => void;
  /** Register the export function the host's Save / Save As calls. */
  registerSave?: (fn: ImageSaveFunction | null) => void;
  /** Page to a sibling image, replacing this viewer in place. */
  onOpenSibling?: (currentPath: string, targetPath: string) => void;
  /** View-only mode: zoom/fit/paging stay; all edit affordances are hidden. */
  readOnly?: boolean;
  /**
   * Window event name that triggers a print of the rendered image (requires the
   * `printImage` adapter). Default "media-ui:print-image".
   */
  printEventName?: string;
  /**
   * Render the toolbar (`.rk-iv-toolbar`) into this element (via a portal)
   * instead of inside the viewer, so hosts can place it in their own chrome.
   * While it is null/undefined the toolbar stays inside the viewer.
   */
  toolbarContainer?: HTMLElement | null;
  adapters?: MediaAdapters;
}

export function ImageViewer(props: ImageViewerProps) {
  const {
    path,
    displayName,
    src,
    displaySrc: displaySrcProp,
    active = true,
    onChange,
    registerSave,
    onOpenSibling,
    readOnly = false,
    printEventName = "media-ui:print-image",
    toolbarContainer,
  } = props;
  const adapters = useMediaAdapters(props.adapters);
  const displaySrc = displaySrcProp || src;
  const exportSrc = src || displaySrc;
  const fonts = adapters.bundledFonts ?? [];
  const [shownSrc, setShownSrc] = useState(displaySrc);
  const frameRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{
    pointerId: number;
    startX: number;
    startY: number;
    originX: number;
    originY: number;
  } | null>(null);
  const textSeqRef = useRef(0);
  const [viewport, setViewport] = useState<ImageSize>({ width: 0, height: 0 });
  const [imageSize, setImageSize] = useState<ImageSize | null>(null);
  const [view, setView] = useState<ImageTransform>(() => initialImageTransform());
  const initialSnapshot = getImageEditSnapshot(path);
  const [edit, setEditState] = useState<ImageEdit>(
    () => initialSnapshot?.edit ?? initialImageEdit(),
  );
  const editRef = useRef(edit);
  const setEdit = useCallback(
    (next: ImageEdit | ((current: ImageEdit) => ImageEdit)) => {
      const value = typeof next === "function" ? next(editRef.current) : next;
      editRef.current = value;
      setEditState(value);
    },
    [],
  );
  const [tool, setTool] = useState<ImageTool>("view");
  const activeTool = readOnly ? "view" : tool;
  const [cropRatio, setCropRatio] = useState<number | null>(null);
  const [cropDraft, setCropDraft] = useState<CropRect | null>(null);
  const [selectedText, setSelectedText] = useState<string | null>(null);
  const [dragging, setDragging] = useState(false);
  const [canUndoEdit, setCanUndoEdit] = useState(() => (initialSnapshot?.undo.length ?? 0) > 0);
  const [canRedoEdit, setCanRedoEdit] = useState(() => (initialSnapshot?.redo.length ?? 0) > 0);
  const undoStackRef = useRef<ImageEdit[]>(
    initialSnapshot?.undo ? [...initialSnapshot.undo] : [],
  );
  const redoStackRef = useRef<ImageEdit[]>(
    initialSnapshot?.redo ? [...initialSnapshot.redo] : [],
  );
  const edited = hasImageEdits(edit);

  // The viewer registers one resource; its edit recipe + history live in the
  // path-keyed store so it survives unmount while dirty.
  const censusRef = useRef<ReturnType<typeof acquireResource> | null>(null);
  const editedRef = useRef(edited);
  editedRef.current = edited;
  useEffect(() => {
    const handle = acquireResource(adapters, {
      kind: "image-viewer",
      documentPath: path,
      dirty: editedRef.current,
      stateExternalized: true,
    });
    censusRef.current = handle;
    return () => {
      handle.release();
      censusRef.current = null;
    };
  }, [adapters, path]);
  // Dirty changes are pushed to the live handle; no re-acquire.
  useEffect(() => {
    censusRef.current?.update({ dirty: edited });
  }, [edited]);

  // Hydrate from the store when the path changes (sibling paging replaces the viewer in place).
  useEffect(() => {
    const snapshot = getImageEditSnapshot(path);
    if (snapshot) {
      undoStackRef.current = [...snapshot.undo];
      redoStackRef.current = [...snapshot.redo];
      setCanUndoEdit(snapshot.undo.length > 0);
      setCanRedoEdit(snapshot.redo.length > 0);
      setEdit(snapshot.edit);
    } else {
      undoStackRef.current = [];
      redoStackRef.current = [];
      setCanUndoEdit(false);
      setCanRedoEdit(false);
      setEdit(initialImageEdit());
    }
    setTool("view");
    setCropDraft(null);
    setSelectedText(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [path, setEdit]);

  const displaySize = useMemo(
    () => (imageSize ? cropSourceSize(imageSize, edit.crop) : null),
    [edit.crop, imageSize],
  );
  const stageSize = activeTool === "crop" && imageSize ? imageSize : displaySize;
  const selectedLayer = useMemo(
    () => edit.text.find((layer) => layer.id === selectedText) ?? null,
    [edit.text, selectedText],
  );
  const ready = stageSize !== null && viewport.width > 0 && viewport.height > 0;
  const panning =
    stageSize && ready && activeTool === "view" ? canPan(view, viewport, stageSize) : false;
  const { siblingIndex, siblingTotal, canPrev, canNext, goSibling, pageFromHorizontalWheel } =
    useImageSiblingPaging({
      active,
      currentPath: path,
      tool: activeTool,
      panning: !!panning,
      adapters,
      onOpenSibling,
    });

  const resetForDecodedImage = useCallback(
    (nextSrc: string, nextSize: ImageSize) => {
      const snapshot = getImageEditSnapshot(path);
      if (snapshot) {
        undoStackRef.current = [...snapshot.undo];
        redoStackRef.current = [...snapshot.redo];
        setCanUndoEdit(snapshot.undo.length > 0);
        setCanRedoEdit(snapshot.redo.length > 0);
        setEdit(snapshot.edit);
      } else {
        undoStackRef.current = [];
        redoStackRef.current = [];
        setCanUndoEdit(false);
        setCanRedoEdit(false);
        const nextEdit = initialImageEdit();
        setEdit(nextEdit);
        setImageEditSnapshot(path, { edit: nextEdit, undo: [], redo: [] });
      }
      setTool("view");
      setCropDraft(null);
      setSelectedText(null);
      setImageSize(nextSize);
      setView(initialImageTransform());
      setShownSrc(nextSrc);
    },
    [path, setEdit],
  );
  useDecodedImageSwap({
    documentPath: path,
    displaySrc,
    shownSrc,
    imageSize,
    adapters,
    onDecodedSwap: resetForDecodedImage,
  });

  useEffect(() => {
    const frame = frameRef.current;
    if (!frame) return;
    const update = () => setViewport({ width: frame.clientWidth, height: frame.clientHeight });
    update();
    const observer = new ResizeObserver(update);
    observer.observe(frame);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (!stageSize || !ready) return;
    setView((current) =>
      current.mode === "fit"
        ? fitTransform(current, viewport, stageSize)
        : clampOffset(current, viewport, stageSize),
    );
  }, [ready, stageSize, viewport]);

  useEffect(() => {
    if (!active || !adapters.printImage) return;
    const onPrint = (event: Event) => {
      if (!exportSrc || shownSrc !== displaySrc) return;
      event.preventDefault();
      void exportImageEdit(exportSrc, edit, "png", fonts, imageSize)
        .then((payload) => {
          const blob = new Blob([payload.buffer], { type: payload.mime });
          return new Promise<string>((resolve, reject) => {
            const reader = new FileReader();
            reader.onload = () => resolve(reader.result as string);
            reader.onerror = () => reject(reader.error);
            reader.readAsDataURL(blob);
          });
        })
        .then((dataUrl) => adapters.printImage?.(dataUrl))
        .then((printed) => {
          if (printed) return;
          adapters.logError?.("print:failed");
          adapters.notify?.("Couldn't open print dialog", "error");
        })
        .catch(() => {
          adapters.logError?.("print:failed");
          adapters.notify?.("Couldn't open print dialog", "error");
        });
    };
    window.addEventListener(printEventName, onPrint);
    return () => window.removeEventListener(printEventName, onPrint);
  }, [active, adapters, exportSrc, edit, fonts, printEventName, imageSize, shownSrc, displaySrc]);

  useLayoutEffect(() => {
    if (!registerSave) return;
    // While a new display image is still decoding the recipe belongs to the
    // previous image; exporting then would render the wrong content.
    if (!exportSrc || shownSrc !== displaySrc) {
      registerSave(null);
      return;
    }
    const editSpace = imageSize;
    registerSave((outputExt = "png") => exportImageEdit(exportSrc, edit, outputExt, fonts, editSpace));
    return () => registerSave(null);
  }, [edit, exportSrc, registerSave, fonts, imageSize, shownSrc, displaySrc]);

  const mutateEdit = useCallback(
    (fn: (current: ImageEdit) => ImageEdit) => {
      setEdit((current) => {
        const next = fn(current);
        if (!sameImageEdit(next, current)) {
          undoStackRef.current.push(current);
          redoStackRef.current = []; // a fresh edit forks history — drop the redo branch
          setCanRedoEdit(false);
          setCanUndoEdit(true);
          setImageEditSnapshot(path, { edit: next, undo: [...undoStackRef.current], redo: [] });
          onChange?.();
        }
        return next;
      });
    },
    [path, onChange, setEdit],
  );

  const withReadyState = useCallback(
    (fn: (current: ImageTransform, viewport: ImageSize, image: ImageSize) => ImageTransform) => {
      if (!stageSize || viewport.width <= 0 || viewport.height <= 0) return;
      setView((current) => fn(current, viewport, stageSize));
    },
    [stageSize, viewport],
  );

  const fit = useCallback(
    () => withReadyState((current, v, image) => fitTransform(current, v, image)),
    [withReadyState],
  );
  const actual = useCallback(
    () => withReadyState((current, v, image) => actualSizeTransform(current, v, image)),
    [withReadyState],
  );
  const zoomAtCenter = useCallback(
    (factor: number) =>
      withReadyState((current, v, image) =>
        zoomAtPoint(current, v, image, current.zoom * factor, { x: v.width / 2, y: v.height / 2 }),
      ),
    [withReadyState],
  );
  const zoomToPercent = useCallback(
    (percent: number) =>
      withReadyState((current, v, image) =>
        zoomAtPoint(current, v, image, percent / 100, { x: v.width / 2, y: v.height / 2 }),
      ),
    [withReadyState],
  );
  const reset = useCallback(() => {
    if (edited && !window.confirm("Reset all image edits?")) return;
    if (edited) {
      undoStackRef.current.push(edit);
      redoStackRef.current = [];
      setCanRedoEdit(false);
      setCanUndoEdit(true);
    }
    const next = initialImageEdit();
    setEdit(next);
    setImageEditSnapshot(path, { edit: next, undo: [...undoStackRef.current], redo: [] });
    setTool("view");
    setCropDraft(null);
    setSelectedText(null);
    withReadyState((_, v, image) => fitTransform(initialImageTransform(), v, image));
    onChange?.();
  }, [path, edit, edited, onChange, withReadyState, setEdit]);

  const undoEdit = useCallback(() => {
    const previous = undoStackRef.current.pop();
    if (!previous) return;
    setEdit((current) => {
      redoStackRef.current.push(current);
      setImageEditSnapshot(path, {
        edit: previous,
        undo: [...undoStackRef.current],
        redo: [...redoStackRef.current],
      });
      return previous;
    });
    setCanRedoEdit(true);
    setCanUndoEdit(undoStackRef.current.length > 0);
    onChange?.();
  }, [path, onChange, setEdit]);

  const redoEdit = useCallback(() => {
    const next = redoStackRef.current.pop();
    if (!next) return;
    setEdit((current) => {
      undoStackRef.current.push(current);
      setImageEditSnapshot(path, {
        edit: next,
        undo: [...undoStackRef.current],
        redo: [...redoStackRef.current],
      });
      return next;
    });
    setCanUndoEdit(true);
    setCanRedoEdit(redoStackRef.current.length > 0);
    onChange?.();
  }, [path, onChange, setEdit]);

  const rotate = (delta: number) => {
    mutateEdit((current) => ({ ...current, rotation: (current.rotation + delta + 360) % 360 }));
    withReadyState((current, v, image) => rotateTransform(current, delta, v, image));
  };

  const addText = () => {
    const id = `text-${Date.now().toString(36)}-${textSeqRef.current++}`;
    const visible = displaySize ?? imageSize;
    const width = visible ? Math.min(360, Math.max(160, Math.round(visible.width * 0.42))) : 240;
    const offset = Math.min(edit.text.length, 6) * 0.035;
    const centerX =
      imageSize && edit.crop ? (edit.crop.x + edit.crop.width / 2) / imageSize.width : 0.5;
    const centerY =
      imageSize && edit.crop ? (edit.crop.y + edit.crop.height / 2) / imageSize.height : 0.5;
    const x = Math.min(0.85, centerX + offset);
    const y = Math.min(0.85, centerY + offset);
    const layer: TextLayer = {
      id,
      text: "Text",
      fontFamily: adapters.defaultFontId ?? fonts[0]?.id ?? DEFAULT_IMAGE_FONT_ID,
      x,
      y,
      width,
      size: 56,
      color: "#ffffff",
      bold: true,
      angle: 0,
    };
    mutateEdit((current) => ({ ...current, text: [...current.text, layer] }));
    setSelectedText(id);
  };

  const setActiveTool = (next: ImageTool) => {
    if (readOnly) return;
    if (next === "crop" && imageSize) {
      setCropDraft(edit.crop ?? defaultCrop(imageSize, cropRatio));
    } else {
      setCropDraft(null);
    }
    if (next === "text") {
      if (tool === "text" || edit.text.length === 0) addText();
    }
    setTool(next);
  };

  const setCropRatioAndDraft = (ratio: number | null) => {
    setCropRatio(ratio);
    if (imageSize) setCropDraft(defaultCrop(imageSize, ratio));
  };

  const applyCrop = () => {
    if (cropDraft) mutateEdit((current) => ({ ...current, crop: cropDraft }));
    setCropDraft(null);
    setTool("view");
  };

  const cancelCrop = () => {
    setCropDraft(null);
    setTool("view");
  };

  const clearCrop = () => {
    mutateEdit((current) => ({ ...current, crop: null }));
    setCropDraft(null);
    setTool("view");
  };

  const patchSelectedText = useCallback(
    (patch: Partial<TextLayer>) => {
      if (!selectedText) return;
      mutateEdit((current) => ({
        ...current,
        text: current.text.map((layer) =>
          layer.id === selectedText ? { ...layer, ...patch } : layer,
        ),
      }));
    },
    [mutateEdit, selectedText],
  );

  const deleteSelectedText = useCallback(() => {
    if (!selectedText) return;
    mutateEdit((current) => ({
      ...current,
      text: current.text.filter((layer) => layer.id !== selectedText),
    }));
    setSelectedText(null);
  }, [mutateEdit, selectedText]);

  // Entering read-only mid-edit drops transient tool state — the recipe/undo stack is preserved.
  useEffect(() => {
    if (!readOnly) return;
    setTool("view");
    setCropDraft(null);
    setSelectedText(null);
  }, [readOnly]);

  useEffect(() => {
    if (!active) return;
    if (activeTool !== "text" || !selectedText) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || (event.key !== "Delete" && event.key !== "Backspace")) return;
      const target = event.target as HTMLElement | null;
      if (target?.closest?.("input, textarea, select, [contenteditable='true']")) return;
      event.preventDefault();
      deleteSelectedText();
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [active, activeTool, deleteSelectedText, selectedText]);

  const onWheel = (event: ReactWheelEvent<HTMLDivElement>) => {
    if (!stageSize || viewport.width <= 0 || viewport.height <= 0) return;
    // A horizontal gesture pages siblings only when the picture isn't zoomed past fit.
    const horizontalDelta =
      event.deltaX !== 0 && Math.abs(event.deltaX) > Math.abs(event.deltaY)
        ? event.deltaX
        : event.shiftKey
          ? event.deltaY
          : 0;
    if (pageFromHorizontalWheel(horizontalDelta)) {
      event.preventDefault();
      return;
    }
    event.preventDefault();
    frameRef.current?.focus();
    const rect = event.currentTarget.getBoundingClientRect();
    const point = { x: event.clientX - rect.left, y: event.clientY - rect.top };
    const factor = Math.exp(-event.deltaY * 0.0015);
    setView((current) => zoomAtPoint(current, viewport, stageSize, current.zoom * factor, point));
  };

  const onPointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    frameRef.current?.focus();
    if (!stageSize || !panning || event.button !== 0) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    dragRef.current = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      originX: view.offsetX,
      originY: view.offsetY,
    };
    setDragging(true);
  };

  const onPointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag || !stageSize || viewport.width <= 0 || viewport.height <= 0) return;
    setView((current) =>
      clampOffset(
        {
          ...current,
          offsetX: drag.originX + event.clientX - drag.startX,
          offsetY: drag.originY + event.clientY - drag.startY,
          mode: "free",
        },
        viewport,
        stageSize,
      ),
    );
  };

  const stopDrag = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (dragRef.current?.pointerId === event.pointerId) {
      dragRef.current = null;
      setDragging(false);
      if (event.currentTarget.hasPointerCapture(event.pointerId))
        event.currentTarget.releasePointerCapture(event.pointerId);
    }
  };

  const panBy = (dx: number, dy: number) =>
    withReadyState((current, v, image) =>
      clampOffset(
        { ...current, offsetX: current.offsetX + dx, offsetY: current.offsetY + dy, mode: "free" },
        v,
        image,
      ),
    );
  const patchTextLayer = (id: string, patch: Partial<TextLayer>) =>
    mutateEdit((current) => ({
      ...current,
      text: current.text.map((layer) => (layer.id === id ? { ...layer, ...patch } : layer)),
    }));
  const flipX = useCallback(
    () => mutateEdit((current) => ({ ...current, flipX: !current.flipX })),
    [mutateEdit],
  );
  const flipY = useCallback(
    () => mutateEdit((current) => ({ ...current, flipY: !current.flipY })),
    [mutateEdit],
  );
  const updateAdjustment = useCallback(
    (key: keyof ImageAdjustments, value: number) =>
      mutateEdit((current) => ({
        ...current,
        adjustments: { ...current.adjustments, [key]: value },
      })),
    [mutateEdit],
  );
  const updateResize = useCallback(
    (resize: ExportResize) => mutateEdit((current) => ({ ...current, resize })),
    [mutateEdit],
  );
  const imageLoad = (event: SyntheticEvent<HTMLImageElement>) => {
    const img = event.currentTarget;
    const next = {
      width: Math.max(1, img.naturalWidth || img.width || 1),
      height: Math.max(1, img.naturalHeight || img.height || 1),
    };
    setImageSize((current) => {
      if (current?.width === next.width && current.height === next.height) return current;
      setView(initialImageTransform());
      return next;
    });
  };

  if (!displaySrc)
    return <div className="rk-iv-empty">No image data.</div>;

  return (
    <ImageViewerShell
      displayName={displayName ?? path.split(/[\\/]/).pop() ?? "image"}
      displaySrc={shownSrc || displaySrc}
      frameRef={frameRef}
      siblingIndex={siblingIndex}
      siblingTotal={siblingTotal}
      canPrev={canPrev}
      canNext={canNext}
      dragging={dragging}
      panning={!!panning}
      ready={ready}
      edited={edited}
      canUndoEdit={canUndoEdit}
      canRedoEdit={canRedoEdit}
      view={view}
      stageSize={stageSize}
      imageSize={imageSize}
      tool={activeTool}
      readOnly={readOnly}
      edit={edit}
      cropDraft={cropDraft}
      cropRatio={cropRatio}
      selectedText={selectedText}
      selectedLayer={selectedLayer}
      adapters={adapters}
      toolbarContainer={toolbarContainer}
      onTool={setActiveTool}
      onFit={fit}
      onActual={actual}
      onZoomIn={() => zoomAtCenter(CENTER_ZOOM_STEP)}
      onZoomOut={() => zoomAtCenter(1 / CENTER_ZOOM_STEP)}
      onZoomPreset={zoomToPercent}
      onRotateLeft={() => rotate(-90)}
      onRotateRight={() => rotate(90)}
      onFlipX={flipX}
      onFlipY={flipY}
      onReset={reset}
      onUndo={undoEdit}
      onRedo={redoEdit}
      onCropRatio={setCropRatioAndDraft}
      onCropApply={applyCrop}
      onCropCancel={cancelCrop}
      onCropClear={clearCrop}
      onAddText={addText}
      onTextPatch={patchSelectedText}
      onTextDelete={deleteSelectedText}
      onAdjustment={updateAdjustment}
      onResize={updateResize}
      onPageSibling={goSibling}
      onWheel={onWheel}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerStop={stopDrag}
      onKeyPan={panBy}
      onImageLoad={imageLoad}
      onCropChange={setCropDraft}
      onTextSelect={setSelectedText}
      onTextChange={patchTextLayer}
    />
  );
}

/**
 * Editing-first alias of ImageViewer (`readOnly={false}`). Hosts that offer a
 * dedicated "edit" surface can render this; behaviour is identical.
 */
export function ImageEditor(props: ImageViewerProps) {
  return <ImageViewer {...props} readOnly={false} />;
}
