import type {
  AuthStatus,
  CloudSyncStatus,
  DoctorReport,
  HistoryRow,
  JobEvent,
  JobInfo,
  Settings,
  TranscriptDetail,
  WhisperRuntimeStatus,
} from "../../contract";
import type { Tab } from "../../components/NavTabs";
import { qaModeEnabled } from "./qaGate";

type Unlisten = () => void;

const locationLike = typeof window === "undefined" ? { search: "", hash: "" } : window.location;

// Loaded only by initIpc in a QA build, so this gate is always open here; kept static to match ipc/index.ts.
export const isQaMode = qaModeEnabled(import.meta.env.VITE_RIGHTKIT_QA, locationLike.search);

function qaMediaPath(name: string): string {
  const root = new URLSearchParams(locationLike.search).get("qaMediaRoot");
  return isQaMode && root ? `${root.replace(/[\\/]$/, "")}/${name}` : `/qa/${name}`;
}

/** In-memory stand-in for the native licence (one QA page session): the view the shell's `rightkit_license_*` commands answer. */
let qaLicenseActive = false;

function qaLicenseView() {
  return {
    mode: "configured",
    status: qaLicenseActive ? "active" : "none",
    lifecycle: qaLicenseActive ? "lifetime" : "unlicensed",
    tier: qaLicenseActive ? "pro" : null,
    features: [],
    kind: qaLicenseActive ? "lifetime" : null,
    stage: qaLicenseActive ? "final" : null,
    validUntil: null,
    confirmationDueAt: null,
    staleSince: null,
    licenseIdSuffix: qaLicenseActive ? "QA01" : null,
    busy: false,
    unresolved: false,
    revisionState: qaLicenseActive ? "active" : "none",
    message: "none",
  };
}

