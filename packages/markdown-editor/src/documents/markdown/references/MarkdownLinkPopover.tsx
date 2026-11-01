import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { useDismiss } from "@rightkit/platform-ui/headless";
import { createShortcutRegistry } from "@rightkit/react-shell";
import type { MarkdownLinkDraft, MarkdownLinkModel } from "./markdownLink";

export interface MarkdownLinkChoice {
  readonly key: string;
  readonly label: string;
  readonly value: string;
  readonly detail?: string;
}

export interface MarkdownLinkPopoverProps {
  readonly model: MarkdownLinkModel;
  readonly localFiles?: readonly MarkdownLinkChoice[];
  readonly headings?: readonly MarkdownLinkChoice[];
  readonly localFileDisabledReason?: string;
  readonly onApply: (draft: MarkdownLinkDraft) => string | null | undefined;
  readonly onRemove?: () => string | null | undefined;
  readonly onChooseLocalFile?: (choice: MarkdownLinkChoice) => void;
  readonly onRequestLocalFiles?: () => void;
  readonly onChooseHeading?: (choice: MarkdownLinkChoice) => void;
  readonly onShortcut?: () => void;
  readonly onOpenSource?: () => void;
  readonly onCancel?: () => void;
}

export function MarkdownLinkPopover({
  model,
  localFiles = [],
  headings = [],
  localFileDisabledReason,
  onApply,
  onRemove,
  onChooseLocalFile,
  onRequestLocalFiles,
  onChooseHeading,
  onShortcut,
  onOpenSource,
  onCancel,
}: MarkdownLinkPopoverProps): ReactNode {
  const modelKey = `${model.mode}:${model.from}:${model.to}:${model.address}:${model.text}`;
  const [text, setText] = useState(model.text);
  const [address, setAddress] = useState(model.address);
  const [title, setTitle] = useState(model.title);
  const [error, setError] = useState<string | null>(null);
  const addressRef = useRef<HTMLInputElement>(null);
  const lastModelKey = useRef(modelKey);
  const shortcutCallbackRef = useRef(onShortcut);
  shortcutCallbackRef.current = onShortcut;

  useEffect(() => {
    if (lastModelKey.current === modelKey) return;
    lastModelKey.current = modelKey;
    setText(model.text);
    setAddress(model.address);
    setTitle(model.title);
    setError(null);
  }, [model, modelKey]);

  useEffect(() => {
    if (model.mode === "protected") return;
    addressRef.current?.focus();
  }, [model.mode]);

  useEffect(() => {
    if (model.mode === "protected") return;
    // Ctrl/Cmd+K through a registry scoped to this popover (bubble phase on the window, as the
    // listener it replaces): Ctrl, Meta or both, never with Alt or Shift; an already-prevented
    // event is skipped.
    const registry = createShortcutRegistry();
    registry.register({
      id: "markdown-link.focus-address",
      label: "Focus link address",
      chord: ["ctrl+k", "meta+k", "ctrl+meta+k"],
      hidden: true,
      allowComposing: true,
      run: () => {
        addressRef.current?.focus();
        shortcutCallbackRef.current?.();
      },
    });
    return registry.attach(window);
  }, [model.mode]);

  // Escape inside the popover cancels it (bubble phase on the popover root, which is the protected
  // notice or the form, never both).
  // The root element lives in state, so a mode change (protected notice to form: different
  // elements) hands `useDismiss` a new ref object and re-binds the listener to the live node.
  const [root, setRoot] = useState<HTMLElement | null>(null);
  const rootRef = useMemo(() => ({ current: root }), [root]);
  useDismiss(() => onCancel?.(), { escapeTarget: rootRef, outside: false, imeGuard: false });

  if (model.mode === "protected") {
    return (
      <section
        className="space-y-3 p-3"
        data-testid="markdown-link-popover"
        role="dialog"
        aria-modal="true"
        aria-labelledby="markdown-link-dialog-title"
        ref={setRoot}
      >
        <header>
          <h2 id="markdown-link-dialog-title" className="text-sm font-semibold text-paper">
            Link editing unavailable
          </h2>
          <p className="mt-1 text-xs text-muted-2">{model.protectedReason}</p>
        </header>
        <div className="flex gap-2">
          {onOpenSource && (
            <button type="button" className={secondaryButtonClass} onClick={onOpenSource}>
              Open in Source
            </button>
          )}
          {onCancel && (
            <button type="button" className={secondaryButtonClass} onClick={onCancel}>
              Cancel
            </button>
          )}
        </div>
      </section>
    );
  }

  const submit = () => {
    const nextError = onApply({ text, address, title });
    setError(nextError ?? null);
  };
  const chooseLocalFile = (choice: MarkdownLinkChoice) => {
    setAddress(choice.value);
    setError(null);
    onChooseLocalFile?.(choice);
  };
  const chooseHeading = (choice: MarkdownLinkChoice) => {
    setAddress(choice.value);
    setError(null);
    onChooseHeading?.(choice);
  };

  return (
    <form
      className="space-y-3 p-3"
      data-testid="markdown-link-popover"
      role="dialog"
      aria-modal="true"
      aria-labelledby="markdown-link-dialog-title"
      ref={setRoot}
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <header>
        <h2 id="markdown-link-dialog-title" className="text-sm font-semibold text-paper">
          {model.mode === "edit" ? "Edit link" : "Add link"}
        </h2>
      </header>
      <label className="flex flex-col gap-1">
        <span className="text-xs font-semibold text-paper">Text</span>
        <input
          type="text"
          value={text}
          onChange={(event) => setText(event.currentTarget.value)}
          className={inputClass}
          placeholder="Selected text"
          aria-label="Text"
        />
      </label>
      <label className="flex flex-col gap-1">
        <span className="text-xs font-semibold text-paper">Address</span>
        <input
          ref={addressRef}
          type="text"
          required
          value={address}
          onChange={(event) => setAddress(event.currentTarget.value)}
          className={inputClass}
          placeholder="https://example.com or notes.md#section"
          aria-label="Address"
          autoComplete="off"
        />
      </label>
      <label className="flex flex-col gap-1">
        <span className="text-xs font-semibold text-paper">Title (optional)</span>
        <input
          type="text"
          value={title}
          onChange={(event) => setTitle(event.currentTarget.value)}
          className={inputClass}
          aria-label="Title"
        />
      </label>

      <div className="flex flex-wrap gap-2 border-t border-line pt-2">
        <button
          type="button"
          className={secondaryButtonClass}
          disabled={Boolean(localFileDisabledReason)}
          title={localFileDisabledReason}
          onClick={() => {
            if (localFiles[0]) chooseLocalFile(localFiles[0]);
            else onRequestLocalFiles?.();
          }}
        >
          Choose local file
        </button>
        <button
          type="button"
          className={secondaryButtonClass}
          disabled={headings.length === 0}
          onClick={() => headings[0] && chooseHeading(headings[0])}
        >
          Choose heading
        </button>
      </div>
      {localFileDisabledReason && (
        <p className="text-chrome-xs text-muted-2">{localFileDisabledReason}</p>
      )}
      {localFiles.length > 0 && (
        <ChoiceList label="Local files" choices={localFiles} onChoose={chooseLocalFile} />
      )}
      {headings.length > 0 && (
        <ChoiceList label="Headings" choices={headings} onChoose={chooseHeading} />
      )}
      {error && (
        <p
          role="alert"
          className="rounded border border-red-500/30 bg-red-500/10 px-2 py-1 text-xs text-red-300"
        >
          {error}
        </p>
      )}
      <div className="flex gap-2 pt-1">
        <button type="submit" className={primaryButtonClass}>
          Apply
        </button>
        {model.mode === "edit" && onRemove && (
          <button
            type="button"
            className={dangerButtonClass}
            onClick={() => setError(onRemove() ?? null)}
          >
            Remove link
          </button>
        )}
        {onCancel && (
          <button type="button" className={secondaryButtonClass} onClick={onCancel}>
            Cancel
          </button>
        )}
      </div>
    </form>
  );
}

