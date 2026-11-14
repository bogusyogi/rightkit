import {
  ArrowClockwise,
  ArrowSquareOut,
  CircleNotch,
  DownloadSimple,
  type Icon as PhosphorIcon,
} from "@phosphor-icons/react";
import { memo } from "react";
import { cx } from "@rightkit/react-shell/react";
import { hostShell, openPricing } from "../host.js";

/** The update status the sidebar reads: `@rightkit/app-update`'s `AutoUpdateStatus` satisfies it (the shell takes no dependency on that package). */
export type SidebarUpdateState = { kind: string; version?: string; pct?: number; reason?: string };

const actionClass =
  "has-tip inline-grid h-6 w-6 shrink-0 place-items-center rounded-[var(--radius-sm)] border border-transparent bg-transparent text-[var(--muted)] transition-[background-color,border-color,color,transform] duration-150 ease-[var(--ease-out-strong)] hover:border-[color-mix(in_srgb,var(--line),transparent_10%)] hover:bg-[var(--surface-hover)] hover:text-[var(--accent)] active:scale-[0.96] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]";

type UpdateAction = {
  label: string;
  icon: PhosphorIcon;
  run: () => void;
  accent?: boolean;
};

function SidebarUpdateStatus({
  status,
  surface = "sidebar",
}: {
  status: SidebarUpdateState;
  surface?: "sidebar" | "narrow";
}) {
  const action: UpdateAction | null = (() => {
    if (status.kind === "available") {
      if (status.reason === "pro_required") {
        return {
          label: `Upgrade to install v${status.version}`,
          icon: ArrowSquareOut,
          run: () => openPricing(),
        };
      }
      return {
        label: `Download & install v${status.version}`,
        icon: DownloadSimple,
        run: () => void Promise.resolve(hostShell().update.installAndRelaunch()).catch(() => undefined),
        accent: true,
      };
    }
    if (status.kind === "deferred") {
      return {
        label: `Download & install v${status.version}`,
        icon: DownloadSimple,
        run: () => void Promise.resolve(hostShell().update.installAndRelaunch()).catch(() => undefined),
        accent: true,
      };
    }
    if (status.kind === "ready") {
      return {
        label: "Restart",
        icon: ArrowClockwise,
        run: () => void hostShell().update.relaunch(),
        accent: true,
      };
    }
    if (status.kind === "error") {
      return {
        label: "Retry update check",
        icon: ArrowClockwise,
        run: () => void hostShell().update.check(),
      };
    }
    return null;
  })();

  const busy = status.kind === "checking" || status.kind === "downloading";
  const assistiveStatus = (() => {
    if (status.kind === "checking") return "Checking for updates";
    if (status.kind === "downloading") return `Downloading v${status.version}, ${status.pct}%`;
    if (status.kind === "ready") return `Update v${status.version} installed. Restart required.`;
    if (status.kind === "error") return "Update check failed";
    if (status.kind === "available" || status.kind === "deferred") {
      return `Update v${status.version} available`;
    }
    return status.kind === "up_to_date" ? "Up to date" : "Check for updates";
  })();

  return (
    <div
      data-sidebar-update-status
      data-update-status-surface={surface}
      // Quiet single line under the footer chips.
    >
      <div className="flex min-h-6 min-w-0 items-center gap-[var(--space-2)]">
        <span className="truncate text-[length:var(--size-xs)] text-[var(--muted)] [font-family:var(--font-mono)]">
          v{hostShell().appVersion}
        </span>
        {status.kind !== "idle" && status.kind !== "up_to_date" ? (
          <span className="sr-only" aria-live="polite">
            {assistiveStatus}
          </span>
        ) : null}

        {action ? (
          <button
            type="button"
            data-update-action={action.label}
            data-tooltip=""
            data-tip={action.label}
            aria-label={action.label}
            className={cx(
              actionClass,
              "ml-auto",
              surface === "narrow" && "h-11 w-11",
              action.accent && "text-[var(--accent)]",
            )}
            onClick={action.run}
          >
            <action.icon className="h-[14px] w-[14px]" weight="bold" aria-hidden="true" />
          </button>
        ) : busy ? (
          <span
            className={cx(
              "ml-auto inline-grid h-6 w-6 place-items-center text-[var(--muted)]",
              surface === "narrow" && "h-11 w-11",
            )}
            aria-hidden="true"
          >
            <CircleNotch className="h-[14px] w-[14px] animate-spin" weight="bold" />
          </span>
        ) : (
          <button
            type="button"
            className="ml-auto inline-flex min-h-6 cursor-pointer items-center gap-[7px] whitespace-nowrap rounded-[var(--radius-sm)] border-0 bg-transparent p-0 text-[length:var(--size-xs)] text-[var(--muted)] hover:text-[var(--paper)] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]"
            aria-label="Check for updates"
            title={status.kind === "up_to_date" ? "Up to date. Check again" : "Check for updates"}
            onClick={() => void Promise.resolve(hostShell().update.check()).catch(() => undefined)}
          >
            {status.kind === "up_to_date" ? (
              <span className="h-[7px] w-[7px] rounded-full bg-[var(--ok)]" aria-hidden="true" />
            ) : null}
            {status.kind === "up_to_date" ? "Up to date" : "Check for updates"}
          </button>
        )}
      </div>

      {status.kind === "downloading" ? (
        <span
          className="sr-only"
          role="progressbar"
          aria-label="Update download"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={status.pct}
        />
      ) : null}
    </div>
  );
}

export default memo(SidebarUpdateStatus);
