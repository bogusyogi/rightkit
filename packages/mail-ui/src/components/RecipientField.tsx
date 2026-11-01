// Tokenized recipient field: parses comma/semicolon-separated addresses into
// removable chips, with a live input for typing new ones. The store shape
// (comma-joined string) is unchanged — this is a visual layer on top.
import { IconButton, Input } from "@rightkit/react-shell/react";
import { useRef, type KeyboardEvent } from "react";

function parseTokens(raw: string): string[] {
  return raw
    .split(/[,;]/)
    .map((s) => s.trim())
    .filter(Boolean);
}

function splitRecipients(raw: string): { chips: string[]; draft: string } {
  if (!raw.trim()) return { chips: [], draft: "" };
  if (/[,;]\s*$/.test(raw)) return { chips: parseTokens(raw), draft: "" };
  const parts = raw.split(/[,;]/);
  return {
    chips: parts.slice(0, -1).map((s) => s.trim()).filter(Boolean),
    draft: (parts[parts.length - 1] ?? "").trimStart(),
  };
}

function joinCommitted(chips: string[]): string {
  return chips.length ? `${chips.join(", ")}, ` : "";
}

export function RecipientField({
  value,
  onChange,
  onKeyDown,
  onBlur,
  placeholder,
  children,
}: {
  value: string;
  onChange: (joined: string) => void;
  onKeyDown?: (e: KeyboardEvent<HTMLInputElement>) => void;
  onBlur?: () => void;
  placeholder?: string;
  children?: React.ReactNode;
}) {
  const { chips, draft } = splitRecipients(value);
  const inputRef = useRef<HTMLInputElement>(null);

  function commit(raw: string) {
    const next = raw.trim();
    if (!next) return;
    onChange(joinCommitted([...chips, next]));
  }

  function handleKey(e: KeyboardEvent<HTMLInputElement>) {
    if ((e.key === "," || e.key === ";") && draft.trim()) {
      e.preventDefault();
      commit(draft);
      return;
    }
    if (e.key === "Backspace" && !draft && chips.length > 0) {
      e.preventDefault();
      onChange(joinCommitted(chips.slice(0, -1)));
    }
    onKeyDown?.(e);
    if (e.key === "Enter" && draft.trim() && !e.defaultPrevented) {
      e.preventDefault();
      commit(draft);
    }
  }

  return (
    <div className="recip-chips" onClick={() => inputRef.current?.focus()}>
      {chips.map((c, i) => (
        <span className="recip-chip" key={`${c}-${i}`}>
          {c}
          <IconButton label={`Remove ${c}`}
            className="recip-chip-x"
            aria-label={`Remove ${c}`}
            onMouseDown={(e) => {
              e.preventDefault();
              onChange(
                chips
                  .filter((_, j) => j !== i)
                  .join(", ") + (chips.length > 1 ? ", " : "")
              );
            }}
          >
            &times;
          </IconButton>
        </span>
      ))}
      <Input
        ref={inputRef}
        className="recip-input"
        value={draft}
        placeholder={chips.length === 0 ? placeholder : undefined}
        onChange={(e) => {
          const v = e.target.value;
          if (v.endsWith(",") || v.endsWith(";")) {
            commit(v.slice(0, -1));
            return;
          }
          onChange([...chips, v].join(", "));
        }}
        onKeyDown={handleKey}
        onBlur={() => {
          commit(draft);
          onBlur?.();
        }}
      />
      {children}
    </div>
  );
}
