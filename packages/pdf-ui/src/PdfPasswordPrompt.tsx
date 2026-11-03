import { useEffect, useRef, useState } from "react";

export interface PdfPasswordPromptProps {
  /** True when the previous password was wrong: the dialog says so. */
  retry: boolean;
  onSubmit: (password: string) => void;
  onCancel: () => void;
}

/** The viewer's built-in password dialog (`PdfViewer` prop `passwordPrompt`). A modal region over
 *  the viewer: the field takes focus, Enter unlocks, Escape cancels, Tab stays inside the dialog.
 *  Hosts with their own dialog use `onPassword` instead. */
export function PdfPasswordPrompt({ retry, onSubmit, onCancel }: PdfPasswordPromptProps) {
  const [password, setPassword] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => { inputRef.current?.focus(); }, []);
  return <div className="rk-pdf-password-backdrop">
    <form
      className="rk-pdf-password"
      role="dialog"
      aria-modal="true"
      aria-labelledby="rk-pdf-password-title"
      aria-describedby="rk-pdf-password-description"
      onSubmit={(event) => { event.preventDefault(); if (password) onSubmit(password); }}
      onKeyDown={(event) => {
        if (event.key === "Escape") { event.preventDefault(); onCancel(); return; }
        if (event.key !== "Tab") return;
        const items = Array.from(event.currentTarget.querySelectorAll<HTMLElement>("input, button:not(:disabled)"));
        const first = items[0];
        const last = items[items.length - 1];
        if (!first || !last) return;
        if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
        else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
      }}
    >
      <h2 id="rk-pdf-password-title">Password required</h2>
      <p id="rk-pdf-password-description" role={retry ? "alert" : undefined}>{retry ? "Incorrect password. Try again." : "This PDF is password-protected."}</p>
      <input ref={inputRef} type="password" aria-label="PDF password" placeholder="Password" autoComplete="off" value={password} onChange={(event) => setPassword(event.target.value)} />
      <div className="rk-pdf-password-actions">
        <button type="button" onClick={onCancel}>Cancel</button>
        <button type="submit" disabled={!password}>Unlock</button>
      </div>
    </form>
  </div>;
}
