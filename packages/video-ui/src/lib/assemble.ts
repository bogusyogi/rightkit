// Request/response shapes of `assemble`, exactly as `AssembleInput` deserializes
// (crates/cutright-engine/src/genright_cmd.rs; every struct there is deny_unknown_fields
// except TitleSpec, whose flattened TitleTemplate is).

export type ShotInput = {
  prompt?: string;
  kind: "video" | "image";
  duration_s: number;
  provider: string;
  model: string;
  params?: Record<string, unknown>;
};

export type NarrationInput = { text: string; provider: string; model: string; voice?: string };

/** TitleSpec: the template fields (`template` tag + its text fields) flattened beside timing. */
export type TitleInput = (
  | { template: "lower_third"; title: string; subtitle?: string }
  | { template: "centered"; text: string }
  | { template: "caption"; text: string }
) & { start_s: number; duration_s: number; fade_in_s?: number; fade_out_s?: number };

export type AssembleInput = {
  prompt: string;
  assemble_id: string;
  shots: ShotInput[];
  narration?: NarrationInput;
  titles?: TitleInput[];
  preset: "youtube" | "reels";
  encoder?: string;
  render: boolean;
  out?: string;
};

/** What the form holds; titles keep one editable text field plus an optional subtitle. */
export type TitleDraft = {
  template: "lower_third" | "centered" | "caption";
  text: string;
  subtitle: string;
  start_s: number;
  duration_s: number;
};

export type AssembleForm = {
  prompt: string;
  shots: ShotInput[];
  narration: { text: string; provider: string; model: string; voice: string } | null;
  titles: TitleDraft[];
  preset: "youtube" | "reels";
  render: boolean;
};

/** assemble_id: letters, digits, '-' or '_' (max 64). */
export function newAssembleId(): string {
  const rand = Math.random().toString(36).slice(2, 8);
  return `a-${Date.now().toString(36)}-${rand}`;
}

export function titleInput(t: TitleDraft): TitleInput {
  const timing = { start_s: t.start_s, duration_s: t.duration_s };
  const text = t.text.trim();
  if (t.template === "lower_third") {
    const subtitle = t.subtitle.trim();
    return { template: "lower_third", title: text, ...(subtitle ? { subtitle } : {}), ...timing };
  }
  return { template: t.template, text, ...timing };
}

export function buildAssembleInput(form: AssembleForm, assembleId: string): AssembleInput {
  const input: AssembleInput = {
    prompt: form.prompt.trim(),
    assemble_id: assembleId,
    shots: form.shots.map((s) => ({
      ...(s.prompt?.trim() ? { prompt: s.prompt.trim() } : {}),
      kind: s.kind,
      duration_s: s.duration_s,
      provider: s.provider,
      model: s.model,
    })),
    preset: form.preset,
    render: form.render,
  };
  const n = form.narration;
  if (n && n.text.trim()) {
    input.narration = {
      text: n.text.trim(),
      provider: n.provider,
      model: n.model,
      ...(n.voice.trim() ? { voice: n.voice.trim() } : {}),
    };
  }
  if (form.titles.length > 0) input.titles = form.titles.map(titleInput);
  return input;
}

/** The `assemble.status` result for one assemble id. */
export type AssembleStatus = {
  assemble_id: string;
  /** running | done | failed | cancelled */
  status: string;
  stage: string;
  error: { code?: string; message?: string } | null;
  timeline_built: boolean;
  render: { output?: string } | null;
  /** false when the stored render's output file is missing or no render was stored; absent on an older engine. */
  render_exists?: boolean;
  requests: { item: string; request_id: string; state: string | null; job_id: string | null; imported: boolean }[];
  updated: number;
};

// ---- persisted per project, so "Resume" survives an app restart ----

export type PendingAssemble = { assembleId: string; input: AssembleInput; startedAt: number };

const key = (dir: string) => `cutright.assemble.${dir}`;

export function loadPending(dir: string): PendingAssemble | null {
  try {
    const v = JSON.parse(localStorage.getItem(key(dir)) ?? "null");
    if (v && typeof v.assembleId === "string" && v.input && typeof v.input === "object") return v as PendingAssemble;
  } catch {
    /* unavailable or corrupt */
  }
  return null;
}

export function savePending(dir: string, pending: PendingAssemble) {
  try {
    localStorage.setItem(key(dir), JSON.stringify(pending));
  } catch {
    /* ignore */
  }
}

export function clearPending(dir: string) {
  try {
    localStorage.removeItem(key(dir));
  } catch {
    /* ignore */
  }
}
