/**
 * Markdown properties panel.
 *
 * Renders the frontmatter block as a form; changes are dispatched
 * to the document core as a single batch edit. Opening the panel
 * performs no source mutation — reads only.
 */

import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  readFrontmatterDocument,
  stringifyFrontmatterDocument,
  writeFrontmatterDocument,
} from "./frontmatterDocument";
import type { MarkdownDocumentCore } from "../core/MarkdownDocumentCore";
import type { MarkdownCommand } from "../core/MarkdownDocumentTypes";

export interface MarkdownPropertiesPanelProps {
  readonly core: MarkdownDocumentCore | undefined;
}

/**
 * One form row for a single frontmatter key.
 */
function FieldRow({
  label,
  value,
  onChange,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
}): ReactNode {
  return (
    <div data-testid={`prop-${label}`} className="field-row">
      <label htmlFor={`md-prop-${label}`}>{label}</label>
      <input
        id={`md-prop-${label}`}
        type="text"
        value={value}
        onChange={(e) => onChange(e.target.value)}
      />
    </div>
  );
}

/**
 * Render the properties panel. Returns a placeholder when no
 * frontmatter is present.
 */
export function MarkdownPropertiesPanel({ core }: MarkdownPropertiesPanelProps): ReactNode {
  const snapshot = core?.snapshot();
  const source = snapshot?.logicalSource ?? "";
  const fm = useMemo(() => readFrontmatterDocument(source), [source]);
  const [draft, setDraft] = useState<Record<string, string>>({});
  const dirtyKeysRef = useRef(new Set<string>());

  useEffect(() => {
    void core;
    dirtyKeysRef.current.clear();
  }, [core]);

  useEffect(() => {
    const next: Record<string, string> = {};
    const doc = fm.parsed;
    if (doc) {
      const values = doc.toJS();
      if (values && typeof values === "object" && !Array.isArray(values)) {
        for (const [key, value] of Object.entries(values)) {
          next[key] = typeof value === "object" ? JSON.stringify(value) : String(value);
        }
      }
    }
    setDraft((previous) => {
      const merged = { ...next };
      for (const key of dirtyKeysRef.current) {
        if (key in previous) merged[key] = previous[key];
      }
      return merged;
    });
  }, [fm.parsed]);

  if (!core) return <div data-testid="properties-empty">No document</div>;
  if (fm.isMissing) {
    return (
      <div data-testid="properties-missing">
        No frontmatter. Add a `---` block to enable properties.
      </div>
    );
  }

  const onSave = (): void => {
    const doc = fm.parsed;
    if (!doc) return;
    for (const [key, value] of Object.entries(draft)) {
      doc.set(key, value);
    }
    const newYaml = stringifyFrontmatterDocument(doc);
    const newSource = writeFrontmatterDocument(source, newYaml);
    if (newSource === source) {
      dirtyKeysRef.current.clear();
      return;
    }
    const edits = computeReplaceAllEdits(source, newSource);
    const command: MarkdownCommand = {
      edits,
      expectedRevision: snapshot?.revision,
      origin: "block-editor",
      label: "Format",
      isolateHistory: true,
      timestampMs: Math.round(performance.now()),
    };
    core.applyCommand(command);
    dirtyKeysRef.current.clear();
  };

  const onFieldChange = (key: string, value: string): void => {
    dirtyKeysRef.current.add(key);
    setDraft((prev) => ({ ...prev, [key]: value }));
  };

  const keys = Object.keys(draft);

  return (
    <div data-testid="properties-panel">
      {keys.map((key) => (
        <FieldRow
          key={key}
          label={key}
          value={draft[key] ?? ""}
          onChange={(v) => onFieldChange(key, v)}
        />
      ))}
      <button type="button" data-testid="properties-save" onClick={onSave}>
        Save
      </button>
    </div>
  );
}

function computeReplaceAllEdits(
  oldSource: string,
  newSource: string,
): { from: number; to: number; insert: string; inheritMode: "preserve" }[] {
  return [
    {
      from: 0,
      to: oldSource.length,
      insert: newSource,
      inheritMode: "preserve",
    },
  ];
}
