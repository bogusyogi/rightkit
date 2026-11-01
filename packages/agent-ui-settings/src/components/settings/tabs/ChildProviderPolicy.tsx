/**
 * Per-session child provider policy editor.
 *
 * The `CliDelegationPolicy` shape is the model the resolver in
 * `engine/crates/driver_host/src/cli_runtime/policy.rs` consumes to
 * decide whether a root may spawn a child, whether a child may spawn
 * a child, and whether a model-emitted `providerHint` is honored.
 * This component is the desktop surface — it must mirror the Rust
 * data model exactly so round-tripping through the daemon keeps the
 * cap of allowed children, the depth limit, and the default-child
 * provider in lock-step.
 *
 * The policy is per-session rather than global so the user can opt
 * each session into a different mix of providers. Kiro is intentionally
 * absent from the toggle list.
 */
import { useMemo } from "react";
import { Checkbox, Select, Slider } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import {
  CLI_PROVIDER_IDS,
  getProviderDisplay,
  isLaunchableProvider,
  type CliProviderId,
} from "@rightkit/agent-ui-core/lib/cliProviders";
import type { CliDelegationPolicy } from "@rightkit/agent-ui-core/lib/types/cliProviders";
import { hostAppName } from "../../../appName.js";

export interface ChildProviderPolicyProps {
  policy: CliDelegationPolicy;
  onChange: (policy: CliDelegationPolicy) => void;
  /** Optional hint to render above the controls when the policy is
   *  inconsistent (e.g. default child is no longer allowed). */
  warning?: string | null;
}

const MAX_DEPTH_MIN = 1;
const MAX_DEPTH_MAX = 4;
const DEFAULT_MAX_DEPTH = 2;

export default function ChildProviderPolicy({
  policy,
  onChange,
  warning,
}: ChildProviderPolicyProps) {
  const allowed = useMemo(
    () => new Set<CliProviderId>(policy.allowedChildProviders),
    [policy.allowedChildProviders],
  );

  const toggleAllowed = (id: CliProviderId) => {
    if (!isLaunchableProvider(id)) return;
    const next = new Set(allowed);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    let defaultChild = policy.defaultChildProvider;
    if (defaultChild && !next.has(defaultChild)) defaultChild = null;
    onChange({
      ...policy,
      allowedChildProviders: Array.from(next),
      defaultChildProvider: defaultChild,
    });
  };

  const setDefaultChild = (raw: string) => {
    if (raw === "") {
      onChange({ ...policy, defaultChildProvider: null });
      return;
    }
    if (isLaunchableProvider(raw as CliProviderId) && allowed.has(raw as CliProviderId)) {
      onChange({ ...policy, defaultChildProvider: raw as CliProviderId });
    }
  };

  const setMaxDepth = (parsed: number) => {
    const clamped = Math.max(MAX_DEPTH_MIN, Math.min(MAX_DEPTH_MAX, Math.round(parsed)));
    onChange({ ...policy, maxDepth: clamped });
  };

  const setPermitHints = (allowed: boolean) => {
    onChange({ ...policy, permitModelProviderHints: allowed });
  };

  const reset = () => {
    onChange({
      allowedChildProviders: [],
      defaultChildProvider: null,
      permitModelProviderHints: true,
      maxDepth: DEFAULT_MAX_DEPTH,
      maxParallelChildren: 4,
      maxParallelPerProvider: {},
    });
  };

  return (
    <section className="flex flex-col gap-4">
      <header className="flex flex-col gap-1">
        <h3 className="text-[16px] font-bold text-text">Child provider policy</h3>
        <p className="text-[12px] text-text-3">
          Per-session rules that decide which authenticated CLIs a root or child agent may delegate
          to. {hostAppName()} stays the authority for tools, approvals, and effects.
        </p>
      </header>

      {warning ? (
        <p className="rounded-md border border-warning/40 bg-warning/10 p-2 text-[12px] text-warning">
          {warning}
        </p>
      ) : null}

      <div className="flex flex-col gap-2">
        <span className="text-[12px] font-semibold text-text-2">Max delegation depth</span>
        <div className="flex items-center gap-3">
          <Slider
            label="Max delegation depth"
            min={MAX_DEPTH_MIN}
            max={MAX_DEPTH_MAX}
            step={1}
            value={policy.maxDepth}
            onChange={setMaxDepth}
            format={(depth) => `depth ${depth}`}
            className="w-full max-w-sm"
          />
          <span
            className="font-mono text-[12px] tabular-nums text-text-2"
            data-testid="child-policy-max-depth-value"
          >
            {policy.maxDepth}
          </span>
          <span className="text-[11px] text-text-3">
            (
            {policy.maxDepth === 1
              ? "root only"
              : `root + child${policy.maxDepth > 2 ? " + deeper" : ""}`}
            )
          </span>
        </div>
        <p className="text-[11px] text-text-3">
          Depth 1 forbids any child from spawning another child. The default of 2 permits one
          further level of delegation.
        </p>
      </div>

      <fieldset className="flex flex-col gap-2">
        <legend className="text-[12px] font-semibold text-text-2">Allowed child providers</legend>
        <p className="text-[11px] text-text-3">
          Toggle the providers this session may delegate to. Kiro is not available.
        </p>
        <ul className="grid gap-1.5 sm:grid-cols-2">
          {CLI_PROVIDER_IDS.map((id) => {
            const display = getProviderDisplay(id);
            const checked = allowed.has(id);
            return (
              <li key={id}>
                <Checkbox
                  checked={checked}
                  onChange={() => toggleAllowed(id)}
                  className="rounded-md border border-line bg-panel px-2.5 py-1.5 hover:border-accent"
                  label={
                    <span className="flex-1 truncate">
                      <strong className="text-text">{display.displayName}</strong>
                      <span className="ml-2 text-[11px] text-text-3">{display.shortLabel}</span>
                    </span>
                  }
                />
              </li>
            );
          })}
        </ul>
      </fieldset>

      <div className="flex flex-col gap-2">
        <span className="text-[12px] font-semibold text-text-2">Default child provider</span>
        <Select
          label="Default child provider"
          value={policy.defaultChildProvider ?? ""}
          onChange={setDefaultChild}
          className="w-full max-w-sm"
          options={[
            { value: "", label: `No default — fall back to ${hostAppName()}-native` },
            ...CLI_PROVIDER_IDS.filter((id) => allowed.has(id)).map((id) => ({
              value: id as string,
              label: getProviderDisplay(id).displayName,
            })),
          ]}
        />
        <p className="text-[11px] text-text-3">
          When the model does not name a child, the root session hands the work to this provider.
        </p>
      </div>

      <div className="flex flex-col gap-2">
        <Checkbox
          checked={policy.permitModelProviderHints}
          onChange={setPermitHints}
          label={
            <span>
              Permit model-emitted provider hints
              <span className="ml-2 text-[11px] text-text-3">
                Honors <code>providerHint</code> calls only when the hinted provider is in
                <code> allowed_child_providers</code>.
              </span>
            </span>
          }
        />
      </div>

      <div className="flex items-center justify-end gap-2">
        <button
          type="button"
          onClick={reset}
          className="cr-control-pill inline-flex items-center gap-1.5 rounded-md border border-line bg-panel-2 px-3 py-1.5 text-[12px] font-semibold text-text-2 hover:border-danger/60 hover:text-danger"
        >
          <Icon name="refresh" size={11} />
          Reset to defaults
        </button>
      </div>
    </section>
  );
}
