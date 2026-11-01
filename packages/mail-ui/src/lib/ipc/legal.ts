import { createNativeLegalReceiptPort, type LegalReceiptPort } from "@rightkit/onboarding";
import { invoke } from "./invoke";

/** The native receipt port: create and reset are always present on this one. */
type NativeLegalReceiptPort = LegalReceiptPort &
  Required<Pick<LegalReceiptPort, "create" | "reset">>;

/**
 * The first-run acceptance receipt, native (RightKit M14, ledger A452). The shell's host services
 * answer `rightkit_legal_receipt_{read,create,reset}` (src-tauri/src/commands/legal.rs): the
 * renderer declares what it showed, native code compares it with the manifest it embeds, stamps the
 * UTC time itself and stores the record. There is no renderer-written receipt and no `write`.
 */
export const legalReceiptPort = createNativeLegalReceiptPort({ invoke }) as NativeLegalReceiptPort;
