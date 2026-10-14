import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const workspaceRoot = path.resolve(here, "../../..");
const contractPath = path.join(here, "contract.json");

const sources = {
  hrDecode: "heardright/tauri-app-next/heardright-inference/src/file_transcribe.rs",
  hrStage: "heardright/tauri-app-next/scripts/stage-ffmpeg.mjs",
  srArgs: "scraperight/crates/audio/src/args.rs",
  srProcess: "scraperight/crates/audio/src/process.rs",
  srWav: "scraperight/crates/audio/src/wav.rs",
  srTauriPaths: "scraperight/src-tauri/src/engine_paths.rs",
  srEnginePaths: "scraperight/crates/engine/src/extraction/paths.rs",
  srPackageWin: "scraperight/package.ps1",
  srPackageMac: "scraperight/package.sh",
  srNotices: "scraperight/scripts/third-party-manifest.json",
};

const requiredPatterns = {
  hrDecode: [
    /HR_FFMPEG_PATH/,
    /join\("runtime"\)\.join\(name\)/,
    /join\("Resources"\)\.join\("runtime"\)\.join\(name\)/,
    /PathBuf::from\(name\)/,
    /\.args\(\["-y", "-hide_banner", "-loglevel", "error", "-i"\]\)/,
    /\.args\(\["-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"\]\)/,
    /\.output\(\)/,
    /v as f32 \/ i16::MAX as f32/,
  ],
  hrStage: [
    /HR_FFMPEG_WIN_X64_SRC/,
    /HR_FFMPEG_DARWIN_SRC/,
    /--enable-gpl\|--enable-nonfree/,
    /license check passed \(LGPL confirmed\)/,
    /ffmpeg-LICENSE\.txt/,
  ],
  srArgs: [
    /"-y"\.into\(\),\s*"-i"\.into\(\)/s,
    /"-ac"\.into\(\),\s*"1"\.into\(\)/s,
    /"-ar"\.into\(\),\s*"16000"\.into\(\)/s,
    /"-c:a"\.into\(\),\s*"pcm_s16le"\.into\(\)/s,
  ],
  srProcess: [/should_cancel\(\)/, /child\.kill\(\)/, /child\.wait\(\)/, /Duration::from_millis\(100\)/],
  srWav: [/spec\.channels != 1 \|\| spec\.sample_rate != 16_000/, /v as f32 \/ i16::MAX as f32/],
  srTauriPaths: [/SCRAPERIGHT_FFMPEG/, /bundled_ffmpeg/, /packaging defect/],
  srEnginePaths: [/SCRAPERIGHT_FFMPEG/, /PathBuf::from\("ffmpeg"\)/],
  srPackageWin: [/--enable-gpl/, /--enable-nonfree/, /LGPL ffmpeg/],
  srPackageMac: [/--enable-gpl\|--enable-nonfree/, /LGPL-only/],
  srNotices: [/FFmpeg \(LGPL, decode-only build\)/, /LGPL-2\.1-or-later/, /LGPL-2\.1\.txt/],
};

async function sha256(file) {
  const contents = await readFile(file);
  return createHash("sha256").update(contents).digest("hex");
}

async function assertSourceContract() {
  for (const [key, relative] of Object.entries(sources)) {
    const text = await readFile(path.join(workspaceRoot, relative), "utf8");
    for (const pattern of requiredPatterns[key]) {
      if (!pattern.test(text)) {
        throw new Error(`media characterization drift: ${relative} no longer matches ${pattern}`);
      }
    }
  }
}

export async function characterizeMedia() {
  await assertSourceContract();
  const contract = JSON.parse(await readFile(contractPath, "utf8"));
  const artifacts = [];

  for (const artifact of contract.artifacts) {
    if (artifact.os !== "windows") {
      artifacts.push(structuredClone(artifact));
      continue;
    }

    const locations = [];
    for (const location of artifact.locations) {
      const absolute = path.join(workspaceRoot, location.path);
      try {
        locations.push({ ...location, exists: true, sha256: await sha256(absolute) });
      } catch (error) {
        if (error?.code !== "ENOENT") throw error;
        locations.push({ ...location, exists: false, sha256: null });
      }
    }

    let versionOutput = "";
    const source = locations.find((location) => location.role === "vendor-source");
    if (process.platform === "win32" && source?.exists) {
      const run = spawnSync(path.join(workspaceRoot, source.path), ["-hide_banner", "-version"], {
        encoding: "utf8",
        windowsHide: true,
      });
      if (run.error || run.status !== 0) {
        throw new Error(`approved FFmpeg failed -version: ${run.error ?? `exit ${run.status}`}`);
      }
      versionOutput = `${run.stdout ?? ""}${run.stderr ?? ""}`;
    }

    artifacts.push({ ...structuredClone(artifact), locations, versionOutput });
  }

  return {
    ...contract,
    artifacts,
  };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const result = await characterizeMedia();
  const windows = result.artifacts.find((artifact) => artifact.os === "windows");
  const matching = windows.locations.every(
    (location) => location.exists && location.sha256 === windows.sha256,
  );
  console.log(`media characterization: Windows copies ${matching ? "MATCH" : "MISMATCH"} ${windows.sha256}`);
  console.log(`media characterization: gaps=${result.gaps.length}; rightkit-ffmpeg publish=BLOCKED`);
  if (!matching) process.exitCode = 1;
}