export const qaState = (() => {
  const search = new URLSearchParams(locationLike.search);
  const fromQuery = search.get("state");
  if (fromQuery) return fromQuery;
  return locationLike.hash.replace(/^#\/?/, "") || "queue-default";
})();

export function getQaInitialState(): {
  tab: Tab;
  jobs: JobInfo[];
  detailId: string | null;
} {
  if (qaState.startsWith("library")) {
    return { tab: "library", jobs: qaJobs("queue-default"), detailId: null };
  }
  if (qaState.startsWith("settings")) {
    return { tab: "settings", jobs: qaJobs("queue-default"), detailId: null };
  }
  if (qaState === "detail-carousel") {
    return { tab: "queue", jobs: qaJobs("queue-default"), detailId: "qa-carousel-done" };
  }
  if (qaState === "detail-truncated") {
    return { tab: "queue", jobs: qaJobs("detail-truncated"), detailId: "qa-truncated-done" };
  }
  if (qaState.startsWith("detail-player")) {
    // WS-7: open straight onto the audio+transcript item whose saved media is
    // the bundled QA clip — the player streams it from the preview server.
    return {
      tab: "queue",
      jobs: qaJobs("queue-media-download"),
      detailId: "qa-youtube-audio-both",
    };
  }
  if (qaState.startsWith("detail")) {
    return { tab: "queue", jobs: qaJobs("queue-default"), detailId: "qa-youtube-done" };
  }
  const jobs = qaJobs(qaState);
  const selected = [
    "queue-hover",
    "queue-selected",
    "queue-error",
    "queue-attachments",
    "queue-auth",
  ].includes(qaState)
    ? jobs[0]?.id
    : ["queue-default", "queue-transcript"].includes(qaState)
      ? jobs.find((job) => job.stage === "done")?.id
      : null;
  return { tab: "queue", jobs, detailId: selected ?? null };
}

export async function qaInvoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  switch (command) {
    case "get_jobs":
      return qaJobs(qaState) as T;
    case "add_jobs":
      return qaJobs("queue-processing") as T;
    case "add_pasted_image":
      return [qaImageJob] as T;
    case "submit_profile_images":
      return {
        profile: "instagram.com/qa.profile",
        limit: 50,
        found: 2,
        accepted: 2,
        job_ids: ["qa-images-done", "qa-images-done-2"],
        rejected: [],
      } as T;
    case "submit_playlist":
      return {
        playlist: "youtube.com/@qa.channel",
        limit: 50,
        found: 3,
        accepted: 3,
        job_ids: ["qa-playlist-1", "qa-playlist-2", "qa-playlist-3"],
        rejected: [],
      } as T;
    case "get_history_page":
      return {
        rows: qaState === "queue-empty" ? [] : qaHistoryForState(),
        next_cursor: null,
      } as T;
    case "prepare_media_playback":
      if (qaState === "queue-media-download" || qaState === "detail-player-corrupt")
        throw new Error("PLAYBACK_MISSING: saved media is no longer on disk");
      return qaMediaPath("qa-tone.m4a") as T;
    case "cancel_media_playback":
      return undefined as T;
    case "get_transcript":
      return qaTranscript(String(args?.id ?? "qa-youtube-done")) as T;
    case "get_transcript_variant": {
      const full = qaTranscript(String(args?.id ?? "qa-youtube-done"));
      const kind = String(args?.kind ?? "text");
      const text = (full as unknown as Record<string, unknown>)[kind];
      return { id: full.id, kind, text: typeof text === "string" ? text : null } as T;
    }
    case "get_settings":
      return qaSettings as T;
    case "get_settings_snapshot":
      return { settings: qaSettings, revision: "0" } as T;
    case "set_settings":
      return "0" as T;
    case "auth_status":
    case "test_auth":
      return qaAuthStatus(String(args?.platform ?? "instagram")) as T;
    case "cancel_job":
    case "delete_transcript":
    case "remove_job":
    case "open_file":
    case "open_media_file":
    case "reveal_in_folder":
    case "reveal_media_in_folder":
    case "open_source_url":
    case "save_transcript_as":
    case "sign_in":
    case "open_auth_url_in_browser":
    case "release_auth_browser":
    case "import_auth_cookies":
    case "import_auth_from_browser":
    case "clear_auth":
      return undefined as T;
    case "doctor":
      return qaDoctor as T;
    case "whisper_runtime_status":
    case "install_whisper_runtime":
      return qaRuntimeStatus() as T;
    case "rightkit_license_status":
      return qaLicenseView() as T;
    case "rightkit_license_activate":
      qaLicenseActive = true;
      return qaLicenseView() as T;
    case "rightkit_license_deactivate":
      qaLicenseActive = false;
      return qaLicenseView() as T;
    case "rightkit_license_refresh":
      return { view: qaLicenseView(), outcome: qaLicenseActive ? "refreshed" : "noCredential" } as T;
    case "rightkit_license_request_trial":
      return { accepted: true, message: "trialRequested" } as T;
    case "cloud_sync_status":
      return qaCloudSync as T;
    case "set_cloud_sync_enabled":
      qaCloudSync.enabled = Boolean((args as { enabled?: boolean } | undefined)?.enabled);
      qaCloudSync.state = qaCloudSync.enabled ? "ready" : "off";
      return { ...qaCloudSync } as T;
    case "cloud_sync_now":
      return undefined as T;
    case "cloud_sync_grant_consent":
      return { ...qaCloudSync } as T;
    case "check_for_updates":
      if (qaState === "settings-update-patch") {
        return { kind: "available", version: "0.1.2", tier: "patch", eligible: true } as T;
      }
      if (qaState === "settings-update-feature") {
        return {
          kind: "available",
          version: "0.2.0",
          tier: "update",
          eligible: false,
          reason: "pro_required",
        } as T;
      }
      if (qaState === "settings-update-pro") {
        return { kind: "available", version: "0.2.0", tier: "update", eligible: true } as T;
      }
      return { kind: "up_to_date" } as T;
    case "install_available_update":
      if (qaState === "settings-update-pro") {
        return { kind: "ready", version: "0.2.0", tier: "update" } as T;
      }
      return { kind: "ready", version: "0.1.1", tier: "patch" } as T;
    default:
      throw new Error(`QA mock missing invoke handler: ${command}`);
  }
}

export async function qaListen<T>(
  event: string,
  handler: (event: { payload: T }) => void,
): Promise<Unlisten> {
  if (event === "job://update" && qaState === "queue-live-progress") {
    const timer = window.setTimeout(() => {
      handler({
        payload: {
          id: "qa-instagram-processing",
          stage: "downloading",
          pct: 42,
          msg: "capturing carousel slide",
          error: null,
          error_kind: null,
          needs_auth: null,
        } satisfies JobEvent as T,
      });
    }, 500);
    return () => window.clearTimeout(timer);
  }
  if (event === "job://update" && qaState === "queue-terminal-race") {
    const timer = window.setTimeout(() => {
      handler({
        payload: {
          id: "qa-terminal-race",
          stage: "downloading",
          pct: 42,
          error: null,
          error_kind: null,
          needs_auth: null,
        } satisfies JobEvent as T,
      });
    }, 500);
    return () => window.clearTimeout(timer);
  }
  return () => {};
}

