import { SegmentedControl } from "@rightkit/react-shell/react";
import { SettingsGroup } from "@rightkit/settings-ui";
import { useSettings } from "../settings";
import { onOff } from "./onOff";

export function EditorSection() {
  const { settings, update, reset } = useSettings();

  return (
    <>
      <SettingsGroup title="Source size">
        <SegmentedControl
          label="Source size"
          options={[
            { value: "11", label: "small", title: "Small source editor size (11px)" },
            { value: "12", label: "normal", title: "Normal source editor size (12px)" },
            { value: "14", label: "large", title: "Large source editor size (14px)" },
            { value: "16", label: "x-large", title: "Extra large source editor size (16px)" },
          ]}
          value={String(settings.sourceFontSizePx)}
          onChange={(v) => update({ sourceFontSizePx: Number(v) })}
        />
      </SettingsGroup>

      <SettingsGroup
        title="Motion"
        description="System follows your OS reduced-motion setting."
      >
        <SegmentedControl
          label="Motion"
          options={[
            { value: "system", label: "system", title: "Motion follows system" },
            { value: "reduce", label: "reduced", title: "Reduce motion" },
            { value: "full", label: "full", title: "Full motion" },
          ]}
          value={settings.motion}
          onChange={(v) => update({ motion: v })}
        />
      </SettingsGroup>

      <SettingsGroup title="Spell check">
        <SegmentedControl
          label="Spell check"
          options={onOff("Spell check")}
          value={settings.spellcheck ? "on" : "off"}
          onChange={(v) => update({ spellcheck: v === "on" })}
        />
      </SettingsGroup>

      <SettingsGroup title="Word wrap">
        <SegmentedControl
          label="Word wrap"
          options={onOff("Word wrap")}
          value={settings.wordWrap ? "on" : "off"}
          onChange={(v) => update({ wordWrap: v === "on" })}
        />
      </SettingsGroup>

      <SettingsGroup
        title="Format on save"
        description="Reindent Markdown & HTML source when you save."
      >
        <SegmentedControl
          label="Format on save"
          options={onOff("Format on save")}
          value={settings.formatOnSave ? "on" : "off"}
          onChange={(v) => update({ formatOnSave: v === "on" })}
        />
      </SettingsGroup>

      <SettingsGroup
        title="Safe Markdown HTML"
        description="Render only sanitized local HTML. Source remains unchanged."
      >
        <SegmentedControl
          label="Safe Markdown HTML"
          options={onOff("Safe Markdown HTML")}
          value={settings.markdownSafeHtml ? "on" : "off"}
          onChange={(v) => update({ markdownSafeHtml: v === "on" })}
        />
      </SettingsGroup>

      <SettingsGroup
        title="Auto save"
        description="Save file-backed edits after you pause. Untitled drafts wait for their first Save."
      >
        <SegmentedControl
          label="Auto save"
          options={onOff("Auto save")}
          value={settings.autoSave ? "on" : "off"}
          onChange={(v) => update({ autoSave: v === "on" })}
        />
      </SettingsGroup>

      <button
        type="button"
        onClick={reset}
        className="vr-press vr-focus rounded px-2 py-1.5 font-sans text-xs text-muted-2 hover:bg-panel-2 hover:text-paper"
      >
        Reset to defaults
      </button>
    </>
  );
}
