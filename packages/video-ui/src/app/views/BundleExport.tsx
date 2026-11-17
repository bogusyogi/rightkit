// Portable bundle — write this project to a folder that can be imported elsewhere (bundle.export).
import { Button, useToast } from "@rightkit/react-shell/react";
import { errorMessage } from "../../lib/engine.js";
import type { BundleMedia } from "../../lib/media.js";
import { baseName } from "../../lib/project.js";
import { useVideoBundleExport } from "../hooks/useVideoBundleExport.js";
import { ProgressRow, fileSize, Icon } from "../ui/index.js";

const MEDIA_CHOICES: { value: BundleMedia; label: string; note: string }[] = [
  { value: "collected", label: "Include media", note: "Copies every source file into the bundle." },
  {
    value: "referenced",
    label: "Reference media in place",
    note: "Lists each source by its current path; nothing is copied.",
  },
];

export function BundleExport({ dir }: { dir: string }) {
  const toast = useToast();
  const { media, setMedia, folder, busy, progress, error, result, choose, run, reveal } = useVideoBundleExport({ dir });

  const choice = MEDIA_CHOICES.find((c) => c.value === media) ?? MEDIA_CHOICES[0];

  return (
    <section className="bundle-export" aria-label="Portable bundle">
      <h2 className="panel-title">Portable bundle</h2>
      <p className="panel-note">Write this project to a folder you can import on another computer.</p>

      <div className="panel-stack">
        <div className="batch-checks" role="radiogroup" aria-label="Media">
          {MEDIA_CHOICES.map((c) => (
            <label key={c.value}>
              <input
                type="radio"
                name="bundle-media"
                value={c.value}
                checked={media === c.value}
                disabled={busy}
                onChange={() => setMedia(c.value)}
              />
              {c.label}
            </label>
          ))}
        </div>
        <p className="panel-note">{choice.note}</p>

        <div className="panel-row">
          <Button variant="secondary" size="sm" onClick={() => void choose("Choose an empty folder for the bundle")} disabled={busy}><Icon name="folder-open" />
            Choose folder…
          </Button>
          {folder && (
            <span className="cr-mono cr-clamp-1 bundle-export__path" title={folder}>
              {folder}
            </span>
          )}
        </div>

        <div className="panel-row">
          <Button
            variant="primary"
            size="sm"
            onClick={() => void run()}
            disabled={!folder || busy}
            aria-busy={(busy) || undefined}
          >
            {busy ? "Exporting…" : "Export bundle"}
          </Button>
        </div>

        {busy && (
          <ProgressRow
            label="Bundle"
            status={progress ? `${progress.completed} of ${progress.total} files` : "Preparing"}
            fraction={progress && progress.total > 0 ? progress.completed / progress.total : undefined}
          />
        )}

        {error && (
          <p role="alert" className="bundle-export__error">
            {error}
          </p>
        )}

        {result && (
          <div className="bundle-export__result">
            <p role="status" className="panel-note">
              Bundle written to <span className="cr-mono bundle-export__path">{result.out}</span>
            </p>
            <p className="panel-note">
              {result.members} files, {fileSize(result.bytes)}, plus bundle.json
            </p>
            {result.external.length > 0 && (
              <div className="bundle-export__external">
                <span className="cr-overline">Not in the bundle</span>
                <ul>
                  {result.external.map((e) => (
                    <li key={`${e.kind}:${e.path}`} title={e.path}>
                      {e.name ?? baseName(e.path)}
                      <span className="cr-muted">{e.kind === "asset" ? " · missing font or LUT" : " · referenced in place"}</span>
                    </li>
                  ))}
                </ul>
              </div>
            )}
            <div className="panel-row">
              <Button
                size="sm"
                variant="ghost"
                onClick={() => void reveal().catch((e) => toast(errorMessage(e), { kind: "error", ttl: 0 }))}
              ><Icon name="folder-open" />
                Reveal bundle
              </Button>
            </div>
          </div>
        )}
      </div>
    </section>
  );
}