export function qaCurrentWebview() {
  return {
    onDragDropEvent: async () => () => {},
  };
}

export function qaCurrentWindow() {
  return {
    minimize: async () => {},
    toggleMaximize: async () => {},
    close: async () => {},
  };
}

export async function qaOpenDialog() {
  return ["C:\\App\\qa\\sample-carousel.png"];
}

const qaImageJob: JobInfo = {
  id: "qa-image-queued",
  input: "C:\\App\\qa\\sample-carousel.png",
  kind: "local_file",
  stage: "queued",
  pct: 0,
  title: "Pasted image",
  error: null,
  error_kind: null,
  needs_auth: null,
};

function qaJobs(state: string): JobInfo[] {
  if (state === "queue-empty") return [];

  if (state === "queue-terminal-race") {
    return [
      {
        id: "qa-terminal-race",
        input: "https://www.youtube.com/watch?v=qa-terminal-race",
        kind: "youtube",
        stage: "done",
        pct: 100,
        title: "Terminal race demo",
        error: null,
        error_kind: null,
        needs_auth: null,
      },
    ];
  }

  // The `detail-player` state opens on the audio+transcript fixture below;
  // `get_jobs` must return the same rows the initial state seeded or the
  // workspace drops the selected source.
  if (state === "detail-player") return qaJobs("queue-media-download");

  // `detail-truncated` opens on the Free-cap transcript — it needs a done
  // queue row with the same id or selection falls back to the first row.
  if (state === "detail-truncated") {
    return [
      {
        id: "qa-truncated-done",
        input: "https://www.youtube.com/watch?v=qa-long",
        kind: "youtube",
        stage: "done",
        pct: 100,
        title: "Long lecture transcript (Free cap)",
        error: null,
        error_kind: null,
        needs_auth: null,
      },
      ...qaJobs("queue-default"),
    ];
  }

  if (state === "queue-media-download") {
    // WS-4 sidebar-type coverage: audio-only, video-only, and video +
    // transcript rows — each with a matching history row carrying the
    // persisted `media_action`.
    return [
      {
        id: "qa-youtube-audio-download",
        input: "https://www.youtube.com/watch?v=qa-audio",
        kind: "youtube",
        stage: "done",
        pct: 100,
        title: "Downloaded audio demo",
        error: null,
        error_kind: null,
        needs_auth: null,
        media: { action: "download", video: null, audio: "best" },
      },
      {
        id: "qa-youtube-video-download",
        input: "https://www.youtube.com/watch?v=qa-video",
        kind: "youtube",
        stage: "done",
        pct: 100,
        title: "Downloaded video demo",
        error: null,
        error_kind: null,
        needs_auth: null,
        media: { action: "download", video: "p1080", audio: null },
      },
      {
        id: "qa-youtube-both-download",
        input: "https://www.youtube.com/watch?v=qa-both",
        kind: "youtube",
        stage: "done",
        pct: 100,
        title: "Video plus transcript demo",
        error: null,
        error_kind: null,
        needs_auth: null,
        media: { action: "both", video: "p1080", audio: null },
      },
      {
        // WS-7: transcript row whose saved media is the bundled QA clip, so
        // the in-reader player has a real streamable source in headless QA.
        id: "qa-youtube-audio-both",
        input: "https://www.youtube.com/watch?v=qa-both-audio",
        kind: "youtube",
        stage: "done",
        pct: 100,
        title: "Audio plus transcript demo",
        error: null,
        error_kind: null,
        needs_auth: null,
        media: { action: "both", video: null, audio: "best" },
      },
    ];
  }

  // Regression guard for long, unbreakable titles (a pasted URL with no spaces)
  // overflowing the preview header / pushing actions off-frame.
  if (state === "queue-longtitle") {
    return [
      {
        id: "qa-youtube-done",
        input:
          "https://www.instagram.com/p/DZAFFV-k6xb/?igsh=MXNqdzN1d3Z2cTh1cQ==&utm_source=ig_web_copy_link&extra=padding-to-make-this-really-long",
        kind: "instagram_post",
        stage: "done",
        pct: 100,
        title: null,
        error: null,
        error_kind: null,
        needs_auth: null,
      },
    ];
  }

  if (state === "queue-display-label") {
    return [
      {
        id: "qa-instagram-post-done",
        input: "https://www.instagram.com/p/post-label-demo/",
        kind: "instagram_post",
        stage: "done",
        pct: 100,
        title: "Post label demo",
        error: null,
        error_kind: null,
        needs_auth: null,
      },
      ...qaJobs("queue-default"),
    ];
  }

  const base: JobInfo[] = [
    {
      id: "qa-instagram-processing",
      input: "https://www.instagram.com/p/DZAFFV-k6xb/",
      kind: "instagram_post",
      stage: "downloading",
      pct: state === "queue-live-progress" ? 20 : 47,
      title: null,
      error: null,
      error_kind: null,
      needs_auth: null,
    },
    qaImageJob,
    {
      id: "qa-youtube-done",
      input: "https://www.youtube.com/watch?v=qa-demo",
      kind: "youtube",
      stage: "done",
      pct: 100,
      title: "Timestamped transcript demo",
      error: null,
      error_kind: null,
      needs_auth: null,
    },
    {
      id: "qa-instagram-auth",
      input: "https://www.instagram.com/p/private-demo/",
      kind: "instagram_post",
      stage: "error",
      pct: 0,
      title: null,
      error: "Instagram sign-in required",
      error_kind: "needs_auth",
      needs_auth: "Instagram",
    },
  ];

  if (state === "queue-auth") {
    return [
      {
        id: "qa-instagram-auth-timeout",
        input: "https://www.instagram.com/p/DZY-FHxnwIP/?img_index=4&igsh=b3Q4aWNvYzhjbmZ3",
        kind: "instagram_post",
        stage: "error",
        pct: 0,
        title: null,
        error: "Sign in to Instagram to continue",
        error_kind: "needs_auth",
        needs_auth: "Instagram",
      },
      ...base,
    ];
  }

  if (state === "queue-attachments") {
    return [
      {
        id: "qa-local-video",
        input: "D:\\clips\\launch-demo.mov",
        kind: "local_file",
        stage: "transcribing",
        pct: 64,
        title: "launch-demo.mov",
        error: null,
        error_kind: null,
        needs_auth: null,
      },
      ...base,
    ];
  }

  if (state === "queue-error") {
    return [
      {
        id: "qa-failed-download",
        input: "https://www.instagram.com/p/no-media-demo/",
        kind: "instagram_post",
        stage: "error",
        pct: 20,
        title: null,
        error: "yt-dlp succeeded but no media file found in work_dir",
        error_kind: "no_media",
        needs_auth: null,
      },
      ...base,
    ];
  }

  return base;
}

