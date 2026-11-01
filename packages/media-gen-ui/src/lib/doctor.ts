import type { DoctorReport } from './api.js';

/** Pick the first present scalar field from a doctor report. */
export function pick(doctor: DoctorReport | null, ...keys: string[]): string | null {
  if (!doctor) return null;
  for (const k of keys) {
    // Dotted keys read nested fields, e.g. "platform.os".
    const v = k.split('.').reduce<unknown>((o, part) => (o && typeof o === 'object' ? (o as Record<string, unknown>)[part] : undefined), doctor);
    if (typeof v === 'string' || typeof v === 'number') return String(v);
  }
  return null;
}

/** Scalar fields of the report as label/value rows, for a generic readout. */
export function scalarRows(doctor: DoctorReport | null): Array<[string, string]> {
  if (!doctor) return [];
  return Object.entries(doctor)
    .filter(([, v]) => ['string', 'number', 'boolean'].includes(typeof v))
    .map(([k, v]) => [k.replace(/_/g, ' ').replace(/^./, (c) => c.toUpperCase()), String(v)]);
}

export function doctorHealthy(doctor: DoctorReport | null, error: string | null): 'ok' | 'bad' | 'idle' {
  if (error) return 'bad';
  if (!doctor) return 'idle';
  if (doctor.ok === false) return 'bad';
  return 'ok';
}
