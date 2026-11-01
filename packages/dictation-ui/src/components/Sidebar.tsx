// the host app navigation data and product status adapters for @rightkit/react-shell.

import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import type { AutoUpdateStatus } from "@rightkit/app-update";
import type { NavGroup } from "@rightkit/react-shell";
import { House, ClockCounterClockwise, Keyboard, TextAa, Sparkle, GearSix, Info, Crown, ArrowRight, DeviceMobile } from "@phosphor-icons/react";
import { invoke } from "../lib/tauri";
import { isMac } from "../hooks/usePlatform";
import { useSettings } from "../hooks/useSettings";
import { getLicenseLifecycle, subscribeLicenseLifecycle } from "../lib/licenseLifecycle";
import SidebarUpdateStatus from "./SidebarUpdateStatus";

export type TabKey =
  | "home"
  | "transcribe"
  | "dictation"
  | "shortcuts"
  | "controls"
  | "polish"
  | "settings"
  | "about"
  | "history";

export const NAV_GROUPS: readonly NavGroup[] = [
  {
    items: [
      { id: "home", label: "Home", icon: <House size={18} /> },
      { id: "history", label: "History", icon: <ClockCounterClockwise size={18} /> },
      { id: "controls", label: "Controls", icon: <Keyboard size={18} /> },
    ],
  },
  {
    title: "Voice & Text",
    items: [
      { id: "dictation", label: "Dictation", icon: <TextAa size={18} /> },
      { id: "polish", label: "Polish", icon: <Sparkle size={18} /> },
    ],
  },
  {
    title: "App",
    items: [
      { id: "settings", label: "Settings", icon: <GearSix size={18} /> },
      { id: "about", label: "About", icon: <Info size={18} /> },
    ],
  },
];

type CloudSyncStatus =
  | { state: "disabled" }
  | { state: "unavailable"; detail?: string | null }
  | { state: "ready" };

// Footer status rows: same 16px icon + label rhythm as the nav, so they read as places to go, not as tags. The row's 10px
// padding is cancelled by a -10px margin, so icon and status line up with the version line and wordmark; only the hover wash overhangs.
function StatusRow({
  icon,
  label,
  value,
  on,
  title,
  onOpen,
}: {
  icon: ReactNode;
  label: string;
  value: string;
  on?: boolean;
  title: string;
  onOpen: () => void;
}) {
  return (
    <button
      type="button"
      className="-mx-[10px] flex h-8 w-[calc(100%+20px)] items-center gap-[10px] rounded-[var(--radius-md)] border-0 bg-transparent px-[10px] text-left text-[length:var(--size-base)] text-[var(--muted)] transition-colors hover:bg-[color-mix(in_srgb,var(--paper),transparent_93%)] hover:text-[var(--paper)] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]"
      title={title}
      aria-label={`${label}: ${value}. ${title}`}
      onClick={onOpen}
    >
      <span className="grid h-4 w-4 shrink-0 place-items-center text-[var(--muted-2)]" aria-hidden="true">{icon}</span>
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {/* One fixed-width status cell per row: the dot always sits in the same column and the value starts at the same x. Green is "on"; a dim ring is off or still loading. Ember stays for actions, never for state. */}
      <span className={`inline-flex w-[80px] shrink-0 items-center gap-[7px] text-[length:var(--size-xs)] ${on ? "text-[var(--paper)]" : "text-[var(--muted-2)]"}`}>
        <span
          className={`h-[7px] w-[7px] shrink-0 rounded-full ${on ? "bg-[var(--ok)]" : "border border-[var(--muted-2)] opacity-70"}`}
          aria-hidden="true"
        />
        {value}
      </span>
    </button>
  );
}

// Fired by Apple sync settings after the toggle resolves, so this row (the hub window is reused) follows it without a restart.
export const CLOUD_SYNC_CHANGED_EVENT = "hr-cloud-sync-changed";