const qaHistory: HistoryRow[] = [
  {
    id: "qa-youtube-done",
    url: "https://www.youtube.com/watch?v=qa-demo",
    kind: "youtube",
    title: "Timestamped transcript demo",
    lang: "en",
    duration_s: 383,
    created_at: "2026-06-01T17:30:00Z",
    txt_path: "C:\\App\\transcripts\\demo.txt",
    md_path: "C:\\App\\transcripts\\demo.md",
    srt_path: "C:\\App\\transcripts\\demo.srt",
    slide_dir: null,
    snippet: "A compact transcript entry with timestamped exports and a clean preview.",
  },
  {
    id: "qa-youtube-audio-download",
    url: "https://www.youtube.com/watch?v=qa-audio",
    kind: "youtube",
    title: "Downloaded audio demo",
    lang: "en",
    duration_s: 241,
    created_at: "2026-06-01T17:02:00Z",
    txt_path: null,
    md_path: null,
    srt_path: null,
    slide_dir: null,
    snippet: null,
    media_path:
      "C:\\Users\\QA\\Downloads\\App\\Downloaded audio demo [qa-youtube-audio-download].m4a",
    media_meta: '{"bytes":12345,"container":"m4a","kind":"audio","quality":"best"}',
    media_action: "download_audio",
  },
  {
    id: "qa-youtube-video-download",
    url: "https://www.youtube.com/watch?v=qa-video",
    kind: "youtube",
    title: "Downloaded video demo",
    lang: "en",
    duration_s: 4022,
    created_at: "2026-06-01T16:48:00Z",
    txt_path: null,
    md_path: null,
    srt_path: null,
    slide_dir: null,
    snippet: null,
    media_path:
      "C:\\Users\\QA\\Downloads\\App\\Downloaded video demo [qa-youtube-video-download].mkv",
    media_meta: '{"bytes":987654,"container":"mkv","kind":"video","quality":"p1080"}',
    media_action: "download_video",
  },
  {
    id: "qa-youtube-both-download",
    url: "https://www.youtube.com/watch?v=qa-both",
    kind: "youtube",
    title: "Video plus transcript demo",
    lang: "en",
    duration_s: 1500,
    created_at: "2026-06-01T16:31:00Z",
    txt_path: "C:\\App\\transcripts\\both.txt",
    md_path: "C:\\App\\transcripts\\both.md",
    srt_path: "C:\\App\\transcripts\\both.srt",
    slide_dir: null,
    snippet: "A download + transcript row covering the combined media action.",
    media_path:
      "C:\\Users\\QA\\Downloads\\App\\Video plus transcript demo [qa-youtube-both-download].mkv",
    media_meta: '{"bytes":456789,"container":"mkv","kind":"video","quality":"p1080"}',
    media_action: "both_video",
  },
  {
    id: "qa-carousel-done",
    url: "https://www.instagram.com/p/DZAFFV-k6xb/",
    kind: "carousel",
    title: "Carousel OCR capture",
    lang: "en",
    duration_s: null,
    created_at: "2026-06-01T16:12:00Z",
    txt_path: "C:\\App\\transcripts\\carousel.txt",
    md_path: "C:\\App\\transcripts\\carousel.md",
    srt_path: null,
    slide_dir: "C:\\App\\transcripts\\carousel-slides",
    snippet: "Caption text plus per-slide OCR output saved as transcript markdown.",
  },
  {
    // #248 image job: engine shape is kind carousel + download_images, with
    // the saved files described in media_meta and listed by slide_dir.
    id: "qa-images-done",
    url: "https://www.instagram.com/p/DZIMAGES01/",
    kind: "carousel",
    title: "Image post save",
    lang: null,
    duration_s: null,
    created_at: "2026-06-02T10:05:00Z",
    txt_path: null,
    md_path: null,
    srt_path: null,
    slide_dir: "C:\\App\\downloads\\images\\qa-images-done",
    snippet: "2 images",
    media_path: "C:\\App\\downloads\\images\\qa-images-done\\image-01.jpg",
    media_meta:
      '{"kind":"images","count":2,"bytes":240000,"files":["C:\\\\App\\\\downloads\\\\images\\\\qa-images-done\\\\image-01.jpg","C:\\\\App\\\\downloads\\\\images\\\\qa-images-done\\\\image-02.jpg"]}',
    media_action: "download_images",
  },
];

