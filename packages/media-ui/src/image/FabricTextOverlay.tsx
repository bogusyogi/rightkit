import { useEffect, useRef } from "react";
import { Canvas, Textbox, type FabricObject } from "fabric";
import type { TextLayer } from "./imageEdit.js";
import type { ImageSize } from "./imageViewerState.js";
import { cssFontFamilyForImageText } from "./imageFonts.js";
import { acquireResource, type MediaAdapters } from "../adapters.js";

interface Props {
  text: TextLayer[];
  selectedId: string | null;
  imageSize: ImageSize;
  interactive: boolean;
  adapters: MediaAdapters;
  onSelect: (id: string | null) => void;
  onChange: (id: string, patch: Partial<TextLayer>) => void;
}

type TextboxWithId = Textbox & { mediaUiId?: string };

export function FabricTextOverlay({
  text,
  selectedId,
  imageSize,
  interactive,
  adapters,
  onSelect,
  onChange,
}: Props) {
  const hostEl = useRef<HTMLDivElement | null>(null);
  const canvasEl = useRef<HTMLCanvasElement | null>(null);
  const fabricCanvas = useRef<Canvas | null>(null);
  const syncing = useRef(false);
  const onSelectRef = useRef(onSelect);
  const onChangeRef = useRef(onChange);
  const fonts = adapters.bundledFonts ?? [];
  const accent = adapters.accentColor ?? "#d9803f";

  useEffect(() => {
    onSelectRef.current = onSelect;
  }, [onSelect]);

  useEffect(() => {
    onChangeRef.current = onChange;
  }, [onChange]);

  useEffect(() => {
    if (!canvasEl.current) return;
    const canvas = new Canvas(canvasEl.current, {
      width: imageSize.width,
      height: imageSize.height,
      backgroundColor: "transparent",
      preserveObjectStacking: true,
      selection: interactive,
    });
    fabricCanvas.current = canvas;
    const releaseCensus = acquireResource(adapters, {
      kind: "fabric-canvas",
      estimatedBytes: imageSize.width * imageSize.height * 4,
    }).release;

    const selectTarget = (target: FabricObject | undefined) => {
      const id = (target as TextboxWithId | undefined)?.mediaUiId ?? null;
      onSelectRef.current(id);
    };
    const syncTarget = (target: FabricObject | undefined) => {
      if (syncing.current) return;
      const object = target as TextboxWithId | undefined;
      if (!object?.mediaUiId) return;
      normalizeScale(object);
      onChangeRef.current(object.mediaUiId, objectToPatch(object, imageSize));
    };

    canvas.on("selection:created", (event) => selectTarget(event.selected?.[0]));
    canvas.on("selection:updated", (event) => selectTarget(event.selected?.[0]));
    canvas.on("selection:cleared", () => onSelectRef.current(null));
    canvas.on("object:moving", (event) => syncTarget(event.target));
    canvas.on("object:scaling", (event) => syncTarget(event.target));
    canvas.on("object:rotating", (event) => syncTarget(event.target));
    canvas.on("object:modified", (event) => syncTarget(event.target));
    canvas.on("text:changed", (event) => syncTarget(event.target));

    return () => {
      fabricCanvas.current = null;
      releaseCensus();
      void canvas.dispose();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [imageSize.height, imageSize.width, interactive, imageSize]);

  useEffect(() => {
    const canvas = fabricCanvas.current;
    if (!canvas) return;
    canvas.setDimensions({ width: imageSize.width, height: imageSize.height });
  }, [imageSize.height, imageSize.width]);

  useEffect(() => {
    const canvas = fabricCanvas.current;
    if (!canvas) return;
    syncing.current = true;
    const objects = new Map<string, TextboxWithId>();
    for (const object of canvas.getObjects()) {
      const textbox = object as TextboxWithId;
      if (textbox.mediaUiId) objects.set(textbox.mediaUiId, textbox);
    }

    for (const layer of text) {
      const object = objects.get(layer.id);
      if (object) {
        object.set(layerToObject(layer, imageSize, interactive, fonts, accent));
        // Words edited outside the canvas (inspector, undo) must reach the object; leave an in-progress edit alone.
        const words = layer.text || "Text";
        if (!object.isEditing && object.text !== words) object.set({ text: words });
        setTextboxEditingContainer(object, canvas.wrapperEl ?? hostEl.current);
        objects.delete(layer.id);
      } else {
        const textbox = new Textbox(
          layer.text || "Text",
          layerToObject(layer, imageSize, interactive, fonts, accent),
        ) as TextboxWithId;
        textbox.mediaUiId = layer.id;
        setTextboxEditingContainer(textbox, canvas.wrapperEl ?? hostEl.current);
        canvas.add(textbox);
      }
    }
    for (const stale of objects.values()) canvas.remove(stale);
    canvas.getObjects().forEach((object) => {
      object.selectable = interactive;
      object.evented = interactive;
    });
    if (!interactive) canvas.discardActiveObject();
    syncing.current = false;
    canvas.requestRenderAll();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [imageSize, interactive, text]);

  useEffect(() => {
    const canvas = fabricCanvas.current;
    if (!canvas || !interactive) return;
    if (!selectedId) {
      canvas.discardActiveObject();
      canvas.requestRenderAll();
      return;
    }
    const object = canvas
      .getObjects()
      .find((candidate) => (candidate as TextboxWithId).mediaUiId === selectedId);
    if (object && canvas.getActiveObject() !== object) {
      canvas.setActiveObject(object);
      canvas.requestRenderAll();
    }
  }, [interactive, selectedId]);

  return (
    <div
      ref={hostEl}
      className="rk-iv-overlay"
      style={{ pointerEvents: interactive ? "auto" : "none" }}
      onDoubleClick={(event) => event.stopPropagation()}
      onMouseDown={(event) => {
        if (interactive) event.stopPropagation();
      }}
    >
      <canvas ref={canvasEl} width={imageSize.width} height={imageSize.height} />
    </div>
  );
}

function setTextboxEditingContainer(
  object: TextboxWithId,
  container: HTMLElement | null | undefined,
) {
  if (container)
    (object as TextboxWithId & { hiddenTextareaContainer?: HTMLElement }).hiddenTextareaContainer =
      container;
}

function layerToObject(
  layer: TextLayer,
  image: ImageSize,
  interactive: boolean,
  fonts: MediaFontOptionLike[],
  accent: string,
) {
  return {
    left: layer.x * image.width,
    top: layer.y * image.height,
    originX: "center" as const,
    originY: "center" as const,
    width: Math.max(32, layer.width),
    fontSize: Math.max(8, layer.size),
    fontFamily: cssFontFamilyForImageText(fonts, layer.fontFamily),
    fontWeight: layer.bold ? "700" : "400",
    fill: layer.color,
    angle: layer.angle,
    textAlign: "center" as const,
    editable: interactive,
    selectable: interactive,
    evented: interactive,
    transparentCorners: false,
    cornerColor: accent,
    cornerStrokeColor: "#0f0f0e",
    borderColor: accent,
    cornerSize: 10,
    padding: 4,
  };
}

type MediaFontOptionLike = import("../adapters.js").MediaFontOption;

function normalizeScale(object: TextboxWithId) {
  const nextWidth = Math.max(32, (object.width || 32) * (object.scaleX || 1));
  const nextSize = Math.max(8, (object.fontSize || 8) * (object.scaleY || 1));
  object.set({ width: nextWidth, fontSize: nextSize, scaleX: 1, scaleY: 1 });
}

function objectToPatch(object: TextboxWithId, image: ImageSize): Partial<TextLayer> {
  return {
    text: object.text || "",
    x: Math.min(1, Math.max(0, (object.left || 0) / image.width)),
    y: Math.min(1, Math.max(0, (object.top || 0) / image.height)),
    width: Math.max(32, object.width || 32),
    size: Math.max(8, object.fontSize || 8),
    color: String(object.fill || "#ffffff"),
    bold: String(object.fontWeight || "400") !== "400",
    angle: object.angle || 0,
  };
}
