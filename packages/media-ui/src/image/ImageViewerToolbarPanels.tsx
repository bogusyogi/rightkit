import { useEffect, useMemo, useState } from "react";
import {
  cssFontFamilyForImageText,
  DEFAULT_IMAGE_FONT_ID,
  isBundledImageFontId,
} from "./imageFonts.js";
import type { ExportResize, ImageAdjustments, TextLayer } from "./imageEdit.js";
import type { MediaAdapters } from "../adapters.js";
import { TBtn } from "./toolbar.js";

type FontLoadState = "idle" | "loading" | "ready" | "failed";

let cachedSystemFonts: string[] | null = null;

export function CropPanel({
  ratio,
  onRatio,
  onApply,
  onCancel,
  onClear,
}: {
  ratio: number | null;
  onRatio: (ratio: number | null) => void;
  onApply: () => void;
  onCancel: () => void;
  onClear: () => void;
}) {
  return (
    <div className="rk-iv-panel">
      <span className="rk-iv-dim">aspect</span>
      <select
        value={ratio ?? "free"}
        onChange={(e) => onRatio(e.target.value === "free" ? null : Number(e.target.value))}
        className="rk-iv-input"
      >
        <option value="free">free</option>
        <option value={1}>1:1</option>
        <option value={4 / 3}>4:3</option>
        <option value={3 / 2}>3:2</option>
        <option value={16 / 9}>16:9</option>
        <option value={9 / 16}>9:16</option>
      </select>
      <TBtn onRun={onApply} title="Apply crop" label="apply" width={48} />
      <TBtn onRun={onCancel} title="Cancel crop" label="cancel" width={54} />
      <TBtn onRun={onClear} title="Clear crop" label="clear" width={48} />
    </div>
  );
}

export function AdjustPanel({
  adjustments,
  onChange,
}: {
  adjustments: ImageAdjustments;
  onChange: (key: keyof ImageAdjustments, value: number) => void;
}) {
  return (
    <div className="rk-iv-panel rk-iv-panel-wrap">
      <Slider
        label="brightness"
        value={adjustments.brightness}
        min={0}
        max={200}
        onChange={(v) => onChange("brightness", v)}
      />
      <Slider
        label="contrast"
        value={adjustments.contrast}
        min={0}
        max={200}
        onChange={(v) => onChange("contrast", v)}
      />
      <Slider
        label="saturation"
        value={adjustments.saturation}
        min={0}
        max={200}
        onChange={(v) => onChange("saturation", v)}
      />
      <Slider
        label="hue"
        value={adjustments.hue}
        min={-180}
        max={180}
        onChange={(v) => onChange("hue", v)}
      />
      <Slider
        label="gray"
        value={adjustments.grayscale}
        min={0}
        max={100}
        onChange={(v) => onChange("grayscale", v)}
      />
      <Slider
        label="sepia"
        value={adjustments.sepia}
        min={0}
        max={100}
        onChange={(v) => onChange("sepia", v)}
      />
      <Slider
        label="blur"
        value={adjustments.blur}
        min={0}
        max={8}
        step={0.25}
        onChange={(v) => onChange("blur", v)}
      />
    </div>
  );
}

export function ResizePanel({
  resize,
  onChange,
}: {
  resize: ExportResize;
  onChange: (resize: ExportResize) => void;
}) {
  const value = resize.mode === "original" ? "" : String(resize.value);
  return (
    <div className="rk-iv-panel">
      <select
        value={resize.mode}
        onChange={(e) => {
          const mode = e.target.value as ExportResize["mode"];
          onChange(
            mode === "original" ? { mode } : { mode, value: mode === "percent" ? 100 : 1200 },
          );
        }}
        className="rk-iv-input"
      >
        <option value="original">original</option>
        <option value="percent">percent</option>
        <option value="width">width</option>
        <option value="height">height</option>
      </select>
      {resize.mode !== "original" && (
        <input
          type="number"
          min={resize.mode === "percent" ? 1 : 16}
          max={resize.mode === "percent" ? 800 : 32000}
          value={value}
          onChange={(e) => onChange({ mode: resize.mode, value: Number(e.target.value) || 1 })}
          className="rk-iv-input rk-iv-num"
        />
      )}
      <span className="rk-iv-dim">applied on Save / Save As</span>
    </div>
  );
}

