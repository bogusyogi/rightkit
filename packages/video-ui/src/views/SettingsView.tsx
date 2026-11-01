import { Badge, Button, Dialog, SegmentedControl, Input, Select, Toggle, useShell, useToast } from "@rightkit/react-shell/react";
import { Suspense, lazy, useEffect, useMemo, useState, useSyncExternalStore } from "react";
import { getVersion } from "@tauri-apps/api/app";
import {
  SettingRow,
  SettingsGroup,
  SettingsPanel,
  aboutSection,
  appearanceSection,
  diagnosticsSection,
  formatBytes,
  legalSection,
  licenseSection,
  mergeSections,
  modelsSection,
  updatesSection,
  type SettingsContext,
  type SettingsSection,
} from "@rightkit/settings-ui";
import "@rightkit/settings-ui/settings.css";
import { Kbd } from "@rightkit/react-shell/react";
import type { DoctorReport, EngineStatus } from "../lib/engine";
import { engineCall, errorMessage } from "../lib/engine";
import { CAPTION_STYLES, DEFAULT_CAPTION_STYLE, storageClear, storageStatus } from "../lib/media";
import { type HostInfo, genRightCallFailed, interpretHost } from "../lib/genright";
import { pickFile, pickFolder } from "../lib/dialog";
import { createModelsBackend } from "../lib/modelsBackend";
import { createHostSettingsBackend } from "../lib/settingsBackend";
import { themeStore } from "../lib/shell";
import { licenseClient, logs, restartAfterUpdate, updates } from "../lib/services";
import { LEGAL_GATE_DOCUMENTS, legalDocumentText, type LegalDocumentText } from "../legal/legalConfig";
import { LegalDocumentBody } from "@rightkit/legal-ui";
import { usePlatform } from "@rightkit/platform-ui/react";

import { HDR_COLOR_PRESETS, HDR_UNAVAILABLE_NOTICE, useHdrToSdr } from "../hooks/useHdrToSdr";
import { hostAppName } from "../appName";

const COLOR_PRESETS = [
  ["neutral", "Neutral Rec.709"],
  ["hdr-natural", "HDR natural"],
  ["hdr-soft", "HDR soft"],
  ["apple-log-natural", "Apple Log natural"],
  ["talking-head-auto", "Talking head (auto)"],
  ["warm", "Warm"],
  ["cool", "Cool"],
  ["lift", "Lift"],
] as const;
const AUDIO_PRESETS = [
  ["", "None"],
  ["talking-head", "Talking head"],
  ["podcast", "Podcast"],
  ["noisy-room", "Noisy room"],
] as const;
/** Plain-language rows for the engine's readiness checks. An optional check never reads as "engine not ready". */
const DIAGNOSTICS: { id: string; label: string; optional?: boolean; sentence: Record<string, string> }[] = [
  {
    id: "data_dir",
    label: "App data folder",
    sentence: {
      ok: `Writable — ${hostAppName()} can save projects, settings and caches here.`,
      failed: `Not writable — ${hostAppName()} cannot save projects until this is fixed.`,
      missing: "Not found on this machine.",
    },
  },
  {
    id: "resource_dir",
    label: "App resources",
    sentence: {
      ok: "Found — bundled tools are available.",
      failed: "Not readable — bundled tools are unavailable.",
      missing: "Not running from an app bundle; bundled tools are unavailable.",
    },
  },
  {
    id: "editing_ffmpeg",
    label: "Video engine (FFmpeg)",
    sentence: {
      ok: "Found — probing, thumbnails and rendering work.",
      failed: "Present but unusable — media features will fail.",
      missing: "Not found — probing, thumbnails and rendering will fail.",
    },
  },
  {
    id: "hdr_to_sdr",
    label: "HDR to SDR conversion",
    optional: true,
    sentence: {
      ok: "Available — HDR (PQ or HLG) footage can be finished to SDR here.",
      failed: "Not available — HDR (PQ or HLG) footage cannot be finished on this computer.",
      missing: "This engine version does not report HDR to SDR conversion.",
    },
  },
];

/** Colour finish options; with HDR to SDR unavailable the HDR presets stay selectable but say so. */
function colourOptions(hdr: boolean | undefined) {
  return COLOR_PRESETS.map(([v, l]) =>
    hdr === false && (HDR_COLOR_PRESETS as readonly string[]).includes(v) ? ([v, `${l} — not available on this computer`] as const) : ([v, l] as const),
  );
}

const str = (v: unknown, fallback = "") => (v === undefined || v === null ? fallback : String(v));

