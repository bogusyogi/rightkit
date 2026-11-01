// Import form model and validation. Pure: the engine's dry run remains the authority on refusals.
// Rules mirror docs/ENGINE-CLI.md "asset import": origin is required, cleared rights need licence evidence,
// and scope is all three ids or none.
import type { AssetImportRequest, AssetScope, ImportEvidence, ImportOrigin, ImportRightsStatus } from './types/library';

export const IMPORT_ORIGIN_OPTIONS: Array<{ value: ImportOrigin; label: string }> = [
  { value: 'supplied', label: 'Supplied' },
  { value: 'acquired', label: 'Acquired' },
  { value: 'captured', label: 'Captured' }
];

export const IMPORT_EVIDENCE_OPTIONS: Array<{ value: ImportEvidence; label: string }> = [
  { value: 'unknown', label: 'Not recorded' },
  { value: 'authentic', label: 'Authentic' },
  { value: 'illustrative', label: 'Illustrative' }
];

export const IMPORT_RIGHTS_OPTIONS: Array<{ value: ImportRightsStatus; label: string }> = [
  { value: 'unknown', label: 'Not recorded' },
  { value: 'unresolved', label: 'Unresolved' },
  { value: 'cleared', label: 'Cleared' }
];

export interface ImportForm {
  path: string;
  origin: ImportOrigin | '';
  evidence: ImportEvidence;
  rightsStatus: ImportRightsStatus;
  /** One licence reference per line (a receipt, licence id or URL). */
  licenceEvidence: string;
  attribution: string;
  venture: string;
  channel: string;
  account: string;
}

export const EMPTY_IMPORT_FORM: ImportForm = {
  path: '',
  origin: '',
  evidence: 'unknown',
  rightsStatus: 'unknown',
  licenceEvidence: '',
  attribution: '',
  venture: '',
  channel: '',
  account: ''
};

export type ImportField = 'path' | 'origin' | 'licence' | 'scope';
export type ImportFormErrors = Partial<Record<ImportField, string>>;

/** The content-brief id pattern the engine checks for scope ids. */
export const SCOPE_ID_PATTERN = /^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$/;

/** A POSIX absolute path, a Windows drive path or a UNC path. */
export function isAbsolutePath(path: string): boolean {
  return /^(\/|[A-Za-z]:[\\/]|\\\\)/.test(path);
}

/** Licence references: one per non-empty line, trimmed. */
export function licenceEntries(text: string): string[] {
  return text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
}

export interface ImportValidation {
  errors: ImportFormErrors;
  request: AssetImportRequest | null;
}

/** Checks the form and builds the engine request when it is complete. Errors are per field. */
export function validateImportForm(form: ImportForm): ImportValidation {
  const errors: ImportFormErrors = {};
  const path = form.path.trim();
  if (!path) errors.path = 'Enter the full path of the file.';
  else if (!isAbsolutePath(path)) errors.path = 'Use the full path, such as /Users/you/Pictures/photo.png.';

  if (!form.origin) errors.origin = 'Choose where the file came from.';

  const licence = licenceEntries(form.licenceEvidence);
  if (form.rightsStatus === 'cleared' && licence.length === 0) {
    errors.licence = 'Cleared rights need licence evidence: a receipt, licence id or URL, one per line.';
  }

  const scopeIds = [form.venture, form.channel, form.account].map((s) => s.trim());
  const given = scopeIds.filter(Boolean).length;
  if (given > 0 && given < 3) {
    errors.scope = 'Scope needs venture, channel and account together, or none of them.';
  } else if (given === 3 && scopeIds.some((id) => !SCOPE_ID_PATTERN.test(id))) {
    errors.scope = 'Scope ids start with a letter or digit and use letters, digits, and . _ : - (up to 128 characters).';
  }

  if (Object.keys(errors).length > 0 || !form.origin) return { errors, request: null };

  const attribution = form.attribution.trim();
  const request: AssetImportRequest = {
    path,
    origin: form.origin,
    evidence: form.evidence,
    rights: {
      status: form.rightsStatus,
      ...(licence.length ? { licence_evidence: licence } : {}),
      ...(attribution ? { attribution } : {})
    }
  };
  if (given === 3) {
    const [venture, channel, account] = scopeIds;
    const scope: AssetScope = { venture, channel, account };
    request.scope = scope;
  }
  return { errors, request };
}
