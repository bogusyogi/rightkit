import { useRef, useState } from "react";
import { ActionDialog } from "@rightkit/react-shell/react";
import { DIALOG_SLOTS, cancelAction, primaryAction } from "../../app/chrome.js";

// App-native PDF password modal (replaces window.prompt). Keyboard-accessible: autofocus, Enter to
// submit, Esc to cancel; the shared modal shell provides role="dialog", focus trap, and Escape.
export function PdfPasswordPrompt({
  retry,
  onSubmit,
  onCancel,
}: {
  retry: boolean;
  onSubmit: (pw: string) => void;
  onCancel: () => void;
}) {
  const [pw, setPw] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);
  return (
    <ActionDialog
      title="Password required"
      ariaLabel="Password required"
      description={retry ? "Incorrect password. Try again." : "This PDF is password-protected."}
      onClose={onCancel}
      width={400}
      slots={{ ...DIALOG_SLOTS, body: "mt-3" }}
      actions={[
        cancelAction(onCancel),
        { ...primaryAction("Unlock", () => pw && onSubmit(pw)), disabled: !pw },
      ]}
    >
      <input
        ref={inputRef}
        type="password"
        aria-label="PDF password"
        value={pw}
        onChange={(e) => setPw(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            if (pw) onSubmit(pw);
          } else if (e.key === "Escape") {
            e.preventDefault();
            onCancel();
          }
        }}
        placeholder="Password"
        className="mb-5 w-full rounded border border-line bg-ink px-3 py-1.5 text-sm text-paper outline-none focus:border-copper"
      />
    </ActionDialog>
  );
}