function qaHistoryForState(): HistoryRow[] {
  if (qaState !== "queue-dedup") return qaHistory;
  return [
    ...qaHistory,
    {
      ...qaHistory[0],
      title: "Duplicate history title must stay hidden",
      snippet: "Same ID as queue-owned transcript",
    },
  ];
}

const qaSettings: Settings = {
  output_dir: "C:\\App\\transcripts",
  downloads_dir: "C:\\App\\downloads",
  cookies_path: "C:\\App\\instagram_cookies.txt",
  ort_dylib_path: "C:\\App\\runtime\\onnxruntime.dll",
  keep_media: false,
  article_fit_copy: true,
  model_idle_minutes: 1,
  model_dir: "C:\\App\\models\\asr\\parakeet_tdt_v3_static1500_qint8_20260722",
};

const qaDoctor: DoctorReport = {
  directml: true,
  model_ok: true,
  ort_dylib_ok: true,
  ytdlp: true,
  ffmpeg: true,
  cookies_present: false,
  notes: ["QA mock: deterministic doctor surface without probing local tools."],
  asr_engine: "parakeet_tdt",
  active_asr_backend: "parakeet_tdt",
  asr_timings_supported: true,
  tdt_model_ok: true,
  article_backends: ["http", "browser-header", "tls-fingerprint", "browser", "fit-filter"],
  ocr_available: true,
  supported_modalities: [
    "youtube",
    "instagram_reel",
    "instagram_carousel",
    "article",
    "image_ocr",
    "pdf",
    "threads",
    "x",
    "tiktok",
  ],
  routing_note: "locked Parakeet TDT v3; no fallback",
};

