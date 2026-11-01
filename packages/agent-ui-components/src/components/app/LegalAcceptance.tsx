import { LegalGate, type LegalGateDocument } from "@rightkit/legal-ui";
import "@rightkit/legal-ui/legal-ui.css";
import {
  legalManifest,
  REQUIRED_LEGAL_ACCEPTANCE_VERSION,
  useLegalStore,
} from "@rightkit/agent-ui-core/state/legal";
import { hostAppName } from "../../appName";

/**
 * First-run gate for the Right Suite Desktop License v1.1 (individual/enterprise model, no Worker
 * threshold). Rendered by `LegalGate` in place of every other window content until
 * the stored receipt satisfies the bundled manifest (`needsLegalAcceptance` is false).
 *
 * Presentation is RightKit's shared `LegalGate` (@rightkit/legal-ui). The acceptance RECORD stays
 * native-owned: `onAgree` hands the chosen basis to `accept`, which declares what the gate showed to
 * the native legal receipt service; the service stamps and stores the receipt.
 *
 * Documents come from the bundled manifest (same order and titles as the contract), not copies.
 * Colours and faces are host CSS variables so the gate follows the host app dark-by-default theme.
 */

const LICENSE_TITLE = "Right Suite Desktop License v1.1";

const documents: LegalGateDocument[] = legalManifest.documents.map((document) => ({
  title: document.title,
  url: document.publicUrl,
}));

const trustLine =
  `${hostAppName()} is free for an individual on devices they own or personally control, including their own freelance work. Any use by or for an Organization — on its devices, or that it pays for or directs — is Enterprise use and needs an Enterprise Order with a Seat for each Worker before that use continues.`;

const agreementStatement =
  "I agree to the EULA, Ownership Notice, Acceptable Use Policy, and Product Schedule. I confirm I have authority to accept them for the person or Organization I selected above. I acknowledge the Privacy Notice; this acknowledgment is not consent to optional data use.";

const theme = {
  bg: "var(--color-bg)",
  surface: "var(--color-panel)",
  border: "var(--color-line)",
  text: "var(--color-text)",
  muted: "var(--color-text-3)",
  accent: "var(--color-accent)",
  accentContrast: "var(--color-on-accent)",
  fontDisplay: "var(--font-display)",
  fontBody: "var(--font-body)",
};

/** Closes this window; the copy asks the user to close the host app if none of the bases apply. */
function quitHost() {
  void import("@tauri-apps/api/window")
    .then(({ getCurrentWindow }) => getCurrentWindow().close())
    .catch(() => {
      // Outside Tauri (browser QA) there is no window to close; the gate stays on screen.
    });
}

export default function LegalAcceptance() {
  const accept = useLegalStore((state) => state.accept);

  return (
    <main className="legal-acceptance h-screen w-screen overflow-y-auto">
      <LegalGate
        theme={theme}
        appName={hostAppName()}
        trustLine={trustLine}
        documents={documents}
        agreementStatement={agreementStatement}
        variant="first-run"
        version={REQUIRED_LEGAL_ACCEPTANCE_VERSION}
        onAgree={({ eligibilityBasis }) => accept(eligibilityBasis)}
        onQuit={quitHost}
        eligibilityLabels={{
          legend: "Which basis applies to you?",
          individual: {
            label:
              `I'm using ${hostAppName()} as an individual, on my own or personally controlled devices.`,
            description: "",
          },
          enterprise: {
            label:
              `I'm using ${hostAppName()} for or on behalf of an Organization, under a valid Enterprise Order (or within the 30-day window to obtain one).`,
            description: "",
          },
        }}
        lockup={
          <div className="flex flex-col items-center gap-1">
            <span
              className="font-mono text-xs uppercase tracking-wide"
              style={{ color: "var(--color-text-3)" }}
            >
              {LICENSE_TITLE}
            </span>
            <span
              className="text-2xl font-semibold leading-none"
              style={{ color: "var(--color-text)", fontFamily: "var(--font-display)" }}
            >
              Code<span style={{ color: "var(--color-accent)" }}>Right</span>
            </span>
          </div>
        }
        dbaLine={`If none applies, close ${hostAppName()} or contact legal@damnedventures.com.`}
      />

      {/* Silent audit anchor: keeps the acceptance version visibly wired to what this screen
          actually renders. */}
      <p className="sr-only">acceptance version {REQUIRED_LEGAL_ACCEPTANCE_VERSION}</p>
    </main>
  );
}