function EditingSection({ ctx }: { ctx: SettingsContext }) {
  const formats = (ctx.values.default_formats as string[] | undefined) ?? ["16:9"];
  return (
    <SettingsGroup>
      <SettingRow id="cut-style" label="Cut style" description="Seeds the edit recipe for new projects.">
        <SegmentedControl
          label="Cut style"
          value={str(ctx.values.cut_preset, "natural") as "natural" | "tight" | "punchy"}
          options={[
            { value: "natural", label: "Natural" },
            { value: "tight", label: "Tight" },
            { value: "punchy", label: "Punchy" },
          ]}
          onChange={(v) => void ctx.set("cut_preset", v)}
        />
      </SettingRow>
      <SettingRow id="formats" label="Formats" description="Formats the recipe produces by default.">
        <span className="cr-settings-chips">
          {["16:9", "9:16", "1:1"].map((f) => (
            <Button variant="ghost" size="sm" className="cr-chip-btn" key={f} onClick={() =>
                void ctx.set("default_formats", formats.includes(f) ? formats.filter((x) => x !== f) : [...formats, f])}><Badge
              tone={formats.includes(f) ? "accent" : "neutral"}
            >
              {f}
            </Badge></Button>
          ))}
        </span>
      </SettingRow>
    </SettingsGroup>
  );
}

function FinishingSection({ ctx }: { ctx: SettingsContext }) {
  const lut = str(ctx.values.lut_path);
  const hdr = useHdrToSdr();
  const preset = str(ctx.values.color_preset, "neutral");
  // A saved HDR preset that cannot run is kept, and the notice is an alert because it is in use.
  const hdrInUse = (HDR_COLOR_PRESETS as readonly string[]).includes(preset);
  return (
    <>
      <SettingsGroup title="Colour">
        <SettingRow id="color-preset" label="Colour finish" description="Applied to every render.">
          <span className="cr-settings-field">
            <Select label="Colour finish" value={preset} options={colourOptions(hdr).map(([value, label]) => ({ value, label }))} onChange={(v) => void ctx.set("color_preset", v)} />
            {hdr === false && (
              <p className="cr-settings-note" role={hdrInUse ? "alert" : undefined}>
                {HDR_UNAVAILABLE_NOTICE}
              </p>
            )}
          </span>
        </SettingRow>
        <SettingRow id="custom-lut" label="Custom LUT" description={lut ? lut.split(/[\\/]/).pop() : "A .cube LUT. It is the default for the Custom finish only; other finishes ignore it."}>
          <span className="cr-settings-chips">
            <Button
              size="sm"
              variant="secondary"
              onClick={() => void pickFile("Choose a LUT file", ["cube", "3dl"]).then((f) => {
              if (f) void ctx.set("lut_path", f);
            })}
            >
              Choose…
            </Button>
            {lut && (
              <Button size="sm" variant="ghost" onClick={() => void ctx.set("lut_path", null)}>
                Clear
              </Button>
            )}
          </span>
        </SettingRow>
        {lut && (
          <SettingRow id="lut-intensity" label="LUT intensity" description="How strongly the LUT is applied (0–100%).">
            <Input
              className="cr-settings-num"
              aria-label="LUT intensity (percent)"
              type="number"
              min={0}
              max={100}
              step={5}
              value={str(ctx.values.lut_intensity, "100")}
              onChange={(e) => void ctx.set("lut_intensity", Number(e.target.value))}
            />
          </SettingRow>
        )}
        <SettingRow id="match-shots" label="Match shots" description="Match colour between sources before grading.">
          <Toggle
            label="Match shots"
            checked={ctx.values.match_shots !== false}
            onChange={(v) => void ctx.set("match_shots", v)}
          />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Framing">
        <SettingRow id="framing-approval" label="Approve framing automatically" description="Approve analysed framing using saved policy; turn off to review each plan.">
          <Toggle
            label="Approve framing automatically"
            checked={ctx.values.framing_approval === "auto"}
            onChange={(v) => void ctx.set("framing_approval", v ? "auto" : "review")}
          />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Audio">
        <SettingRow id="audio-preset" label="Audio finish" description="Cleanup and loudness target.">
          <Select label="Audio finish" value={str(ctx.values.audio_preset, "talking-head")} options={AUDIO_PRESETS.map(([value, label]) => ({ value, label }))} onChange={(v) => void ctx.set("audio_preset", v)} />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Captions">
        <SettingRow id="burn-captions" label="Burn captions" description="Captions are drawn into finished edits.">
          <Toggle
            label="Burn captions"
            checked={ctx.values.burn_captions !== false}
            onChange={(v) => void ctx.set("burn_captions", v)}
          />
        </SettingRow>
        <SettingRow id="caption-style" label="Caption style">
          <Select
            label="Caption style"
            value={str(ctx.values.caption_style, DEFAULT_CAPTION_STYLE)}
            options={CAPTION_STYLES.map(([value]) => ({ value, label: value.replace(/-/g, " ") }))}
            onChange={(v) => void ctx.set("caption_style", v)}
          />
        </SettingRow>
      </SettingsGroup>
    </>
  );
}

