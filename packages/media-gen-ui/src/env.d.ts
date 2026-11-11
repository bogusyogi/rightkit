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
