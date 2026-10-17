import { useState, type ReactNode } from "react";

export interface CodeBlockProps {
  code: string;
  language?: string;
  /** Called after a successful copy. */
  onCopy?: (code: string) => void;
  /** Icon slot rendered inside the copy button for the current state. */
  renderIcon?: (state: "idle" | "copied" | "failed") => ReactNode;
  className?: string;
}

const LABELS = { idle: "Copy", copied: "Copied", failed: "Copy failed" } as const;

/** Code block with a language label and a clipboard copy button. The code renders as text (never
 * as markup); it wraps by default and scrolls past the `--rk-doc-code-max-height` CSS variable. */
export function CodeBlock({ code, language, onCopy, renderIcon, className }: CodeBlockProps) {
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(code);
      setState("copied");
      onCopy?.(code);
    } catch {
      setState("failed");
    }
    setTimeout(() => setState("idle"), 1500);
  };
  return (
    <div className={`rk-doc-codeblock${className ? ` ${className}` : ""}`}>
      <div className="rk-doc-codeblock-bar">
        <span className="rk-doc-codeblock-lang">{language ?? ""}</span>
        <button type="button" className="rk-doc-codeblock-copy" data-state={state} aria-label={LABELS[state]} onClick={copy}>
          {renderIcon?.(state)}
          <span>{LABELS[state]}</span>
        </button>
      </div>
      <pre className="rk-doc-code"><code>{code}</code></pre>
    </div>
  );
}