/** Encoder choices per platform; ids must match the engine's encoder names. */
const ENCODER_OPTIONS_MAC: readonly (readonly [string, string])[] = [
  ["auto", "Auto"],
  ["videotoolbox", "VideoToolbox"],
];
const ENCODER_OPTIONS_WINDOWS: readonly (readonly [string, string])[] = [
  ["auto", "Auto"],
  ["nvenc", "NVIDIA NVENC"],
  ["mf", "Media Foundation"],
];

function OutputSection({ ctx }: { ctx: SettingsContext }) {
  const encoderOptions = usePlatform() === "macos" ? ENCODER_OPTIONS_MAC : ENCODER_OPTIONS_WINDOWS;
  const savedEncoder = str(ctx.values.encoder, "auto");
  // A saved value that this platform does not offer (e.g. legacy "libx264") shows as Auto.
  const encoderValue = encoderOptions.some(([v]) => v === savedEncoder) ? savedEncoder : "auto";
  return (
    <SettingsGroup>
      <SettingRow id="encoder" label="Encoder" description="The system H.264 encoder for exports.">
        <Select
          label="Encoder"
          value={encoderValue}
          options={encoderOptions.map(([value, label]) => ({ value, label }))}
          onChange={(v) => void ctx.set("encoder", v)}
        />
      </SettingRow>
      <SettingRow id="export-dir" label="Export folder" description={str(ctx.values.export_dir, "Renders stay inside the project folder.")}>
        <Button
          size="sm"
          variant="secondary"
          onClick={() => void pickFolder("Default export folder").then((d) => {
              if (d) void ctx.set("export_dir", d);
            })}
        >
          Choose…
        </Button>
      </SettingRow>
    </SettingsGroup>
  );
}

function StorageSection({ ctx, dir }: { ctx: SettingsContext; dir: string | null }) {
  const toast = useToast();
  const [storage, setStorage] = useState<{ cache_bytes: number; default_project_root?: string } | null>(null);
  useEffect(() => {
    storageStatus(dir ?? undefined).then(setStorage).catch(() => setStorage(null));
  }, [dir]);
  return (
    <SettingsGroup>
      <SettingRow
        id="project-folder"
        label="Project folder"
        description={`Dropped footage creates a new project here: ${str(ctx.values.project_root, storage?.default_project_root ?? "")}`}
      >
        <Button
          size="sm"
          variant="secondary"
          onClick={() => void pickFolder("Project folder").then((d) => {
              if (d) void ctx.set("project_root", d);
            })}
        >
          Choose…
        </Button>
      </SettingRow>
      <SettingRow id="cache" label="Cache" description="Thumbnails and waveforms are rebuilt on demand; clearing is safe.">
        <span className="cr-settings-chips">
          <span className="cr-mono">{storage ? formatBytes(storage.cache_bytes) : "…"}</span>
          <Button
            size="sm"
            variant="secondary"
            disabled={!storage || storage.cache_bytes === 0}
            onClick={() =>
              void storageClear(dir ?? undefined)
                .then((r) => {
                  setStorage((s) => (s ? { ...s, cache_bytes: 0 } : s));
                  toast(`Cleared ${formatBytes(r.freed_bytes)}`, { kind: "success" });
                })
                .catch((e) => toast(errorMessage(e), { kind: "error", ttl: 0 }))
            }
          >
            Clear
          </Button>
        </span>
      </SettingRow>
    </SettingsGroup>
  );
}

