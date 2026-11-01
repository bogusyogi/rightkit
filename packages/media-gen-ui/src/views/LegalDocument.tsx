import { LegalDocumentBody } from '@rightkit/legal-ui';
import { Button, ModalShell } from '@rightkit/react-shell/react';
import { legalDocument, type LegalDocumentId } from '../legal/documents';

/**
 * Reads one bundled legal document in the shell's modal surface. Opened from the onboarding gate's
 * document links; closes on the Close button, a backdrop click, or Escape (ModalShell).
 */
export function LegalDocumentView({ id, onClose }: { id: LegalDocumentId; onClose: () => void }) {
  const doc = legalDocument(id);
  return (
    <ModalShell label={doc.title} labelledBy="legal-document-title" onClose={onClose} width={720} dismissOn="click">
      <section className="legal-doc" data-document-id={doc.id}>
        <header className="legal-doc-head">
          <div>
            <h1 id="legal-document-title">{doc.title}</h1>
            <p>Version {doc.version}</p>
          </div>
          <Button variant="secondary" onClick={onClose}>Close</Button>
        </header>
        {/* `title` drops the Markdown's own matching H1; the dialog header already shows it. */}
        <LegalDocumentBody content={doc.content} format="markdown" title={doc.title} />
      </section>
    </ModalShell>
  );
}
