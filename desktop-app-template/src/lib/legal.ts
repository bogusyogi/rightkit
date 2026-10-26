// The legal agreement the first-run gate shows, from the generated files `legal-snapshot` writes (legal/legal-manifest.json and
// src/generated/legal-contract.json). The shell compiles the same two files into the receipt service, which refuses any
// declaration that differs from them.
import { acceptanceDeclaration, type LegalManifest } from "@rightkit/legal/browser";
import type { LegalStepConfig } from "@rightkit/onboarding";
import manifestJson from "../../legal/legal-manifest.json";
import contract from "../generated/legal-contract.json";

const manifest = manifestJson as LegalManifest;
const declaration = acceptanceDeclaration(manifest);

/** The acceptance version the receipt must carry; a new version re-prompts. */
export const requiredVersion = contract.acceptanceVersion;
/** The digest of the material documents (`materialAcceptanceDigest`); a text change re-prompts even without a version bump. */
export const requiredDocumentsHash = contract.materialSha256;
/** The document set declared to the native receipt, exactly as `acceptanceDeclaration` lists it. */
export const receiptDocuments = declaration.documents;

/** Every document with its public URL, for the gate and the settings Legal section. */
export const legalDocuments = manifest.documents.map((d) => ({ title: d.title, url: d.publicUrl }));

export const legalGate: LegalStepConfig = {
  appName: manifest.productName,
  trustLine: `${manifest.productName} runs on your computer.`,
  documents: legalDocuments,
  agreementStatement: "By continuing you agree to the documents above.",
  version: requiredVersion,
};