const qaWhisperRuntime: WhisperRuntimeStatus = {
  os: "windows",
  arch: "x86_64",
  gpu_names: [],
  preferred_backends: ["directml"],
  // Whisper Pro add-on absent: install row renders gated under the Free mock
  // license ("Requires the host app Pro").
  installed_backend: null,
  cli_path: null,
  runtime_root: "C:\\App\\runtime\\whisper",
  model_ok: false,
  model_path: "C:\\App\\models\\whisper",
  tdt_model_ok: true,
  tdt_model_path: "C:\\App\\models\\asr\\parakeet_tdt_v3_static1500_qint8_20260722",
  tdt_has_timing: true,
};

function qaRuntimeStatus(): WhisperRuntimeStatus {
  if (qaState === "settings-runtime-macos") {
    return {
      ...qaWhisperRuntime,
      os: "macos",
      arch: "aarch64",
      preferred_backends: ["coreml-ane"],
      runtime_root: "/Users/qa/Library/Application Support/App/runtime/whisper",
      model_path: "/Users/qa/Library/Application Support/App/models/whisper",
      tdt_model_path: "/Users/qa/Library/Application Support/RightSuite/models/parakeet-tdt-v3",
    };
  }
  if (qaState === "settings-runtime-heardright") {
    return {
      ...qaWhisperRuntime,
      model_ok: false,
      tdt_model_ok: false,
      tdt_model_path: "C:\\Users\\qa\\AppData\\Local\\App\\models\\parakeet-tdt-v3",
    };
  }
  if (qaState === "settings-runtime-missing") {
    return {
      ...qaWhisperRuntime,
      model_ok: true,
      tdt_model_ok: false,
      tdt_model_path: "C:\\Users\\qa\\AppData\\Local\\RightSuite\\models\\parakeet-tdt-v3",
    };
  }
  return qaWhisperRuntime;
}

/** Mutable so the Settings toggle can flip it and read back the new state. */
const qaCloudSync: CloudSyncStatus = {
  enabled: false,
  state: "off",
  detail: null,
  supported: true,
};

function qaAuthStatus(platform: string): AuthStatus {
  const isThreads = platform.toLowerCase() === "threads";
  return {
    platform: isThreads ? "Threads" : "Instagram",
    profile_dir: `C:\\App\\auth\\${isThreads ? "threads" : "instagram"}`,
    profile_present: !isThreads,
    exported_cookies_path: isThreads ? null : "C:\\App\\auth\\instagram_cookies.txt",
    exported_cookies_present: !isThreads,
    last_error: isThreads ? "Threads session not found" : null,
  };
}