function IPhoneSyncChip({ onOpen }: { onOpen: () => void }) {
  const [state, setState] = useState<CloudSyncStatus["state"] | null>(null);
  useEffect(() => {
    let alive = true;
    const read = () => {
      invoke<CloudSyncStatus>("cloud_sync_status")
        .then((status) => alive && setState(status.state))
        .catch(() => alive && setState(null));
    };
    read();
    window.addEventListener(CLOUD_SYNC_CHANGED_EVENT, read);
    window.addEventListener("focus", read);
    return () => {
      alive = false;
      window.removeEventListener(CLOUD_SYNC_CHANGED_EVENT, read);
      window.removeEventListener("focus", read);
    };
  }, []);
  // Only "ready" is on: "unavailable" (sync is switched on but iCloud is not there) is not working, so it never reads On.
  return (
    <StatusRow
      icon={<DeviceMobile size={16} />}
      label="Apple sync"
      value={state === null ? "…" : state === "ready" ? "On" : state === "unavailable" ? "Unavailable" : "Off"}
      on={state === "ready"}
      title="Apple Sync settings"
      onOpen={onOpen}
    />
  );
}

function PlanChip({ onOpen }: { onOpen: () => void }) {
  const [lifecycle, setLifecycle] = useState(getLicenseLifecycle);
  useEffect(() => subscribeLicenseLifecycle(setLifecycle), []);
  // Display only: the lifecycle monitor's native read. While it is still checking, show a neutral row, not the upgrade call to action.
  if (lifecycle.kind === "checking") {
    return <StatusRow icon={<Crown size={16} weight="fill" />} label="Plan" value="…" title="Plan and license" onOpen={onOpen} />;
  }
  if (lifecycle.isPro) {
    return <StatusRow icon={<Crown size={16} weight="fill" />} label="Pro" value="Active" on title="Plan and license" onOpen={onOpen} />;
  }
  // The one upgrade call to action in the shell: tinted ember, not a grey tag.
  return (
    <button
      type="button"
      className="flex h-9 w-full items-center gap-[10px] rounded-[var(--radius-md)] border border-[color-mix(in_srgb,var(--accent),transparent_62%)] bg-[color-mix(in_srgb,var(--accent),transparent_88%)] px-[10px] text-left text-[length:var(--size-base)] font-semibold text-[var(--paper)] transition-colors hover:bg-[color-mix(in_srgb,var(--accent),transparent_80%)] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]"
      title="Compare plans"
      aria-label="Get Pro. Compare plans"
      onClick={onOpen}
    >
      <Crown size={16} weight="fill" className="shrink-0 text-[var(--accent)]" aria-hidden="true" />
      <span className="min-w-0 flex-1 truncate">Get Pro</span>
      <ArrowRight size={14} weight="bold" className="shrink-0 text-[var(--accent)]" aria-hidden="true" />
    </button>
  );
}

function PolishChip({ onOpen }: { onOpen: () => void }) {
  const { loaded, get } = useSettings();
  const [lifecycle, setLifecycle] = useState(getLicenseLifecycle);
  useEffect(() => subscribeLicenseLifecycle(setLifecycle), []);
  const checking = lifecycle.kind === "checking";
  // AI polish only runs for Pro, so the setting alone never reads On.
  const on = loaded && !checking && lifecycle.isPro && get("ai_polish_enabled", false);
  return (
    <StatusRow
      icon={<Sparkle size={16} />}
      label="AI polish"
      value={!loaded || checking ? "…" : on ? "On" : "Off"}
      on={on}
      title="AI polish settings"
      onOpen={onOpen}
    />
  );
}

export function SidebarStatusFooter({ updateStatus, onChange }: { updateStatus: AutoUpdateStatus; onChange: (tab: TabKey, settingId?: string) => void }): ReactNode {
  const openAppleSyncSettings = () => onChange("settings", "settings.sync-with-apple-devices");
  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-col gap-[2px]">
        <PlanChip onOpen={() => onChange("about")} />
        <PolishChip onOpen={() => onChange("polish")} />
        {isMac() ? <IPhoneSyncChip onOpen={openAppleSyncSettings} /> : null}
      </div>
      <SidebarUpdateStatus status={updateStatus} />
    </div>
  );
}
