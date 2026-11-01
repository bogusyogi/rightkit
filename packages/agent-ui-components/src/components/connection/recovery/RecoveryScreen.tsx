import { useCallback, useState } from "react";
import type { FormEvent } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import { isTauriHost, restartDaemon } from "@rightkit/agent-ui-core/lib/daemon";
import {
  formatRecoveryHost,
  getRecoveryConfig,
  resolveRecoveryNextStep,
  type RecoveryVariant,
} from "@rightkit/agent-ui-core/lib/connection/recovery/recoveryPolicy";
import { hostAppName } from "../../../appName.js";

export type { RecoveryVariant } from "@rightkit/agent-ui-core/lib/connection/recovery/recoveryPolicy";

export interface RecoveryScreenProps {
  variant: RecoveryVariant;
  hostUrl?: string;
  hostLabel?: string;
  onRetry?: () => void;
  onChooseRemote?: () => void;
  showRemoteForm?: boolean;
  onCloseRemoteForm?: () => void;
  onSwitchToLocalFromRemote?: () => void;
  onEnterLocalSetup?: () => void;
  onSubmitRemote?: (url: string, label: string) => void;
  isRetrying?: boolean;
  localAvailable?: boolean;
}

function RecoveryIcon({ icon }: { icon: "local" | "remote" }) {
  return <Icon name={icon === "local" ? "folder" : "server"} size={28} />;
}

export function RecoveryScreen({
  variant,
  hostUrl,
  hostLabel,
  onRetry,
  onChooseRemote,
  showRemoteForm = false,
  onCloseRemoteForm,
  onSwitchToLocalFromRemote,
  onEnterLocalSetup,
  onSubmitRemote,
  isRetrying = false,
  localAvailable = true,
}: RecoveryScreenProps) {
  const config = getRecoveryConfig(variant, hostLabel, hostUrl);
  const safeHost = formatRecoveryHost(hostLabel, hostUrl);
  const [remoteUrl, setRemoteUrl] = useState("");
  const [remoteLabel, setRemoteLabel] = useState("");

  const restartLocal = useCallback(async () => {
    if (isTauriHost()) {
      await restartDaemon();
      return;
    }
    onRetry?.();
  }, [onRetry]);

  const handleRetry = useCallback(async () => {
    await restartLocal();
  }, [restartLocal]);

  const handleLocal = useCallback(async () => {
    const step = resolveRecoveryNextStep(variant, "use-local");
    if (step.kind === "local-setup") {
      onEnterLocalSetup?.();
      return;
    }
    if (onSwitchToLocalFromRemote) {
      onSwitchToLocalFromRemote();
      return;
    }
    await restartLocal();
  }, [onEnterLocalSetup, onSwitchToLocalFromRemote, restartLocal, variant]);

  const handleRemote = useCallback(() => {
    const step = resolveRecoveryNextStep(variant, "use-remote");
    if (step.kind === "remote-form") onChooseRemote?.();
  }, [onChooseRemote, variant]);

  const submitRemote = useCallback(
    (event: FormEvent<HTMLFormElement>) => {
      event.preventDefault();
      if (remoteUrl.trim()) onSubmitRemote?.(remoteUrl.trim(), remoteLabel.trim());
    },
    [onSubmitRemote, remoteLabel, remoteUrl],
  );

  if (showRemoteForm) {
    return (
      <div className="flex min-h-0 flex-1 flex-col items-center justify-center p-8">
        <form className="grid w-full max-w-md gap-4" onSubmit={submitRemote} aria-label="Remote connection">
          <button type="button" className="w-fit border-0 bg-transparent p-0 text-[12px] text-text-3 hover:text-text" onClick={onCloseRemoteForm ?? onChooseRemote}>
            ← Back
          </button>
          <div className="text-center">
            <h1 className="text-xl font-semibold text-text">Connect to a remote daemon</h1>
            <p className="mt-2 text-[12px] text-text-3">Enter a {hostAppName()} daemon address to continue.</p>
          </div>
          <label className="grid gap-1 text-[12px] text-text-2">
            Server address
            <input className="h-9 rounded-lg border border-line bg-panel px-3 text-[13px] text-text outline-none focus:border-accent" type="url" value={remoteUrl} onChange={(event) => setRemoteUrl(event.target.value)}
              // biome-ignore lint/a11y/noAutofocus: recovery dialog must place keyboard focus on its primary repair field.
              autoFocus required />
          </label>
          <label className="grid gap-1 text-[12px] text-text-2">
            Name (optional)
            <input className="h-9 rounded-lg border border-line bg-panel px-3 text-[13px] text-text outline-none focus:border-accent" type="text" value={remoteLabel} onChange={(event) => setRemoteLabel(event.target.value)} />
          </label>
          <div className="flex gap-2">
            {localAvailable && <button type="button" className="h-9 flex-1 rounded-lg border border-line bg-transparent px-3 text-[12px] text-text-2 hover:bg-panel-2" onClick={onSwitchToLocalFromRemote ?? onEnterLocalSetup}>Use local</button>}
            <button type="submit" className="h-9 flex-1 rounded-lg bg-accent px-3 text-[12px] font-semibold text-on-accent disabled:opacity-50" disabled={!remoteUrl.trim()}>Connect</button>
          </div>
        </form>
      </div>
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col items-center justify-center p-8" data-testid="connection-recovery">
      <div className="grid w-full max-w-md gap-6">
        <div className="grid justify-items-center gap-3 text-center">
          <div className="grid h-14 w-14 place-items-center rounded-full border border-warning/40 bg-warning/10 text-warning" aria-hidden="true">
            <RecoveryIcon icon={config.iconKey} />
          </div>
          <h1 className="text-xl font-semibold text-text">{config.title}</h1>
          <p className="max-w-sm text-[13px] leading-[1.5] text-text-3">{config.description}</p>
        </div>
        {safeHost && config.iconKey === "remote" && (
          <div className="rounded-lg border border-line bg-panel px-3 py-2">
            <div className="text-[11px] text-text-3">Server address</div>
            <div className="truncate font-mono text-[12px] text-text" title={safeHost}>{safeHost}</div>
          </div>
        )}
        <div className="grid gap-2">
          {config.showRetry && onRetry && (
            <button type="button" onClick={() => void handleRetry()} disabled={isRetrying} className="inline-flex h-9 items-center justify-center gap-2 rounded-lg bg-accent px-3 text-[12px] font-semibold text-on-accent disabled:opacity-50">
              <Icon name="refresh" size={15} />{isRetrying ? "Retrying…" : config.retryLabel}
            </button>
          )}
          <div className="flex gap-2">
            {config.showUseLocal && localAvailable && (
              <button type="button" onClick={() => void handleLocal()} disabled={isRetrying} className="inline-flex h-9 flex-1 items-center justify-center gap-2 rounded-lg border border-line bg-transparent px-3 text-[12px] text-text-2 hover:bg-panel-2 disabled:opacity-50">
                <Icon name="folder" size={15} />{config.useLocalLabel}
              </button>
            )}
            {config.showUseRemote && (
              <button type="button" onClick={handleRemote} disabled={isRetrying} className="inline-flex h-9 flex-1 items-center justify-center gap-2 rounded-lg border border-line bg-transparent px-3 text-[12px] text-text-2 hover:bg-panel-2 disabled:opacity-50">
                <Icon name="server" size={15} />{config.useRemoteLabel}
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

export default RecoveryScreen;
