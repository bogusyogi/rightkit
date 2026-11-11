import { cx } from "@rightkit/react-shell/react";
import { useCallback, useState } from "react";
import type { JobInfo } from "../contract.js";
import { invoke } from "../lib/ipc/index.js";
import { statusText } from "../lib/jobDisplay.js";
import { buttonClass, errorTextClass, panelClass, primaryButtonClass, subtlePanelClass } from "../../ui.js";
import { Clipboard, LogIn, PanelRightOpen, RotateCcw } from "lucide-react";

type JobStatePreviewProps = {
  job: JobInfo;
  active: boolean;
  progress: number;
  onRetryAfterSignin: (job: JobInfo, options?: { skipExport?: boolean }) => void;
  onError: (message: string) => void;
};

export function JobStatePreview({
  job,
  active,
  progress,
  onRetryAfterSignin,
  onError,
}: JobStatePreviewProps) {
  const [cookieText, setCookieText] = useState("");
  const [importing, setImporting] = useState(false);
  const [browserChoice, setBrowserChoice] = useState("edge");
  const [browserImporting, setBrowserImporting] = useState(false);

  const openSignin = useCallback(() => {
    if (!job.needs_auth) return;
    invoke("sign_in", { platform: job.needs_auth, url: job.input }).catch((e) =>
      onError(String(e)),
    );
  }, [job.input, job.needs_auth, onError]);

  const importCookiesAndRetry = useCallback(async () => {
    if (!job.needs_auth) return;
    setImporting(true);
    try {
      await invoke("import_auth_cookies", {
        platform: job.needs_auth,
        cookiesText: cookieText,
      });
      setCookieText("");
      await onRetryAfterSignin(job, { skipExport: true });
    } catch (e) {
      onError(String(e));
    } finally {
      setImporting(false);
    }
  }, [cookieText, job, onError, onRetryAfterSignin]);

  const importBrowserCookiesAndRetry = useCallback(async () => {
    if (!job.needs_auth) return;
    setBrowserImporting(true);
    try {
      await invoke("import_auth_from_browser", {
        platform: job.needs_auth,
        browser: browserChoice,
      });
      await onRetryAfterSignin(job, { skipExport: true });
    } catch (e) {
      onError(String(e));
    } finally {
      setBrowserImporting(false);
    }
  }, [browserChoice, job, onError, onRetryAfterSignin]);

  const openSelectedBrowser = useCallback(() => {
    if (!job.needs_auth) return;
    invoke("open_auth_url_in_browser", {
      platform: job.needs_auth,
      browser: browserChoice,
      url: job.input,
    }).catch((e) => onError(String(e)));
  }, [browserChoice, job.input, job.needs_auth, onError]);

  return (
    <div className="grid gap-4 p-5">
      <div className={cx(subtlePanelClass, "p-4")}>
        <div className="flex items-center justify-between gap-3">
          <span className="font-[family-name:var(--mono)] text-xs text-[var(--muted)]">
            Selected source
          </span>
          <span className="font-[family-name:var(--mono)] text-xs text-[var(--muted)]">
            {statusText(job)}
          </span>
        </div>
        {active && (
          <div className="mt-4">
            <div
              className="progress-shine h-2 overflow-hidden rounded-full bg-[color-mix(in_srgb,var(--line)_58%,transparent)]"
              role="progressbar"
              aria-label={`${statusText(job)} progress`}
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={progress}
            >
              <span
                className="block h-full rounded-full bg-[var(--accent)] transition-[width] duration-300"
                style={{ width: `${progress}%` }}
              />
            </div>
          </div>
        )}
        {job.error && !job.needs_auth && (
          <div
            className={cx(
              "mt-4 rounded-[8px] border border-[color-mix(in_srgb,var(--warn)_45%,var(--line))] bg-[color-mix(in_srgb,var(--warn)_8%,var(--panel-2))] p-3 font-[family-name:var(--mono)] text-xs",
              errorTextClass,
            )}
          >
            {job.error.replace(/\byt-dlp\b/gi, "media downloader")}
          </div>
        )}
      </div>

      {job.needs_auth && (
        <div className={cx(panelClass, "grid gap-3 p-4")}>
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div>
              <h3 className="m-0 text-base font-semibold text-[var(--paper)]">
                {job.needs_auth} sign-in required
              </h3>
              <p className="m-0 mt-1 text-sm text-[var(--muted)]">
                Sign in with an account that can view this post, then close the browser window — the
                download continues automatically.
              </p>
            </div>
            <button
              type="button"
              className={primaryButtonClass}
              onClick={openSignin}
              title={`Sign in to ${job.needs_auth}`}
              aria-label={`Sign in to ${job.needs_auth}`}
            >
              <LogIn size={15} strokeWidth={1.9} />
              <span>Sign in</span>
            </button>
          </div>
          <details>
            <summary className="cursor-pointer select-none font-[family-name:var(--mono)] text-xs text-[var(--muted)] hover:text-[var(--paper)]">
              Didn’t continue automatically? Manual options
            </summary>
            <div className="mt-3 grid gap-3">
              <button
                type="button"
                className={buttonClass}
                onClick={() => onRetryAfterSignin(job)}
                title="Retry source after sign-in"
                aria-label="Retry source after sign-in"
              >
                <RotateCcw size={15} strokeWidth={1.8} />
                <span>Retry now</span>
              </button>
              <p className="m-0 text-xs text-[var(--muted)]">
                Cookie fallback: paste a request Cookie header from a browser where this exact post
                is visible.
              </p>
              <textarea
                className="min-h-[96px] resize-y rounded-[8px] border border-[var(--line)] bg-[var(--ink)] p-3 font-[family-name:var(--mono)] text-xs leading-5 text-[var(--paper)] outline-none transition-colors placeholder:text-[var(--muted)] focus:border-[var(--accent)] focus:ring-2 focus:ring-[var(--accent)]/25"
                value={cookieText}
                onChange={(e) => setCookieText(e.target.value)}
                placeholder="Cookie: sessionid=...; ds_user_id=..."
                spellCheck={false}
              />
              <div className="flex flex-wrap items-center justify-between gap-2">
                <span className="flex flex-wrap items-center gap-2">
                  <select
                    className="h-10 rounded-[8px] border border-[var(--line)] bg-[var(--ink)] px-3 font-[family-name:var(--mono)] text-xs text-[var(--paper)] outline-none focus:border-[var(--accent)] focus:ring-2 focus:ring-[var(--accent)]/25"
                    value={browserChoice}
                    onChange={(e) => setBrowserChoice(e.target.value)}
                    aria-label="Browser to import Instagram cookies from"
                  >
                    <option value="edge">Edge</option>
                    <option value="chrome">Chrome</option>
                    <option value="firefox">Firefox</option>
                    <option value="brave">Brave</option>
                  </select>
                  <button
                    type="button"
                    className={buttonClass}
                    onClick={openSelectedBrowser}
                    title="Open this post in the selected browser"
                    aria-label="Open this post in the selected browser"
                  >
                    <PanelRightOpen size={15} strokeWidth={1.8} />
                    <span>Open in browser</span>
                  </button>
                  <button
                    type="button"
                    className={buttonClass}
                    onClick={importBrowserCookiesAndRetry}
                    disabled={browserImporting}
                    title="Import Instagram cookies from a local browser and retry"
                    aria-label="Import Instagram cookies from a local browser and retry"
                  >
                    {browserImporting ? (
                      <RotateCcw size={15} strokeWidth={1.8} className="animate-spin" />
                    ) : (
                      <Clipboard size={15} strokeWidth={1.8} />
                    )}
                    <span>{browserImporting ? "Importing" : "Import browser cookies + retry"}</span>
                  </button>
                </span>
                <button
                  type="button"
                  className={buttonClass}
                  onClick={importCookiesAndRetry}
                  disabled={importing || cookieText.trim().length === 0}
                  title="Import pasted cookies and retry"
                  aria-label="Import pasted cookies and retry"
                >
                  {importing ? (
                    <RotateCcw size={15} strokeWidth={1.8} className="animate-spin" />
                  ) : (
                    <Clipboard size={15} strokeWidth={1.8} />
                  )}
                  <span>{importing ? "Importing" : "Import + retry"}</span>
                </button>
              </div>
            </div>
          </details>
        </div>
      )}
    </div>
  );
}
