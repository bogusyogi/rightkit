import { useEffect, useRef, useState } from "react";
import { ActionDialog } from "@rightkit/react-shell/react";
import { DIALOG_SLOTS, cancelAction, dangerAction, primaryAction } from "@rightkit/document-core/app/chrome";

// Notepad/Word-style link dialog: Display text + Address, shared by source editors so link
// insertion is consistent across formats. Keyboard: autofocus, Enter inserts, Esc cancels. The
// caller restores the saved selection and performs the actual insert in onInsert.
export function LinkDialog({
  initialText,
  initialUrl = "",
  onInsert,
  onCancel,
  onRemove,
}: {
  initialText: string;
  initialUrl?: string;
  onInsert: (text: string, url: string) => void;
  onCancel: () => void;
  /** Provided when editing an existing link — shows a "Remove link" button. */
  onRemove?: () => void;
}) {
  const [text, setText] = useState(initialText);
  const [url, setUrl] = useState(initialUrl);
  const textRef = useRef<HTMLInputElement>(null);
  const urlRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    // If text is already selected, jump straight to the address; otherwise start with display text.
    (initialText ? urlRef.current : textRef.current)?.focus();
  }, [initialText]);

  const submit = () => {
    const u = url.trim();
    if (!u) return;
    onInsert(text.trim() || u, u); // insert exactly as typed — no rewriting
  };

  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "Enter") {
      e.preventDefault();
      submit();
    }
  };

  return (
    <ActionDialog
      title="Link"
      ariaLabel="Link"
      onClose={onCancel}
      width={420}
      autoFocus={false}
      slots={{ ...DIALOG_SLOTS, header: `mb-4 ${DIALOG_SLOTS.header}` }}
      actions={[
        ...(onRemove ? [dangerAction("Remove link", onRemove)] : []),
        cancelAction(onCancel),
        { ...primaryAction(onRemove ? "Update" : "Insert", submit), disabled: !url.trim() },
      ]}
    >
      <label
        htmlFor="vr-link-text"
        className="mb-1 block text-chrome-xs uppercase tracking-[0.15em] text-muted-2"
      >
        Display text
      </label>
      <input
        id="vr-link-text"
        ref={textRef}
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={onKey}
        placeholder="Text to show"
        className="mb-4 w-full rounded border border-line bg-ink px-3 py-1.5 text-sm text-paper outline-none focus:border-copper"
      />
      <label
        htmlFor="vr-link-url"
        className="mb-1 block text-chrome-xs uppercase tracking-[0.15em] text-muted-2"
      >
        Address
      </label>
      <input
        id="vr-link-url"
        ref={urlRef}
        value={url}
        onChange={(e) => setUrl(e.target.value)}
        onKeyDown={onKey}
        placeholder="https://example.com"
        className="mb-5 w-full rounded border border-line bg-ink px-3 py-1.5 font-mono text-sm text-paper outline-none focus:border-copper"
      />
    </ActionDialog>
  );
}