export function TextPanel({
  selected,
  adapters,
  onAdd,
  onPatch,
  onDelete,
}: {
  selected: TextLayer | null;
  adapters: MediaAdapters;
  onAdd: () => void;
  onPatch: (patch: Partial<TextLayer>) => void;
  onDelete: () => void;
}) {
  const bundledFonts = adapters.bundledFonts ?? [];
  const [systemFonts, setSystemFonts] = useState<string[]>(cachedSystemFonts ?? []);
  const [fontState, setFontState] = useState<FontLoadState>(cachedSystemFonts ? "ready" : "idle");
  const selectedFont = selected?.fontFamily ?? DEFAULT_IMAGE_FONT_ID;
  const bundledFontValue = isBundledImageFontId(bundledFonts, selectedFont) ? selectedFont : "";
  const systemFontValue =
    selected && !isBundledImageFontId(bundledFonts, selectedFont) ? selectedFont : "";
  const systemFontOptions = useMemo(() => {
    if (
      systemFontValue &&
      !systemFonts.some((font) => font.toLocaleLowerCase() === systemFontValue.toLocaleLowerCase())
    ) {
      return [systemFontValue, ...systemFonts];
    }
    return systemFonts;
  }, [systemFontValue, systemFonts]);
  const listSystemFonts = adapters.listSystemFonts;

  useEffect(() => {
    let alive = true;
    if (cachedSystemFonts || !listSystemFonts) return;
    setFontState("loading");
    void listSystemFonts()
      .then((fonts) => {
        cachedSystemFonts = fonts;
        if (!alive) return;
        setSystemFonts(fonts);
        setFontState("ready");
      })
      .catch(() => {
        if (alive) setFontState("failed");
      });
    return () => {
      alive = false;
    };
  }, [listSystemFonts]);

  return (
    <div className="rk-iv-panel rk-iv-panel-wrap">
      <TBtn onRun={onAdd} title="Add text" label="add" width={38} />
      <input
        value={selected?.text ?? ""}
        disabled={!selected}
        onChange={(e) => onPatch({ text: e.target.value })}
        className="rk-iv-input rk-iv-text"
        placeholder="select text"
      />
      {bundledFonts.length > 0 && (
        <label className="rk-iv-field">
          <span className="rk-iv-dim">font</span>
          <select
            value={bundledFontValue}
            disabled={!selected}
            onChange={(e) => e.target.value && onPatch({ fontFamily: e.target.value })}
            className="rk-iv-input rk-iv-font-bundled"
            title={adapters.bundledFontsLabel ?? "Bundled fonts"}
          >
            <option value="">{adapters.bundledFontsEmptyOption ?? "Bundled"}</option>
            {bundledFonts.map((font) => (
              <option key={font.id} value={font.id} style={{ fontFamily: font.stack }}>
                {font.name}
              </option>
            ))}
          </select>
        </label>
      )}
      <label className="rk-iv-field">
        <span className="rk-iv-dim">system</span>
        <select
          value={systemFontValue}
          disabled={!selected || fontState === "loading" || fontState === "failed"}
          onChange={(e) => e.target.value && onPatch({ fontFamily: e.target.value })}
          className="rk-iv-input rk-iv-font"
          title="Installed system fonts"
        >
          <option value="">{systemFontLabel(fontState, systemFontOptions.length)}</option>
          {systemFontOptions.map((font) => (
            <option
              key={font}
              value={font}
              style={{ fontFamily: cssFontFamilyForImageText(bundledFonts, font, {
                defaultFontId: adapters.defaultFontId,
                fallbackStack: adapters.fallbackFontStack,
                systemFallbackStack: adapters.systemFontFallbackStack,
              }) }}
            >
              {font}
            </option>
          ))}
        </select>
      </label>
      <label className="rk-iv-field rk-iv-color" title="Text color">
        <span className="rk-iv-dim" aria-hidden="true">color</span>
        <span
          className="rk-iv-swatch"
          style={{ backgroundColor: selected?.color ?? "#ffffff" }}
          aria-hidden="true"
        >
          <span className="rk-iv-swatch-slash" />
          <input
            type="color"
            value={selected?.color ?? "#ffffff"}
            disabled={!selected}
            onChange={(e) => onPatch({ color: e.target.value })}
            aria-label="Text color"
          />
        </span>
      </label>
      <input
        type="number"
        min={8}
        max={240}
        value={selected?.size ?? 56}
        disabled={!selected}
        onChange={(e) => onPatch({ size: Number(e.target.value) || 56 })}
        className="rk-iv-input rk-iv-num-sm"
        title="Text size"
      />
      <TBtn
        active={!!selected?.bold}
        disabled={!selected}
        onRun={() => onPatch({ bold: !selected?.bold })}
        title="Bold"
        label="B"
        bold
      />
      <TBtn disabled={!selected} onRun={onDelete} title="Delete text" label="delete" width={54} />
    </div>
  );
}

function systemFontLabel(state: FontLoadState, count: number): string {
  if (state === "loading") return "loading...";
  if (state === "failed") return "unavailable";
  return count > 0 ? "System fonts" : "System";
}

function Slider({
  label,
  value,
  min,
  max,
  step = 1,
  onChange,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  step?: number;
  onChange: (v: number) => void;
}) {
  return (
    <label className="rk-iv-slider">
      <span className="rk-iv-dim rk-iv-slider-label">{label}</span>
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        className="rk-iv-range"
      />
      <span className="rk-iv-dim rk-iv-slider-value">{Math.round(value)}</span>
    </label>
  );
}
