import { Field } from "@rightkit/react-shell/react";
import type { OperatorScope } from "../data";

/** The channel the suggestions and spend screens read. A native select, labelled, in the focus order. */
export function ScopeSelect({
  scopes,
  value,
  onChange,
}: {
  scopes: readonly OperatorScope[];
  value: string;
  onChange: (scopeId: string) => void;
}) {
  return (
    <Field label="Channel">
      {({ id, "aria-describedby": describedBy }) => (
        <select
          id={id}
          aria-describedby={describedBy}
          className="op-select"
          value={value}
          onChange={(e) => onChange(e.target.value)}
        >
          {scopes.map((scope) => (
            <option key={scope.id} value={scope.id}>
              {scope.label}
            </option>
          ))}
        </select>
      )}
    </Field>
  );
}
