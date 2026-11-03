import { useEffect, useRef } from 'react';

export interface BootProbationReporterProps {
  /** Report how far this boot got (for the native `boot_stage` owner). Called once with `mountedStage` when the UI is up. */
  stage(name: string): Promise<unknown> | unknown;
  /** Report a healthy boot (for the native `boot_ok` owner). Called once, after `stage`, while `healthy` is not false. */
  ok(): Promise<unknown> | unknown;
  /** Hold the healthy report until the app has proven itself (data loaded, first view drawn). Default `true`. */
  healthy?: boolean;
  /** The stage name reported on mount. Default `renderer-mounted`; 1 to 64 letters, digits, `_`, `-` or `.`. */
  mountedStage?: string;
  /** Called with the failure of `stage` or `ok`. Default: ignored, because a probation report must never break the UI. */
  onError?(error: unknown): void;
}

/**
 * Renderer half of boot probation (#448): once the UI is up it reports the boot stage, then, when the app is
 * healthy, acknowledges the boot. It renders nothing. Pair it with the native probation owner:
 * `<BootProbationReporter stage={probation.reportStage} ok={probation.acknowledgeHealthy} />`, where `probation`
 * is `createNativeBootProbation(transport)` from `@rightkit/app-update/native`, or pass any two callbacks that
 * call the app's own `boot_stage` and `boot_ok` commands. Each report runs once per mount.
 */
export function BootProbationReporter({ stage, ok, healthy = true, mountedStage = 'renderer-mounted', onError }: BootProbationReporterProps): null {
  const staged = useRef<Promise<void> | null>(null);
  const acknowledged = useRef(false);
  const handlers = useRef({ stage, ok, onError });
  handlers.current = { stage, ok, onError };

  const report = (error: unknown): void => {
    try { handlers.current.onError?.(error); } catch { /* A caller error never breaks the UI. */ }
  };

  useEffect(() => {
    if (staged.current !== null) return;
    staged.current = (async () => {
      try { await handlers.current.stage(mountedStage); } catch (error) { report(error); }
    })();
  }, [mountedStage]);

  useEffect(() => {
    if (!healthy || acknowledged.current) return;
    acknowledged.current = true;
    void (async () => {
      await staged.current;
      try { await handlers.current.ok(); } catch (error) { acknowledged.current = false; report(error); }
    })();
  }, [healthy]);

  return null;
}
