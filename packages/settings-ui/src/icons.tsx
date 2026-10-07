import type { ReactNode } from "react";

const svg = (children: ReactNode) => (
  <svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    {children}
  </svg>
);

/** Nav icons for the built-in sections (currentColor, 16px). Apps can replace any of them with `override: { id: { icon } }`. */
export const sectionIcons = {
  appearance: svg(<><circle cx="8" cy="8" r="5.5" /><path d="M8 2.5v11" /><path d="M8 2.5a5.5 5.5 0 0 1 0 11z" fill="currentColor" stroke="none" /></>),
  updates: svg(<><path d="M13 8a5 5 0 1 1-1.6-3.7" /><path d="M13 2.5V5h-2.5" /></>),
  models: svg(<><rect x="3" y="3" width="10" height="10" rx="2" /><path d="M6 6h4v4H6z" /></>),
  license: svg(<><rect x="2.5" y="4" width="11" height="8" rx="1.5" /><path d="M5 7.5h6M5 10h3" /></>),
  legal: svg(<><path d="M4 2.5h5.5L12 5v8.5H4z" /><path d="M9.5 2.5V5H12M6 8h4M6 10.5h4" /></>),
  apiKeys: svg(<><circle cx="5.5" cy="10.5" r="2.5" /><path d="M7.3 8.7 13 3M11 5l1.5 1.5" /></>),
  diagnostics: svg(<><path d="M2 8h3l1.5-4 3 8L11 8h3" /></>),
  about: svg(<><circle cx="8" cy="8" r="5.5" /><path d="M8 7.5V11M8 5.2v.1" /></>),
} as const satisfies Record<string, ReactNode>;