function ChoiceList({
  label,
  choices,
  onChoose,
}: {
  readonly label: string;
  readonly choices: readonly MarkdownLinkChoice[];
  readonly onChoose?: (choice: MarkdownLinkChoice) => void;
}) {
  return (
    <div className="space-y-1">
      <div className="text-chrome-xs font-semibold uppercase tracking-[0.12em] text-muted-2">
        {label}
      </div>
      <div className="max-h-32 overflow-y-auto rounded border border-line">
        {choices.map((choice) => (
          <button
            type="button"
            key={choice.key}
            className="block w-full truncate px-2 py-1.5 text-left text-xs text-paper hover:bg-ink/60"
            onClick={() => onChoose?.(choice)}
          >
            {choice.label}
            {choice.detail && <span className="ml-2 text-muted-2">{choice.detail}</span>}
          </button>
        ))}
      </div>
    </div>
  );
}

const inputClass = "rounded border border-line bg-ink px-2 py-1.5 text-xs text-paper";
const primaryButtonClass =
  "flex-1 rounded bg-copper px-3 py-1.5 text-xs font-semibold text-white hover:bg-copper/90";
const secondaryButtonClass =
  "rounded border border-line bg-panel-2 px-3 py-1.5 text-xs text-muted hover:text-paper disabled:cursor-not-allowed disabled:opacity-50";
const dangerButtonClass =
  "rounded border border-red-500/40 px-3 py-1.5 text-xs text-red-300 hover:bg-red-500/10";