function HostSection({ ctx }: { ctx: SettingsContext }) {
  const [gr, setGr] = useState<HostInfo>({ state: "checking" });
  const [path, setPath] = useState(str(ctx.values.genright_engine_path));
  const check = () =>
    void engineCall("genright.status", {})
      .then((r) => setGr(interpretHost(r)))
      .catch((e) => setGr(genRightCallFailed(errorMessage(e))));
  useEffect(check, []);
  return (
    <SettingsGroup>
      <SettingRow id="genright-path" label="Engine location" description={`Path to the ${hostAppName()} engine; empty means auto-detect.`} stacked>
        <span className="cr-settings-chips">
          <Input
            className="cr-settings-path"
            aria-label={`${hostAppName()} engine path`}
            value={path}
            placeholder="Auto-detect"
            onChange={(e) => setPath(e.target.value)}
            onBlur={() => void ctx.set("genright_engine_path", path.trim() === "" ? null : path.trim())}
          />
          <Button
            size="sm"
            variant="secondary"
            onClick={() =>
              void pickFile(`Locate the ${hostAppName()} engine`, null).then((f) => {
                if (f) {
                  setPath(f);
                  void ctx.set("genright_engine_path", f);
                }
              })
            }
          >
            Locate…
          </Button>
        </span>
      </SettingRow>
      <SettingRow id="genright-status" label="Status" description={gr.detail ?? `Media generation runs through the ${hostAppName()} app.`}>
        <span className="cr-settings-chips">
          <Badge tone={gr.state === "ready" ? "ok" : gr.state === "checking" ? "neutral" : "warn"}>
            {gr.state === "ready" ? `${hostAppName()} ready` : gr.state === "checking" ? "Checking…" : "Not available"}
          </Badge>
          <Button size="sm" variant="secondary" onClick={check}>
            Re-check
          </Button>
        </span>
      </SettingRow>
      <SettingRow id="genright-budget" label="Budget" description="Optional spend cap (USD) for generation jobs.">
        <Input
          className="cr-settings-num"
          aria-label={`${hostAppName()} budget (USD)`}
          type="number"
          min={0}
          step={1}
          value={str(ctx.values.genright_budget_usd)}
          placeholder="No cap"
          onChange={(e) => void ctx.set("genright_budget_usd", e.target.value === "" ? null : Number(e.target.value))}
        />
      </SettingRow>
    </SettingsGroup>
  );
}

function EngineSection({ doctor, error }: { doctor: DoctorReport | null; error: string | null }) {
  const checks = doctor?.readiness?.checks ?? [];
  return (
    <SettingsGroup description={error ?? undefined}>
      {DIAGNOSTICS.map((d) => {
        const c = checks.find((x) => x.id === d.id);
        const st = (c?.status ?? "").toLowerCase();
        // An optional check that is absent from a loaded report is reported as unavailable, not still checking.
        const waiting = !c && !(d.optional && doctor);
        const ok = st === "ok";
        const tone = ok ? "ok" : waiting || d.optional ? "neutral" : "warn";
        const label = ok ? "OK" : waiting ? "Checking…" : d.optional ? "Unavailable" : "Needs attention";
        const sentence = waiting ? "Waiting for the engine check." : !c ? d.sentence.missing : (d.sentence[st] ?? d.sentence.failed);
        return (
          <SettingRow
            key={d.id}
            id={`diag-${d.id}`}
            label={d.label}
            description={
              <>
                {sentence}
                {c?.detail && (
                  <details className="cr-settings-detail">
                    <summary>Details</summary>
                    <p>{c.detail}</p>
                  </details>
                )}
              </>
            }
          >
            <Badge tone={tone}>{label}</Badge>
          </SettingRow>
        );
      })}
    </SettingsGroup>
  );
}

/** Every binding on the shell's shortcut registry, as the help overlay lists them (hidden ones omitted). */
// Agent demo settings: a lazy chunk that exists only when `__RIGHTKIT_DEMO__` (vite.config.ts), so a
// release bundle neither imports nor contains src/demo.
const DEMO_BUILD = typeof __RIGHTKIT_DEMO__ === "boolean" && __RIGHTKIT_DEMO__;
const DemoSettingsLazy = DEMO_BUILD
  ? lazy(() => import("../demo/DemoSettings").then((m) => ({ default: m.DemoSettings })))
  : null;

function ShortcutsSection() {
  const { registry } = useShell();
  useSyncExternalStore(registry.subscribe, registry.version);
  return (
    <>
      {registry.groups().map((group) => (
        <SettingsGroup key={group.title} title={group.title}>
          {group.items.map((info) => (
            <SettingRow key={info.id} id={`key-${info.id}`} label={info.label}>
              {info.chords.map((chord) => (
                <Kbd key={chord} chord={chord} />
              ))}
            </SettingRow>
          ))}
        </SettingsGroup>
      ))}
    </>
  );
}

