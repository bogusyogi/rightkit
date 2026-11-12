// Build-time switches a host may define (Vite `define`); every read is typeof-guarded, so a host that defines
// neither gets the release behaviour (no QA dialog answers, no demo settings chunk).
declare const __RIGHTKIT_QA__: boolean | undefined;
declare const __RIGHTKIT_DEMO__: boolean | undefined;
