// The product's one signing identity, in a file with no imports: the gate's route decision (rightkit-gate.mjs, which runs from an installed package with
// no node_modules) and the signers both read it, so the two cannot name different identities (rightkit#1303).

/** The one Apple team every product signs with, release and development. */
export const DEFAULT_TEAM = "6KLGD3LLKF";
/** The signer subject every Windows artifact carries (Azure Artifact Signing's certificate profile), as `signtool verify` reports it. */
export const WINDOWS_SIGNER_SUBJECT = "CN=Damned Ventures LLC";
