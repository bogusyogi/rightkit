import { useRef, type KeyboardEvent } from "react";
import type { SettingsSection } from "@rightkit/settings-ui";
import "./settings-tabs.css";

/**
 * Chrome-style section tabs across the top of Settings. The shared SettingsPanel
 * keeps rendering the content with its own nav hidden; this strip selects the
 * section through the panel's controlled `activeId`.
 */
export function SettingsTabs({
  sections,
  activeId,
  onSelect,
}: {
  sections: readonly SettingsSection[];
  activeId: string;
  onSelect: (id: string) => void;
}) {
  const listRef = useRef<HTMLDivElement>(null);

  function focusTab(index: number) {
    const tabs = listRef.current?.querySelectorAll<HTMLButtonElement>('[role="tab"]');
    if (!tabs || tabs.length === 0) return;
    const next = tabs[(index + tabs.length) % tabs.length];
    next.focus();
    next.click();
  }

  function onKeyDown(event: KeyboardEvent<HTMLButtonElement>, index: number) {
    switch (event.key) {
      case "ArrowRight":
        event.preventDefault();
        focusTab(index + 1);
        break;
      case "ArrowLeft":
        event.preventDefault();
        focusTab(index - 1);
        break;
      case "Home":
        event.preventDefault();
        focusTab(0);
        break;
      case "End":
        event.preventDefault();
        focusTab(sections.length - 1);
        break;
    }
  }

  return (
    <div ref={listRef} className="hr-settings-tabs" role="tablist" aria-label="Settings sections">
      {sections.map((section, index) => {
        const active = section.id === activeId;
        return (
          <button
            key={section.id}
            type="button"
            role="tab"
            id={`settings-tab-${section.id}`}
            aria-selected={active}
            aria-controls="settings-tabpanel"
            tabIndex={active || (!activeId && index === 0) ? 0 : -1}
            data-section-id={section.id}
            title={section.label}
            className="hr-settings-tab"
            data-active={active ? "true" : undefined}
            onClick={() => onSelect(section.id)}
            onKeyDown={(event) => onKeyDown(event, index)}
          >
            <span className="hr-settings-tab__label">{section.label}</span>
          </button>
        );
      })}
    </div>
  );
}
