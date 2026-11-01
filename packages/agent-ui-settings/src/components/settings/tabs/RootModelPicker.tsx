import type { CatalogGroup, InferenceSelection } from "@rightkit/agent-ui-core/lib/types/inference";
import { inferenceSelectionKey, selectionForCatalogModel } from "@rightkit/agent-ui-core/lib/types/inference";

export interface RootModelPickerProps {
  catalogGroups: CatalogGroup[];
  value?: InferenceSelection | null;
  onSelect: (selection: InferenceSelection) => void;
  error?: string | null;
  compact?: boolean;
}

function modelSubtitle(group: CatalogGroup, model: CatalogGroup["models"][number]): string {
  if (group.authority === "direct") {
    try {
      return `${group.protocol.replace("_", " ")} · ${new URL(group.name).host}`;
    } catch {
      return group.protocol.replace("_", " ");
    }
  }
  const identity = [model.ownedBy, model.root, model.parent].filter(Boolean).join(" · ");
  const limits = [
    model.contextLength && `${model.contextLength.toLocaleString()} ctx`,
    model.maxInputTokens && `${model.maxInputTokens.toLocaleString()} in`,
    model.maxOutputTokens && `${model.maxOutputTokens.toLocaleString()} out`,
  ]
    .filter(Boolean)
    .join(" · ");
  return (
    [identity, model.capabilities.join(", "), limits]
      .filter(Boolean)
    .join(" · ") || "Direct catalog"
  );
}

/** Authority-first selector shared by Settings & composer. */
export default function RootModelPicker({
  catalogGroups,
  value,
  onSelect,
  error,
  compact = false,
}: RootModelPickerProps) {
  const current = inferenceSelectionKey(value);
  const valid = catalogGroups.some((group) =>
    group.models.some(
      (model) => inferenceSelectionKey(selectionForCatalogModel(group, model.id)) === current,
    ),
  );
  return (
    <div className="flex flex-col gap-3" data-testid="inference-picker">
      {error ? (
        <p className="rounded-md border border-danger/40 bg-danger/10 p-2 text-[12px] text-danger">
          {error}
        </p>
      ) : null}
      {value && !valid ? (
        <p
          className="rounded-md border border-warning/40 bg-warning/10 p-2 text-[12px] text-warning"
          aria-live="polite"
        >
          Saved selection is unavailable. Probe or repair its connection before choosing a
          replacement.
        </p>
      ) : null}
      {catalogGroups.map((group) => {
        const models = group.models.filter((model) => !model.id.startsWith("auto/"));
        if (models.length === 0) return null;
        return (
          <section
            key={`${group.authority}:${group.connectionId}`}
            className="rounded-lg border border-line bg-panel"
            aria-label={`Direct Connections: ${group.name}`}
          >
            <header className="border-b border-line px-3 py-2">
              <strong className="block text-[12px] text-text">
                Direct Connections
              </strong>
              <span className="text-[11px] text-text-3">
                {group.name} · {group.state.replaceAll("_", " ")}
              </span>
            </header>
            <ul className={compact ? "grid grid-cols-2 gap-1 p-1" : "flex flex-col gap-1 p-1"}>
              {models.map((model) => {
                const selection = selectionForCatalogModel(group, model.id);
                const active = current === inferenceSelectionKey(selection);
                const disabled = !model.selectable || group.state !== "ready";
                return (
                  <li key={model.id}>
                    <button
                      type="button"
                      aria-pressed={active}
                      disabled={disabled}
                      onClick={() => onSelect(selection)}
                      className={`flex w-full items-center gap-2 rounded-md border px-2.5 py-2 text-left text-[12px] disabled:opacity-50 ${active ? "border-accent bg-panel-2" : "border-transparent hover:border-line hover:bg-panel-2"}`}
                    >
                      <span className="min-w-0 flex-1">
                        <strong className="block truncate text-text">{model.id}</strong>
                        <span className="block truncate text-[11px] text-text-3">
                          {model.unavailableReason ?? modelSubtitle(group, model)}
                        </span>
                      </span>
                      {active ? (
                        <span aria-hidden="true" className="text-accent">
                          ✓
                        </span>
                      ) : null}
                    </button>
                  </li>
                );
              })}
            </ul>
          </section>
        );
      })}
      {catalogGroups.length === 0 ? (
        <p className="text-[12px] text-text-3">
          No Direct connections are ready. Add a connection to continue.
        </p>
      ) : null}
    </div>
  );
}
