/** Safe renderer DTOs for M12-DESIGN v3.1 section 2 + v3.2 (no cached view). */
export type UpdateKind = 'patch' | 'update';
export type UpdateRefusal = 'notLicensed' | 'expired' | 'revoked' | 'tierIneligible' | 'busy' | 'unresolved' | 'disabledNoPublicPatch' | 'offline' | 'none';
export type UpdateState = 'idle' | 'checking' | 'available' | 'downloading' | 'downloaded' | 'installing' | 'failed';
/**
 * Message text for the fixed message codes of the native update host. The keys are the codes the native
 * `UpdateMessage` enum (camelCase) in `crates/rightkit-tauri-shell/src/update.rs` sends; that file lands with PK13-2.
 * A key is added here only with the native enum. Native sends the code, the renderer owns the text, and the text names no licence,
 * key, device or account.
 */
export const UPDATE_MESSAGES = Object.freeze({
  none: '',
  unconfigured: 'Updates are not available in this build.',
  busy: 'Another update operation is in progress. Try again in a moment.',
  unresolved: 'The last update did not finish. Try again to complete it.',
  networkUnavailable: 'Could not reach the update server. Check your connection and try again.',
  revoked: 'This update is not available for this installation.',
  expired: 'This update is no longer available.',
  forbidden: 'This window cannot manage updates.',
  unsupported: 'This feature is not available in this app.',
  updateRefused: 'This update cannot be installed on this installation.',
  updateFailed: 'The update could not be downloaded or installed.',
  verifyFailed: 'The update could not be verified.',
  storeFailed: 'The update could not be saved on this device.',
  unknown: 'Something went wrong.',
});
export type UpdateMessageCode = keyof typeof UPDATE_MESSAGES;
export type UpdateErrorCode = 'unconfigured' | 'busy' | 'unresolved' | 'network' | 'store' | 'verify' | 'forbidden' | 'unsupported' | 'unknown';
export interface UpdateCheckView {
  available: boolean; kind?: UpdateKind; version?: string; date?: string; notesSummary?: string;
  tier?: string; eligible: boolean; refusal: UpdateRefusal; handleId?: string; checkedAt: number; fresh: true;
}
export interface UpdateStatusView {
  state: UpdateState; handleId?: string; version?: string; kind?: UpdateKind;
  received?: number; total?: number; message: UpdateMessageCode;
}
export interface UpdateProgress { handleId: string; received: number; total?: number }
export interface UpdateDownloadResult {
  handleId: string; state: 'downloaded' | 'cancelled' | 'failed' | 'refused'; reason?: UpdateRefusal; message: UpdateMessageCode;
}
export interface UpdateInstallResult {
  state: 'installing' | 'refused' | 'failed'; reason?: UpdateRefusal | 'stale' | 'consumed'; message: UpdateMessageCode;
}
export type Unlisten = () => void;
export interface NativeUpdateTransport {
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>;
  listen<T>(event: string, callback: (event: { payload: T }) => void): Promise<Unlisten>;
}
export interface NativeUpdateClient {
  check(): Promise<UpdateCheckView>;
  status(): Promise<UpdateStatusView>;
  download(handleId: string): Promise<UpdateDownloadResult>;
  install(handleId: string): Promise<UpdateInstallResult>;
  cancel(handleId: string): Promise<{ cancelled: boolean }>;
  onProgress(callback: (progress: UpdateProgress) => void): Promise<Unlisten>;
  /** Also cleans registrations whose asynchronous listen call has not resolved. */
  dispose(): void;
}
export class NativeUpdateClientError extends Error {
  readonly code: UpdateErrorCode;
  readonly messageCode: UpdateMessageCode;
  constructor(code: UpdateErrorCode = 'unknown', messageCode: UpdateMessageCode = 'unknown') {
    super(UPDATE_MESSAGES[messageCode]);
    this.name = 'NativeUpdateClientError'; this.code = code; this.messageCode = messageCode;
  }
}
const kinds = ['patch', 'update'] as const;
const refusals = ['notLicensed', 'expired', 'revoked', 'tierIneligible', 'busy', 'unresolved', 'disabledNoPublicPatch', 'offline', 'none'] as const;
const states = ['idle', 'checking', 'available', 'downloading', 'downloaded', 'installing', 'failed'] as const;
const errorCodes = ['unconfigured', 'busy', 'unresolved', 'network', 'store', 'verify', 'forbidden', 'unsupported', 'unknown'] as const;
type Data = Record<string, unknown>;
function invalid(): never { throw new NativeUpdateClientError(); }
function record(value: unknown): Data {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) return invalid();
  return value as Data;
}
function member<T extends string>(value: unknown, values: readonly T[]): T {
  if (typeof value !== 'string' || !values.includes(value as T)) return invalid();
  return value as T;
}
function bool(value: unknown): boolean { return typeof value === 'boolean' ? value : invalid(); }
function numeric(value: unknown): number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : invalid();
}
function text(value: unknown, limit: number): string {
  return typeof value === 'string' && value.length <= limit && !/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/u.test(value) ? value : invalid();
}
function handle(value: unknown): string {
  return typeof value === 'string' && /^[A-Za-z0-9_-]{1,256}$/u.test(value) ? value : invalid();
}
function message(value: unknown): UpdateMessageCode {
  return typeof value === 'string' && Object.hasOwn(UPDATE_MESSAGES, value) ? value as UpdateMessageCode : 'unknown';
}
function releaseFields(source: Data): { kind?: UpdateKind; version?: string } {
  const result: { kind?: UpdateKind; version?: string } = {};
  if (source.kind !== undefined) result.kind = member(source.kind, kinds);
  if (source.version !== undefined) result.version = text(source.version, 256);
  return result;
}
function counts(source: Data): { received?: number; total?: number } {
  const result: { received?: number; total?: number } = {};
  if (source.received !== undefined) result.received = numeric(source.received);
  if (source.total !== undefined) result.total = numeric(source.total);
  if (result.received !== undefined && result.total !== undefined && result.received > result.total) invalid();
  return result;
}
function checkView(value: unknown): UpdateCheckView {
  const source = record(value);
  const result: UpdateCheckView = {
    ...releaseFields(source), available: bool(source.available), eligible: bool(source.eligible),
    refusal: member(source.refusal, refusals), checkedAt: numeric(source.checkedAt), fresh: true,
  };
  if (source.fresh !== true) invalid();
  if ((result.eligible || !result.available) && result.refusal !== 'none') invalid();
  if (source.date !== undefined) result.date = text(source.date, 128);
  if (source.notesSummary !== undefined) result.notesSummary = text(source.notesSummary, 8192);
  if (source.tier !== undefined) result.tier = text(source.tier, 128);
  if (source.handleId !== undefined) {
    if (!result.available || !result.eligible) invalid();
    result.handleId = handle(source.handleId);
  }
  return result;
}
function statusView(value: unknown): UpdateStatusView {
  const source = record(value);
  const result: UpdateStatusView = { ...releaseFields(source), ...counts(source), state: member(source.state, states), message: message(source.message) };
  if (source.handleId !== undefined) result.handleId = handle(source.handleId);
  return result;
}
function progressView(value: unknown): UpdateProgress {
  const source = record(value);
  return { ...counts(source), handleId: handle(source.handleId), received: numeric(source.received) };
}
function downloadResult(value: unknown): UpdateDownloadResult {
  const source = record(value);
  const result: UpdateDownloadResult = { handleId: handle(source.handleId), state: member(source.state, ['downloaded', 'cancelled', 'failed', 'refused']), message: message(source.message) };
  if (source.reason !== undefined) result.reason = member(source.reason, refusals);
  return result;
}
function installResult(value: unknown): UpdateInstallResult {
  const source = record(value);
  const result: UpdateInstallResult = { state: member(source.state, ['installing', 'refused', 'failed']), message: message(source.message) };
  if (source.reason !== undefined) result.reason = member(source.reason, [...refusals, 'stale', 'consumed'] as const);
  return result;
}
function safeError(value: unknown): NativeUpdateClientError {
  // Even Error instances and string rejections lose their native text and cause.
  try {
    const source = record(value);
    const code = member(source.code, errorCodes);
    const defaults: Record<typeof code, UpdateMessageCode> = {
      unconfigured: 'unconfigured', busy: 'busy', unresolved: 'unresolved', network: 'networkUnavailable',
      store: 'storeFailed', verify: 'verifyFailed', forbidden: 'forbidden', unsupported: 'unsupported',
      unknown: 'unknown',
    };
    return new NativeUpdateClientError(code, source.message === undefined ? defaults[code] : message(source.message));
  } catch { return new NativeUpdateClientError(); }
}
/** Native owns authorization, generations and lifecycle. Every call reads native state. */
export function createNativeUpdateClient(transport: NativeUpdateTransport): NativeUpdateClient {
  let disposed = false;
  const registrations = new Set<{ active: boolean; unlisten?: Unlisten }>();
  async function request<T>(command: string, project: (value: unknown) => T, args?: Record<string, unknown>): Promise<T> {
    if (disposed) throw new NativeUpdateClientError('unsupported', 'unsupported');
    let value: unknown;
    try { value = await transport.invoke<unknown>(command, args); }
    catch (error) { throw safeError(error); }
    try { return project(value); } catch { throw new NativeUpdateClientError(); }
  }
  function stop(registration: { active: boolean; unlisten?: Unlisten }): void {
    registration.active = false;
    registrations.delete(registration);
    const unlisten = registration.unlisten;
    registration.unlisten = undefined;
    if (unlisten) { try { unlisten(); } catch { /* Native cleanup text is never exposed. */ } }
  }
  return {
    check: () => request('rightkit_update_check', checkView),
    status: () => request('rightkit_update_status', statusView),
    async download(handleId) { return request('rightkit_update_download', downloadResult, { handleId: handle(handleId) }); },
    async install(handleId) { return request('rightkit_update_install', installResult, { handleId: handle(handleId) }); },
    async cancel(handleId) { return request('rightkit_update_cancel', value => ({ cancelled: bool(record(value).cancelled) }), { handleId: handle(handleId) }); },
    async onProgress(callback) {
      if (disposed) throw new NativeUpdateClientError('unsupported', 'unsupported');
      const registration: { active: boolean; unlisten?: Unlisten } = { active: true };
      registrations.add(registration);
      try {
        registration.unlisten = await transport.listen<unknown>('rightkit://update-progress', event => {
          if (!registration.active) return;
          let progress: UpdateProgress;
          try { progress = progressView(event.payload); } catch { return; }
          callback(progress);
        });
        if (typeof registration.unlisten !== 'function') throw new NativeUpdateClientError();
        if (!registration.active) stop(registration);
        return () => stop(registration);
      } catch (error) { stop(registration); throw safeError(error); }
    },
    dispose() { disposed = true; for (const registration of registrations) stop(registration); },
  };
}


