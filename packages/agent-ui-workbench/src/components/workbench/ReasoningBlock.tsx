import { useId, useState } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";

export default function ReasoningBlock({
  content,
  expanded,
  onToggle,
}: {
  content: string;
  expanded?: boolean;
  onToggle?: () => void;
}) {
  const [localExpanded, setLocalExpanded] = useState(false);
  const isExpanded = expanded ?? localExpanded;
  const contentId = useId();
  const trimmed = content.trim();
  if (!trimmed) return null;

  function toggle() {
    if (onToggle) onToggle();
    else setLocalExpanded((value) => !value);
  }

  return (
    <article className="flex flex-col gap-1 text-text-3">
      <button
        type="button"
        className="flex items-center gap-2 px-0 py-1 text-left cursor-pointer border-0 bg-transparent text-text-3 hover:text-text-2 w-full"
        onClick={toggle}
        aria-expanded={isExpanded}
        aria-controls={contentId}
      >
        <span className="w-[6px] h-[6px] rounded-full bg-accent-2" aria-hidden="true" />
        <span className="flex-1 text-[11px] font-semibold uppercase tracking-wide">Thinking</span>
        <span className={`transition-transform duration-150 motion-reduce:transition-none ${isExpanded ? "rotate-180" : ""}`}>
          <Icon name="chevron" size={13} />
        </span>
      </button>
      {isExpanded ? (
        <section
          id={contentId}
          aria-label="Thinking details"
          className="max-h-72 overflow-y-auto rounded-lg border border-line bg-bg p-2"
        >
          <pre className="m-0 text-[12px] text-text-3 font-mono whitespace-pre-wrap break-words">
            {content}
          </pre>
        </section>
      ) : null}
    </article>
  );
}
