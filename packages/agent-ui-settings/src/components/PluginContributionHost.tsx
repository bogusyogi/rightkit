import { useState, type ReactNode } from "react";
import type {
  PluginPanelProps,
  PluginPanelComponent,
} from "@rightkit/agent-ui-core/lib/pluginContributionTypes";
import type {
  PluginContributionProjection,
  ProjectedPanel,
} from "@rightkit/agent-ui-core/lib/pluginContributionProjection";

export interface PluginContributionHostProps {
  projection: PluginContributionProjection;
  onCommand?: (command: string) => void | Promise<void>;
  onPanelSelect?: (panel: ProjectedPanel) => void;
  onSessionAction?: (actionId: string) => void | Promise<void>;
  className?: string;
  /** Optional fallback for a panel whose activated component is not available. */
  panelFallback?: (panel: ProjectedPanel) => ReactNode;
}

function PanelBody({
  panel,
  fallback,
}: {
  panel: ProjectedPanel;
  fallback?: (panel: ProjectedPanel) => ReactNode;
}) {
  const Component = panel.component as PluginPanelComponent | undefined;
  if (Component) {
    const props: PluginPanelProps = { pluginId: panel.pluginId, panelId: panel.id };
    return <Component {...props} />;
  }
  return fallback?.(panel) ?? (
    <p className="text-[12px] text-text-3">This panel is not active.</p>
  );
}

/**
 * Renders Hermes-style plugin contributions without owning navigation or
 * daemon state. Commands/actions are inert until the orchestrator supplies a
 * callback; panel components receive only the donor SDK's identity props.
 */
export default function PluginContributionHost({
  projection,
  onCommand,
  onPanelSelect,
  onSessionAction,
  className = "",
  panelFallback,
}: PluginContributionHostProps) {
  const [selectedPanelId, setSelectedPanelId] = useState<string | null>(null);
  const selectedPanel = projection.panels.find((panel) => panel.id === selectedPanelId);

  return (
    <section
      className={`flex min-w-0 flex-col gap-4 ${className}`}
      data-plugin-id={projection.pluginId}
      data-testid="plugin-contribution-host"
    >
      <header className="flex min-w-0 flex-col gap-1">
        <h2 className="truncate text-[14px] font-semibold text-text">
          {projection.pluginName}
        </h2>
        <p className="truncate text-[12px] text-text-3">
          v{projection.version} · {projection.description}
        </p>
      </header>

      {projection.panels.length > 0 ? (
        <section aria-label="Plugin panels" className="flex flex-col gap-2">
          <h3 className="text-[11px] font-semibold uppercase tracking-wide text-text-3">Panels</h3>
          <div className="flex flex-wrap gap-2">
            {projection.panels.map((panel) => (
              <button
                key={panel.id}
                type="button"
                className="rounded-lg border border-line bg-panel px-3 py-2 text-left text-[12px] text-text-2 hover:bg-panel-2 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
                aria-pressed={selectedPanelId === panel.id}
                data-plugin-panel={panel.id}
                onClick={() => {
                  setSelectedPanelId(panel.id);
                  onPanelSelect?.(panel);
                }}
              >
                {panel.name}
                <span className="ml-2 text-[10px] text-text-3">{panel.side}</span>
              </button>
            ))}
          </div>
          {selectedPanel ? (
            <div className="min-h-0 rounded-lg border border-line bg-panel p-3" data-plugin-panel-view={selectedPanel.id}>
              <PanelBody panel={selectedPanel} fallback={panelFallback} />
            </div>
          ) : null}
        </section>
      ) : null}

      {projection.commands.length > 0 ? (
        <section aria-label="Plugin commands" className="flex flex-col gap-2">
          <h3 className="text-[11px] font-semibold uppercase tracking-wide text-text-3">Commands</h3>
          <div className="flex flex-col gap-1">
            {projection.commands.map((command) => (
              <button
                key={command.command}
                type="button"
                className="flex items-center justify-between gap-3 rounded-lg px-3 py-2 text-left text-[12px] text-text-2 hover:bg-panel focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
                data-plugin-command={command.command}
                onClick={() => void onCommand?.(command.command)}
              >
                <span>{command.title}</span>
                {command.keybinding ? <kbd className="text-[10px] text-text-3">{command.keybinding}</kbd> : null}
              </button>
            ))}
          </div>
        </section>
      ) : null}

      {projection.sessionActions.length > 0 ? (
        <section aria-label="Plugin session actions" className="flex flex-wrap gap-2">
          {projection.sessionActions.map((action) => (
            <button
              key={action.id}
              type="button"
              className="rounded-lg border border-line px-3 py-2 text-[12px] text-text-2 hover:bg-panel focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
              data-plugin-session-action={action.id}
              title={action.name}
              onClick={() => void onSessionAction?.(action.id)}
            >
              {action.name}
              {action.badge?.text || action.badge?.count !== undefined ? (
                <span className="ml-2 rounded bg-panel-2 px-1.5 py-0.5 text-[10px] text-text-3">
                  {action.badge.text ?? action.badge.count}
                </span>
              ) : null}
            </button>
          ))}
        </section>
      ) : null}

      {projection.statusBarItems.some((item) => item.visible) ? (
        <section aria-label="Plugin status" className="flex flex-wrap gap-2 border-t border-line pt-2">
          {projection.statusBarItems.filter((item) => item.visible).map((item) => (
            <button
              key={item.id}
              type="button"
              className="text-[11px] text-text-3 hover:text-text focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
              data-plugin-status-item={item.id}
              title={item.tooltip}
              onClick={() => item.command && void onCommand?.(item.command)}
            >
              {item.text}
            </button>
          ))}
        </section>
      ) : null}

      {Object.keys(projection.settings).length > 0 ? (
        <section aria-label="Plugin settings" className="flex flex-col gap-2 border-t border-line pt-2">
          <h3 className="text-[11px] font-semibold uppercase tracking-wide text-text-3">Settings</h3>
          <dl className="flex flex-col gap-2">
            {Object.entries(projection.settings).map(([key, setting]) => (
              <div key={key} className="flex flex-col gap-0.5">
                <dt className="text-[12px] text-text">{setting.title}</dt>
                <dd className="text-[11px] text-text-3">{setting.description ?? `${key} · ${setting.type}`}</dd>
              </div>
            ))}
          </dl>
        </section>
      ) : null}

      {projection.permissions.length > 0 || projection.unsupportedPermissions.length > 0 ? (
        <section aria-label="Plugin permissions" className="flex flex-col gap-2 border-t border-line pt-2">
          <h3 className="text-[11px] font-semibold uppercase tracking-wide text-text-3">Permissions</h3>
          {projection.permissions.length > 0 ? (
            <ul className="flex flex-wrap gap-x-3 gap-y-1 text-[11px] text-text-3">
              {projection.permissions.map((permission) => <li key={permission}>{permission}</li>)}
            </ul>
          ) : null}
          {projection.unsupportedPermissions.length > 0 ? (
            <p role="status" className="text-[11px] text-warning">
              Unsupported permissions were not granted: {projection.unsupportedPermissions.join(", ")}
            </p>
          ) : null}
        </section>
      ) : null}
    </section>
  );
}
