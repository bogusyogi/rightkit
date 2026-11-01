/** Shared PDF preview policy for attachments. Pure so it can be tested without pdf.js or React. */

/** Pages rendered inline; longer documents show the first pages with a notice. */
export const MAX_PDF_PAGES = 30;

/** Reasons reported by `@rightkit/pdf-ui` `onPassword`. */
export type PdfPasswordReason = "need" | "incorrect";

export function pdfPasswordCopy(reason: PdfPasswordReason): { title: string; body: string } {
  return reason === "incorrect"
    ? {
        title: "Incorrect password",
        body: "That password did not unlock the PDF. Try again.",
      }
    : {
        title: "Password required",
        body: "Enter the password to preview this PDF.",
      };
}
