import { LEGAL_ABOUT_LINKS } from "../../state/legal.js";
import { AboutFrame, LegalPanel, UpdatesPanel } from "../publishedSections.js";
import { useSettings } from "@rightkit/document-core/settings/settings";
import { openExternal } from "@rightkit/document-core/state/tauriBridge";
import { UpdateLicenseBlock } from "./UpdateLicenseBlock.js";

const footerLink = "text-copper-hi underline-offset-4 hover:underline";

const brandCard = (
  <div className="rounded-lg border border-line bg-panel/55 px-4 py-3 text-sm text-muted">
    <div className="flex min-w-0 items-center justify-between gap-3">
      <div className="[font-family:var(--font-wordmark)] text-3xl leading-none tracking-[0.01em] text-paper">
        View<span className="text-copper">Right</span>
      </div>
      <div className="flex items-center gap-1.5 font-sans text-chrome-xs text-muted-2">
        <span className="rounded-full border border-line bg-ink/25 px-2 py-1">Local-first</span>
      </div>
    </div>
    <p className="mt-1 text-xs text-paper/75">
      Your documents, opened locally and kept on your device.
    </p>
    <div
      data-about-contact
      className="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 border-t border-line/70 pt-2 font-sans text-chrome-xs"
    >
      <a className={footerLink} href="https://viewright.cc">
        viewright.cc
      </a>
      <a className={footerLink} href="mailto:hello@viewright.cc">
        hello@viewright.cc
      </a>
    </div>
  </div>
);

const panels = (
  <div className="mt-3 flex flex-col gap-3">
    <UpdatesPanel />
    <UpdateLicenseBlock />
  </div>
);

export function AboutSection() {
  const { settings } = useSettings();
  return (
    <div data-about-section className="flex h-full min-h-0 flex-col pr-9 text-sm text-muted">
      <AboutFrame card={brandCard} panels={panels} />

      {/* Permanent legal documents remain available after installer acceptance. */}
      <p className="mt-3 font-sans text-chrome-xs text-muted-2">Right Suite Desktop License v1.0</p>
      <LegalPanel
        documents={LEGAL_ABOUT_LINKS}
        acceptedAt={settings.legalAcceptedAt || null}
        acceptedVersion={settings.legalAcceptanceVersion || null}
        openExternal={openExternal}
      />
    </div>
  );
}
