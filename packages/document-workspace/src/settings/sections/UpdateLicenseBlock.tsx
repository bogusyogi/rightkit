import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  SettingRow,
  SettingsGroup,
  licenseSection,
  type LicenseClientLike,
  type LicenseLabels,
} from "@rightkit/settings-ui";
import { LicenseClientError, isLicensed, isPro, licenseMessage, type LicenseView } from "@rightkit/license";
import { openExternal } from "@rightkit/document-core/state/tauriBridge";
import { UPDATE_LICENSE_URL, getLicenseClient } from "../../state/updateLicense";
import { PublishedLicense } from "../publishedSections";

/** Fields the published section displays; a native change that alters none of them needs no re-read. */
const signature = (v: LicenseView) =>
  [v.mode, v.status, v.lifecycle, v.revisionState, v.kind, v.tier, v.validUntil, v.confirmationDueAt, v.staleSince, v.message]
    .map(String)
    .join("|");

/**
 * LicenseClientLike over the native client. Retained app code: the published section needs a
 * synchronous client and `getLicenseClient()` is async; the key is trimmed here because the package
 * passes the pasted text through untrimmed; the first read is memoised behind a loading gate. The key is only ever a
 * transient command argument.
 */
function createLicenseClientLike(): LicenseClientLike {
  const firstRead = async (): Promise<LicenseView> => {
    const client = await getLicenseClient();
    return client.status();
  };
  let first: Promise<LicenseView> | null = null;
  let firstDone = false;
  return {
    status: async () => {
      if (!firstDone) {
        first ??= firstRead().finally(() => {
          firstDone = true;
        });
        return first;
      }
      return (await getLicenseClient()).status();
    },
    activate: async (key) => (await getLicenseClient()).activate(key.trim()),
    deactivate: async () => (await getLicenseClient()).deactivate(),
    refresh: async () => (await getLicenseClient()).refresh(),
  };
}

/** Kind badges stay neutral; the "updates" claims below depend on the view's entitlement. */
const KIND = { trial: "Trial", term: "Term", lifetime: "Lifetime" } as const;

function licenseLabels(pro: boolean): LicenseLabels {
  return {
    sectionLabel: "Update license",
    statusLabel: "Update license",
    activeStatusLabel: "Update license",
    searchStatus: "Update license status",
    searchActivate: "Activate update license key",
    free: "Free public build",
    freeDetail: "Activate a key for signed in-app updates.",
    attention: "Update license — needs attention",
    expired: "Expired",
    trial: KIND.trial,
    subscription: KIND.term,
    lifetime: KIND.lifetime,
    activeDetail: (kind, until) => {
      if (!pro) return `${kind} license active on this device.`;
      if (kind === KIND.lifetime) return "Lifetime updates active on this device.";
      if (kind === KIND.trial) return `Trial updates active${until ? ` through ${until}` : ""}.`;
      return `Updates active${until ? ` through ${until}` : ""}.`;
    },
    expiredDetail: (kind, until) =>
      kind === KIND.trial
        ? "Update trial expired."
        : `Update license expired${until ? ` on ${until}` : ""}.`,
    deactivateTitle: "Remove this license",
    deactivateHint: "Frees this device so you can enter a different key.",
    deactivate: "Remove license",
    keyTitle: "License key",
    keyHint: "Paste the key from your purchase email.",
    keyAria: "Update license key",
    keyPlaceholder: "VR-XXXX-XXXX-XXXX",
    activate: "Activate",
    activating: "Activating...",
    purchaseTitle: "Need an update license?",
    purchase: "Buy update license",
    verifyTitle: "Update license check",
    verifyHint: "Connect once to verify this purchase.",
    verify: "Verify purchase",
  };
}

/**
 * the host app update-license block: the published `licenseSection()` (`@rightkit/settings-ui` 0.7.0)
 * over the native licence client. App-owned remainder, each piece with its reason:
 * the lazy client above, the Disabled-build notice, the loading gate (the package shows "Free"
 * until its first read settles), the non-Pro notice (the package marks any active licence as
 * licensed), the cross-window re-read on native change events (the package only reads on mount),
 * and the label map. The webview holds no key or token: the key is a transient command input and
 * the view is the only state. Record: docs/rightkit-exceptions/M12.md#m12-update-license-block.
 */
export function UpdateLicenseBlock() {
  const client = useMemo(createLicenseClientLike, []);
  const [view, setView] = useState<LicenseView | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const shown = useRef("");

  const adopt = useCallback((next: LicenseView | null) => {
    if (!next) return;
    shown.current = signature(next);
    setView(next);
  }, []);

  useEffect(() => {
    let cancelled = false;
    let stop: (() => void) | undefined;
    void (async () => {
      try {
        const first = await client.status!();
        if (cancelled) return;
        adopt(first);
        const release = await (await getLicenseClient()).onChange(
          (changed) => {
            if (cancelled || signature(changed) === shown.current) return;
            adopt(changed);
            setRevision((n) => n + 1); // remount the section so its own read picks the change up
          },
          () => undefined,
        );
        if (cancelled) release();
        else stop = release;
      } catch (error) {
        if (!cancelled) setFailure(error instanceof LicenseClientError && error.message ? error.message : licenseMessage("unknown"));
      }
    })();
    return () => {
      cancelled = true;
      stop?.();
    };
  }, [client, adopt]);

  const licensed = isLicensed(view);
  const pro = isPro(view);
  const section = useMemo(
    () =>
      licenseSection({
        client,
        confirmDeactivate: false,
        purchaseUrl: UPDATE_LICENSE_URL,
        openExternal,
        tierLabels: { pro: "Pro" },
        onChange: adopt,
        labels: licenseLabels(pro),
        planSlot:
          licensed && !pro ? (
            <SettingsGroup>
              <SettingRow
                label="License active, updates not included"
                description="This license does not include signed paid updates. Public patches stay available."
              />
            </SettingsGroup>
          ) : undefined,
      }),
    [client, adopt, licensed, pro],
  );

  let body: ReactNode = null;
  if (failure) {
    body = (
      <SettingsGroup title="Update license">
        <SettingRow label="Update license" description={<span role="alert">{failure}</span>} />
      </SettingsGroup>
    );
  } else if (view?.mode === "disabled") {
    body = (
      <SettingsGroup title="Update license">
        <SettingRow label="Update license" description={licenseMessage("unconfigured")} />
      </SettingsGroup>
    );
  } else if (view) {
    body = <PublishedLicense key={revision} section={section} />;
  }

  return (
    <div data-update-license aria-busy={!view && !failure ? true : undefined}>
      {body}
    </div>
  );
}
