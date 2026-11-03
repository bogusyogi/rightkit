// Permission-dialog helpers for QA runs (macOS system dialog agents).

const SYSTEM_DIALOG_OWNERS = ['usernotificationcenter', 'coreservicesuiagent', 'universalaccessauthwarn', 'securityagent', 'tccd'];

export function isSystemDialogOwner(name: string): boolean { return SYSTEM_DIALOG_OWNERS.includes(name.toLowerCase()); }

/**
 * Positive evidence only: the first sampled foreground application name that belongs to a macOS system
 * permission-dialog agent. An empty answer proves nothing, because these agents rarely become frontmost.
 */
export function permissionDialogIn(frontmostNames: readonly string[]): string | undefined { return frontmostNames.find(isSystemDialogOwner); }

export type DialogCheck = { status: 'clear' } | { status: 'dialog'; owner: string } | { status: 'unsupported'; reason: string };

/**
 * TODO: observe live TCC prompts. This package launches no native app and has no window-by-owner
 * enumeration, so no reliable detector exists here. The hook reports `unsupported` instead of guessing;
 * the Rust crate's `Control` is the surface that can grow a real detector.
 */
export function livePermissionDialogCheck(_appPid: number): DialogCheck {
  return { status: 'unsupported', reason: 'no window-by-owner enumeration; permission dialogs are not observable from Node' };
}

/** Fail the scenario when a system permission dialog is known to be showing; a no-op while the check is unsupported. */
export function assertNoPermissionDialog(appPid: number, frontmostNames: readonly string[] = []): void {
  const seen = permissionDialogIn(frontmostNames);
  if (seen) throw new Error(`a system permission dialog (${seen}) appeared for the app under test`);
  const live = livePermissionDialogCheck(appPid);
  if (live.status === 'dialog') throw new Error(`a system permission dialog (${live.owner}) appeared for the app under test`);
}
