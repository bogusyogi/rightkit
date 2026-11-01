import { MagnifyingGlass } from "@phosphor-icons/react";
import { usePlatform } from "../hooks/usePlatform";
import { useSettings } from "../hooks/useSettings";
import { commandsSearch, useField } from "../lib/fieldStore";
import type { SettingsBlob } from "../types/settings";
import SegmentedControl from "./settings/SegmentedControl";

const screenshotDestinations = [
  { value: "clipboard", label: "Clipboard", title: "Copy screenshots to the clipboard" },
  { value: "disk", label: "Save", title: "Copy screenshots and save them to Pictures/Screenshots" },
] as const;
type ScreenshotDestination = (typeof screenshotDestinations)[number]["value"];

export default function CommandControls() {
  const platform = usePlatform();
  const { loaded, get, patch } = useSettings();
  const [query, setQuery] = useField(commandsSearch);
  const rawValue = get("screenshot_destination", "clipboard") as NonNullable<SettingsBlob["screenshot_destination"]>;
  const value: ScreenshotDestination = rawValue === "both" ? "disk" : rawValue;
  const searchLabel = platform === "macos" ? "Search commands and Shortcuts" : "Search commands";
  if (platform === "ios") return null;

  return (
    <div className="flex min-w-0 flex-wrap items-center gap-3">
      <span data-setting-id="commands.send-screenshots-to" className="flex items-center gap-2 text-[length:var(--size-xs)] text-[var(--muted)] [--on-accent:var(--accent-contrast)]">
        Screenshots
        <SegmentedControl
          label="Screenshots"
          value={value}
          options={screenshotDestinations}
          disabled={!loaded}
          onChange={(next) => void patch("screenshot_destination", next)}
        />
      </span>
      <label className="relative flex h-8 w-[230px] max-w-full items-center rounded-[var(--radius-sm)] border border-[var(--line)] bg-[var(--surface-2)] focus-within:[box-shadow:var(--focus-ring)]">
        <MagnifyingGlass aria-hidden="true" className="pointer-events-none absolute left-2 h-[14px] w-[14px] text-[var(--muted)]" />
        <input
          type="search"
          aria-label={searchLabel}
          placeholder={`${searchLabel}…`}
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          className="h-full w-full min-w-0 border-0 bg-transparent pl-7 pr-2 [font:inherit] text-[length:var(--size-xs)] text-[var(--paper)] placeholder:text-[var(--muted-2)] focus:outline-none"
        />
      </label>
    </div>
  );
}
