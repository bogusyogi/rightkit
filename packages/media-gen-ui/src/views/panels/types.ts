import type { MediaKind } from '../../lib/api.js';

/** What a studio panel inside the Generate workspace can see and change. */
export interface StudioPanelProps {
  kind: MediaKind;
  provider: string | null;
  model: string | null;
  prompt: string;
  setPrompt: (text: string) => void;
  /** Extra params merged into the generate request (panel-owned keys only). */
  setParams: (patch: Record<string, unknown>) => void;
  /** Non-null blocks Generate with this reason (e.g. preflight not approved). */
  setBlock: (reason: string | null) => void;
  /** Optional composer-inline action the panel wants surfaced next to Generate (e.g. Run preflight). Pass null to clear. */
  setAction?: (action: { label: string; run: () => void; disabled?: boolean } | null) => void;
}