export function SettingsView({
  dir,
  doctor,
  error,
  section,
}: {
  dir: string | null;
  status: EngineStatus | null;
  doctor: DoctorReport | null;
  error: string | null;
  /** Land on this section (engine status → "engine"). */
  section?: string;
}) {
  const backend = useMemo(() => createHostSettingsBackend(), []);
  const models = useMemo(() => createModelsBackend(), []);
  const [version, setVersion] = useState("");
  const [active, setActive] = useState<string | undefined>(section);
  const [reading, setReading] = useState<LegalDocumentText | null>(null);
  useEffect(() => {
    getVersion().then(setVersion).catch(() => setVersion(""));
  }, []);
  useEffect(() => {
    if (section) setActive(section === "models" ? "engine" : section);
  }, [section]);

  const sections: SettingsSection[] = mergeSections(
    [
      aboutSection({ appName: hostAppName(), version: version || "dev", copyright: "Orthic Labs" }),
      appearanceSection({ themeStore }),
      modelsSection({ models }),
      licenseSection({ client: licenseClient }),
      updatesSection({ client: updates, currentVersion: version || "dev", onRestart: restartAfterUpdate }),
      diagnosticsSection({ logs }),
      legalSection({
        documents: LEGAL_GATE_DOCUMENTS,
        // legalSection's per-document callback (settings-ui sections.tsx legalSection `openExternal`): the
        // bundled documents open in the owner Dialog + LegalDocumentBody below; no document is opened
        // externally and legal-ui has no reader component to adopt instead (its exports are LegalGate,
        // policyGateDocuments and LegalDocumentBody).
        openExternal: (url) => {
          const document = LEGAL_GATE_DOCUMENTS.find((entry) => entry.url === url);
          if (document) setReading({ title: document.title, text: legalDocumentText(document.path) });
        },
      }),
    ],
    [
      { id: "editing", label: "Editing defaults", group: "Editing", order: 300, keywords: ["cut", "format"], render: (ctx) => <EditingSection ctx={ctx} /> },
      {
        id: "finishing",
        label: "Finishing",
        group: "Editing",
        order: 310,
        keywords: ["colour", "color", "lut", "audio", "captions", "loudness"],
        entries: [
          { id: "color-preset", label: "Colour finish" },
          { id: "custom-lut", label: "Custom LUT" },
          { id: "match-shots", label: "Match shots" },
          { id: "audio-preset", label: "Audio finish" },
          { id: "burn-captions", label: "Burn captions" },
          { id: "caption-style", label: "Caption style" },
        ],
        render: (ctx) => <FinishingSection ctx={ctx} />,
      },
      { id: "output", label: "Output", group: "Editing", order: 320, keywords: ["encoder", "export"], render: (ctx) => <OutputSection ctx={ctx} /> },
      { id: "storage", label: "Storage", group: "App", order: 400, keywords: ["cache", "project folder"], render: (ctx) => <StorageSection ctx={ctx} dir={dir} /> },
      { id: "genright", label: "Media generation", group: "App", order: 410, keywords: ["generate", "assets", "budget"], render: (ctx) => <HostSection ctx={ctx} /> },
      { id: "engine", label: "Engine", group: "App", order: 420, keywords: ["diagnostics", "ffmpeg", "doctor"], render: () => <EngineSection doctor={doctor} error={error} /> },
      { id: "shortcuts", label: "Shortcuts", group: "App", order: 430, keywords: ["keys", "keyboard"], render: () => <ShortcutsSection /> },
      // Agent demo mode: present only in builds with the demo feature (vite.config.ts), off by default.
      ...(DEMO_BUILD ? [{ id: "agent-demos", label: "Agent demos", group: "App", order: 440, keywords: ["agent", "demo", "record", "automation", "consent"], render: () => DemoSettingsLazy ? <Suspense fallback={null}><DemoSettingsLazy /></Suspense> : null }] : []),
    ],
  );

  return (
    <>
      <SettingsPanel
        backend={backend}
        sections={sections}
        title="Settings"
        activeId={active}
        onActiveChange={setActive}
        defaultActiveId="appearance"
      />
      {reading && (
        <Dialog
          title={reading.title}
          ariaLabel={reading.title}
          width={760}
          onClose={() => setReading(null)}
          dismissible={false}
          actions={<Button onClick={() => setReading(null)}>Close</Button>}
        >
          <LegalDocumentBody content={reading.text} format="markdown" title={reading.title} />
        </Dialog>
      )}
    </>
  );
}