function qaTranscript(id: string): TranscriptDetail {
  if (id === "qa-images-done") {
    return {
      id,
      url: "https://www.instagram.com/p/DZIMAGES01/",
      kind: "carousel",
      title: "Image post save",
      text: "",
      md_text: null,
      timed_text: null,
      timed_md_text: null,
      srt: null,
      slides: ["/qa/slide-01.png", "/qa/slide-02.png"],
      media_path: "C:\\App\\downloads\\images\\qa-images-done\\image-01.jpg",
      media_meta:
        '{"kind":"images","count":2,"bytes":240000,"files":["/qa/slide-01.png","/qa/slide-02.png"]}',
      media_action: "download_images",
    };
  }

  if (id === "qa-youtube-audio-download") {
    return {
      id,
      url: "https://www.youtube.com/watch?v=qa-audio",
      kind: "youtube",
      title: "Downloaded audio demo",
      text: "",
      md_text: null,
      timed_text: null,
      timed_md_text: null,
      srt: null,
      slides: [],
      media_path:
        "C:\\Users\\QA\\Downloads\\App\\Downloaded audio demo [qa-youtube-audio-download].m4a",
      media_meta: '{"bytes":12345,"container":"m4a","kind":"audio","quality":"best"}',
      media_action: "download_audio",
    };
  }

  if (id === "qa-truncated-done") {
    // Free-tier cap: transcript stops at 30 minutes, banner offers Pro. The
    // timed surfaces are still populated — Free keeps the SRT on the history
    // record, so the reader's timestamp toggle works without timed files.
    return {
      id,
      url: "https://www.youtube.com/watch?v=qa-long",
      kind: "youtube",
      title: "Long lecture transcript (Free cap)",
      text: "App keeps queue work visible, exports clean transcript files, and avoids foreground automation during routine QA.",
      md_text:
        "# Long lecture transcript (Free cap)\n\nHost keeps queue work visible and exports clean transcript files.",
      timed_text:
        "[00:00:00] App keeps queue work visible.\n[00:00:14] Timestamped text and Markdown exports are available.",
      timed_md_text:
        "- [00:00:00] App keeps queue work visible.\n- [00:00:14] Timestamped text and Markdown exports are available.",
      srt: "1\n00:00:00,000 --> 00:00:04,000\nHost keeps queue work visible.\n\n2\n00:00:14,000 --> 00:00:18,000\nTimestamped exports are available.",
      slides: [],
      truncated_at_s: 1800,
    };
  }

  if (id === "qa-youtube-audio-both") {
    // WS-7: transcript + playable saved media. `media_path` is the bundled
    // clip under /qa/ — MediaPlayer keeps it root-relative instead of running
    // it through convertFileSrc, so the preview server can stream it.
    return {
      id,
      url: "https://www.youtube.com/watch?v=qa-both-audio",
      kind: "youtube",
      title: "Audio plus transcript demo",
      text: "The queue keeps saved audio alongside the transcript. Playback and seeking stay inside the reader. Timestamped lines seek the saved media.",
      md_text:
        "# Audio plus transcript demo\n\nThe queue keeps saved audio alongside the transcript.",
      timed_text:
        "[00:00:00] The queue keeps saved audio alongside the transcript.\n[00:00:04] Playback and seeking stay inside the reader.\n[00:00:08] Timestamped lines seek the saved media.",
      timed_md_text:
        "- [00:00:00] The queue keeps saved audio alongside the transcript.\n- [00:00:04] Playback and seeking stay inside the reader.\n- [00:00:08] Timestamped lines seek the saved media.",
      srt: "1\n00:00:00,000 --> 00:00:03,500\nThe queue keeps saved audio alongside the transcript.\n\n2\n00:00:04,000 --> 00:00:07,500\nPlayback and seeking stay inside the reader.\n\n3\n00:00:08,000 --> 00:00:11,500\nTimestamped lines seek the saved media.",
      slides: [],
      media_path:
        qaState === "detail-player-video"
          ? qaMediaPath("qa-video.mp4")
          : qaState === "detail-player-recovery" || qaState === "detail-player-corrupt"
            ? "/qa/missing.m4a"
            : qaMediaPath("qa-tone.m4a"),
      media_meta:
        qaState === "detail-player-video"
          ? '{"kind":"video","container":"mp4"}'
          : '{"bytes":39118,"container":"m4a","kind":"audio","quality":"best"}',
      media_action: "both_audio",
    };
  }

  if (id === "qa-carousel-done") {
    return {
      id,
      url: "https://www.instagram.com/p/DZAFFV-k6xb/",
      kind: "carousel",
      title: "Carousel OCR capture",
      text: "[caption]\nA compact design teardown.\n\n[slide 1]\nQueue work stays visible.\n\n[slide 2]\nExports include clean OCR text.",
      md_text:
        "# Carousel OCR capture\n\n[caption]\nA compact design teardown.\n\n[slide 1]\nQueue work stays visible.\n\n[slide 2]\nExports include clean OCR text.",
      timed_text: null,
      timed_md_text: null,
      srt: null,
      slides: ["/qa/slide-01.png", "/qa/slide-02.png"],
    };
  }

  return {
    id,
    url: "https://www.youtube.com/watch?v=qa-demo",
    kind: "youtube",
    title: "Timestamped transcript demo",
    text: "App keeps queue work visible, exports clean transcript files, and avoids foreground automation during routine QA.",
    md_text:
      "# Timestamped transcript demo\n\nHost keeps queue work visible and exports clean transcript files.",
    timed_text:
      "[00:00:00] App keeps queue work visible.\n[00:00:14] Timestamped text and Markdown exports are available.",
    timed_md_text:
      "- [00:00:00] App keeps queue work visible.\n- [00:00:14] Timestamped text and Markdown exports are available.",
    srt: "1\n00:00:00,000 --> 00:00:04,000\nHost keeps queue work visible.\n\n2\n00:00:14,000 --> 00:00:18,000\nTimestamped exports are available.",
    slides: [],
  };
}
