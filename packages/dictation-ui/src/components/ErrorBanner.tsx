import { hostAppName } from "../appName.js";
interface Props {
  message: string;
  onDismiss?: () => void;
}

function humanize(message: string): string {
  const lower = message.toLowerCase();
  // Typed privacy denial: the code is for hosts; the sentence names the grant.
  const permission = /^(?:Error: )?E_PERMISSION: /.exec(message);
  if (permission) return message.slice(permission[0].length);
  if (lower.includes("wake word is not available")) {
    return "Wake word isn't available yet.";
  }
  if (lower.includes("no last delivery") || lower.includes("no recent fill")) {
    return "No recent transcript is available yet.";
  }
  if (lower.includes("runtime lock poisoned") || lower.includes("settings lock poisoned")) {
    return `${hostAppName()} hit an internal state error. Restart the app and try again.`;
  }
  if (lower.includes("clipboard")) {
    return `${hostAppName()} could not access the clipboard. Try again in a moment.`;
  }
  // Licensing rejections carry the explanation ("... is a Pro feature.") —
  // swallowing them into the generic line reads as a bug, not a paywall.
  // Apple Sync fan-out failures arrive prefixed with the channel that failed
  // ("Transcripts: …", "Pro resolution: …"); the prefix is the diagnosis.
  if (lower.startsWith("transcripts:") || lower.startsWith("pro resolution:") || lower.startsWith("apple keychain:")) {
    return message;
  }
  if (lower.includes("pro feature")) {
    return message;
  }
  // HR-ACCOUNT-P8: account deletion blocked by an active subscription (409
  // from `DELETE /v1/shop/account`) — this is actionable, specific guidance,
  // not a generic failure, so it must reach the user verbatim.
  if (lower.includes("active subscription")) {
    return message;
  }
  if (
    lower.includes("microphone") ||
    lower.includes("permission") ||
    lower.includes("accessibility") ||
    lower.includes("keychain") ||
    lower.includes("credential")
  ) {
    return message;
  }
  if (
    lower.includes("ffmpeg") ||
    lower.includes("could not read this file") ||
    lower.includes("could not run") ||
    lower.includes("file not found") ||
    lower.includes("unexpected format")
  ) {
    return message;
  }
  return `That did not work. Try again, and restart ${hostAppName()} if it keeps happening.`;
}

export default function ErrorBanner({ message, onDismiss }: Props) {
  if (!message) return null;
  return (
    <div
      className="hr-stagger mb-[var(--space-3)] flex min-w-0 items-start justify-between gap-[var(--space-3)] rounded-[var(--radius-sm)] border border-[var(--danger)] bg-[rgba(192,74,61,0.12)] px-[var(--space-3)] py-[var(--space-2)] text-[length:var(--size-sm)] text-[var(--danger)]"
      role="alert"
    >
      <span>{humanize(message)}</span>
      {onDismiss && (
        <button
          type="button"
          className="min-h-7 min-w-7 cursor-pointer rounded-[var(--radius-sm)] border-0 bg-transparent p-px [font:inherit] leading-none text-[var(--danger)] opacity-75 hover:opacity-100 focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]"
          onClick={onDismiss}
          aria-label="Dismiss error"
        >
          x
        </button>
      )}
    </div>
  );
}
