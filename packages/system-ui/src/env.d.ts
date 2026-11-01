/// <reference types="vite/client" />

declare module "*.css";
declare module "*.svg" {
  const src: string;
  export default src;
}
declare module "*.png" {
  const src: string;
  export default src;
}
declare module "*.txt?raw" {
  const text: string;
  export default text;
}
declare module "*.tsv?raw" {
  const text: string;
  export default text;
}
declare module "*?url" {
  const src: string;
  export default src;
}

// Build-time switches a host may define (Vite `define`); every read is typeof-guarded.
declare const __RIGHTKIT_QA__: boolean | undefined;
declare const __RIGHTKIT_DEMO__: boolean | undefined;
declare const __RIGHTKIT_MOCK__: boolean | undefined;
declare const __APP_VERSION__: string | undefined;

interface Window {
  /** UI QA only: the data source a QA shell selects. */
  __RIGHTKIT_QA_DATA_SOURCE__?: "engine";
}

// Host Vite plugin module with the legal documents shown in the first-run gate.
declare module "virtual:rightkit-legal" {
  export const LEGAL_DOCUMENTS: readonly { title: string; content: string }[];
  export const LEGAL_VERSION: string;
  export const LEGAL_DOCUMENTS_HASH: string;
  export const LEGAL_RECEIPT_DOCUMENTS: readonly { id: string; role: string; version: string; sha256: string; treatment: string; material: boolean }[];
}

declare module "@rightkit-host/legal/legal-manifest.json" {
  const manifest: {
    documents: Array<{ id: string; role: string; title: string; publicUrl: string; version?: string; sha256?: string; [key: string]: unknown }>;
    [key: string]: unknown;
  };
  export default manifest;
}

declare module "@rightkit-host/legal/legal-manifest.json?raw" {
  const text: string;
  export default text;
}

declare module "@rightkit-host/src/generated/legal-contract.json" {
  const contract: { acceptanceVersion: string; materialSha256: string; [key: string]: unknown };
  export default contract;
}
