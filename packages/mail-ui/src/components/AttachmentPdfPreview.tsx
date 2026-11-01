import { type FormEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { PdfViewer } from "@rightkit/pdf-ui";
import "@rightkit/pdf-ui/styles.css";
import AttachmentTypeIcon from "./AttachmentTypeIcon";
import {
  MAX_PDF_PAGES,
  pdfPasswordCopy,
  type PdfPasswordReason,
} from "../lib/attachmentPdf";

import { Button, Input } from "@rightkit/react-shell/react";
type PdfPasswordRequest = {
  reason: PdfPasswordReason;
  resolve: (password: string | null) => void;
};

const INITIAL_VIEW = { page: 1, fit: null, scale: 1.25 } as const;

const truncationNotice = ({ shown, total }: { shown: number; total: number }) =>
  `Showing first ${shown} of ${total} pages.`;

export function AttachmentPdfPreview({ bytes, filename }: { bytes: Uint8Array; filename: string }) {
  const passwordInputRef = useRef<HTMLInputElement | null>(null);
  const [loading, setLoading] = useState(true);
  const [unlocking, setUnlocking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [passwordRequest, setPasswordRequest] = useState<PdfPasswordRequest | null>(null);
  const [passwordInput, setPasswordInput] = useState("");

  // The exact bytes the modal passed in; pdf-ui copies them before handing them to pdf.js.
  const source = useMemo(() => ({ kind: "bytes" as const, data: bytes }), [bytes]);

  // pdf-ui reloads the document when onError/onLoadingChange identities change, so keep them stable.
  const handleLoadingChange = useCallback((value: boolean) => setLoading(value), []);
  const handleError = useCallback((cause: Error) => {
    setPasswordRequest(null);
    setUnlocking(false);
    setLoading(false);
    setError(cause.message);
  }, []);
  // Resolves with the typed password, or null to give up (pdf-ui then reports a cancellation).
  const handlePassword = useCallback(
    (reason: PdfPasswordReason) =>
      new Promise<string | null>((resolve) => {
        setError(null);
        setUnlocking(false);
        setPasswordInput("");
        setPasswordRequest({ reason, resolve });
      }),
    [],
  );

  useEffect(() => {
    if (passwordRequest) passwordInputRef.current?.focus();
  }, [passwordRequest]);

  function handlePasswordSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!passwordRequest || passwordInput.length === 0) return;
    const resolve = passwordRequest.resolve;
    const nextPassword = passwordInput;
    setPasswordRequest(null);
    setPasswordInput("");
    setUnlocking(true);
    resolve(nextPassword);
  }

  const status = loading ? (unlocking ? "Unlocking PDF..." : "Rendering PDF...") : "";
  const copy = passwordRequest ? pdfPasswordCopy(passwordRequest.reason) : null;

  return (
    <div className="attachment-pdf-preview">
      {passwordRequest && copy && (
        <form className="attachment-password-form" onSubmit={handlePasswordSubmit}>
          <AttachmentTypeIcon filename={filename} mimeType="application/pdf" size="lg" />
          <div className="attachment-password-copy">
            <span className="attachment-password-title">{copy.title}</span>
            <span>{copy.body}</span>
          </div>
          <Input
            ref={passwordInputRef}
            className="attachment-password-input"
            type="password"
            value={passwordInput}
            onChange={(event) => setPasswordInput(event.currentTarget.value)}
            placeholder="PDF password"
            aria-label={`Password for ${filename}`}
            autoComplete="off"
            spellCheck={false}
          />
          <div className="attachment-password-actions">
            <Button variant="primary" size="sm" type="submit" disabled={passwordInput.length === 0}>
              Open PDF
            </Button>
          </div>
        </form>
      )}
      {!passwordRequest && status && <div className="attachment-preview-loading">{status}</div>}
      {!passwordRequest && error && (
        <div className="attachment-preview-fallback">
          <AttachmentTypeIcon filename={filename} mimeType="application/pdf" size="lg" />
          <span>PDF preview failed.</span>
          <span className="attachment-preview-error">{error}</span>
        </div>
      )}
      {/* Stays mounted while the password prompt is open so the pending unlock is not torn down. */}
      <div
        hidden={Boolean(passwordRequest || error)}
        style={{ flex: "1 1 auto", minHeight: 0, display: "flex", flexDirection: "column" }}
      >
        <PdfViewer
          source={source}
          initialView={INITIAL_VIEW}
          hideToolbar
          maxRenderPages={MAX_PDF_PAGES}
          onPassword={handlePassword}
          onError={handleError}
          onLoadingChange={handleLoadingChange}
          truncationNotice={truncationNotice}
          className="attachment-pdf-viewer"
        />
      </div>
    </div>
  );
}
