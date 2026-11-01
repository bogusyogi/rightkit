import type { RecipeParameter } from "@rightkit/agent-ui-core/lib/types";
import { isRecipeBooleanChecked } from "./runtimeUtils";

export default function RecipeParameterField({
  parameter,
  value,
  onChange,
}: {
  parameter: RecipeParameter;
  value: string;
  onChange: (value: string) => void;
}) {
  const id = `recipe-param-${parameter.key}`;
  const commonInputClass =
    "h-8 min-w-0 rounded border border-line bg-bg px-3 text-[12px] text-text outline-none";

  return (
    <div className="grid grid-cols-[minmax(120px,0.38fr)_minmax(0,1fr)] items-center gap-3 border-b border-line px-3 py-2 last:border-b-0">
      <label className="min-w-0" htmlFor={id}>
        <span className="block truncate text-[12px] font-semibold text-text">{parameter.key}</span>
        <span className="mt-0.5 block truncate text-[10px] uppercase text-text-3">
          {parameter.inputType} · {parameter.requirement}
        </span>
      </label>
      <div className="min-w-0">
        {parameter.inputType === "boolean" ? (
          <label className="inline-flex h-8 items-center gap-2 text-[12px] text-text-2">
            {/* target-size-gate-allow: native checkbox sits inside this activating label, whose row (with its trailing text) is the real pointer target and already clears 24x24 */}
            <input
              id={id}
              className="h-4 w-4 accent-accent"
              type="checkbox"
              checked={isRecipeBooleanChecked(value)}
              onChange={(event) => onChange(event.target.checked ? "true" : "false")}
            />
            {isRecipeBooleanChecked(value) ? "true" : "false"}
          </label>
        ) : (
          <input
            aria-label="Recipe parameter value"
            id={id}
            className={commonInputClass}
            type={
              parameter.inputType === "number"
                ? "number"
                : parameter.inputType === "secret"
                  ? "password"
                  : "text"
            }
            autoComplete={parameter.inputType === "secret" ? "off" : undefined}
            value={value}
            onChange={(event) => onChange(event.target.value)}
          />
        )}
        {parameter.description ? (
          <p className="mt-1 truncate text-[11px] text-text-3">{parameter.description}</p>
        ) : null}
      </div>
    </div>
  );
}
