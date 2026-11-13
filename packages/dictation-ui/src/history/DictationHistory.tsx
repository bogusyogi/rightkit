import { type ComponentType, type ReactNode, useMemo } from "react";
import {
  friendlyAppName,
  type LibraryButtonProps,
  type LibraryComponents,
  LibraryScreen,
  type LibraryOriginFilter,
} from "@rightkit/library-ui";
import ErrorBanner from "../components/ErrorBanner.js";
import { dictationClient, hostAppName, hostComponents } from "../host.js";
import { cx, ui } from "../lib/ui.js";
import { DELIVERY_VARIANTS, deliveryGroups, deliveryLabels } from "./deliveryRows.js";
import { useHistorySourceSelection } from "./useHistorySourceSelection.js";

// The package's button classes for the list's buttons: the plain `ui.button` for the default and primary
// variants, ghost and danger as `ui` defines them, compact for the list's `sm`.
function HistoryButton({ variant, size, disabled, onClick, buttonRef, children }: LibraryButtonProps) {
  return (
    <button
      ref={buttonRef}
      type="button"
      className={cx(ui.button, variant === "ghost" && ui.buttonGhost, variant === "danger" && ui.buttonDanger, size === "sm" && ui.buttonCompact)}
      disabled={disabled}
      onClick={onClick}
    >
      {children}
    </button>
  );
}

function HistoryErrorNotice({ message, onDismiss }: { message: string; onDismiss: () => void }) {
  return <ErrorBanner message={message} onDismiss={onDismiss} />;
}

// The app filter matches the raw key the store knows an app by and shows its friendly name, sorted by the key.
const ORIGIN_FILTER: LibraryOriginFilter = { sortBy: "value", optionLabel: (value) => friendlyAppName(value) };

/**
 * The dictation History page: `@rightkit/library-ui`'s list over the host's history source
 * (`DictationClient.history`, rows mapped with `deliveryRow`), with the Dictations/Cancelled
 * variants, the device groups and the transcript wording. `search` comes from wherever the host
 * renders the field. The page frame is the host's `components.PageShell` (called with `page: "history"`
 * and the toolbar as `header`) when it has one, the list's own otherwise.
 */
export function DictationHistory({ search = "" }: { search?: string }) {
  const groups = useMemo(() => deliveryGroups(), []);
  const labels = useMemo(() => deliveryLabels(), []);
  // Windows has no Apple sync: every row is this PC's and the source choice is fixed.
  const { groupSelection } = useHistorySourceSelection();
  const HostShell = hostComponents().PageShell;
  const components = useMemo<LibraryComponents>(
    () => ({
      PageShell: HostShell
        ? ({ header, children }: { maxWidth: number; header: ReactNode; children: ReactNode }) => (
            <HostShell page="history" header={header}>
              {children}
            </HostShell>
          )
        : undefined,
      ErrorNotice: HistoryErrorNotice,
      Button: HistoryButton as ComponentType<LibraryButtonProps>,
    }),
    [HostShell],
  );
  const classNames = useMemo(() => ({ input: ui.input }), []);
  return (
    <LibraryScreen
      source={dictationClient().history}
      search={search}
      variants={DELIVERY_VARIANTS}
      groups={groups}
      groupSelection={groupSelection}
      labels={labels}
      exportName={`${hostAppName().toLowerCase()}-history`}
      components={components}
      classNames={classNames}
      originFilter={ORIGIN_FILTER}
    />
  );
}
