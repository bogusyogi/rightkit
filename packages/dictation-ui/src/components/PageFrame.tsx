// PageFrame: the wrapper the Dictation, Polish and Home pages render in. The host's
// `components.PageShell` when it supplies one, else the shared shell with the page's classes.

import { PageShell } from "@rightkit/react-shell/react";
import type { ReactNode } from "react";
import { hostComponents, type DictationPageName } from "../host.js";

export default function PageFrame({ page, children }: { page: DictationPageName; children: ReactNode }) {
  const Custom = hostComponents().PageShell;
  if (Custom) return <Custom page={page}>{children}</Custom>;
  return (
    <PageShell maxWidth={1320} className={`dictation-page dictation-page--${page}`}>
      {children}
    </PageShell>
  );
}
