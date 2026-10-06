// Headless-Chrome e2e harness. Bundled by browser-e2e.mjs (esbuild) and served
// over loopback; real media decodes for real in Chrome. Audio is muted by the
// launcher (--mute-audio); assertions read DOM/media-element state only.
import { useState } from "react";
import { createRoot } from "react-dom/client";
import { StrictMode } from "react";
import {
  ImageViewer,
  MediaAdaptersProvider,
  MediaPlayer,
  exportImageEdit,
  type MediaAdapters,
} from "../src/index.js";
import { setImageEditSnapshot } from "../src/image/imageEditStore.js";
import { initialImageEdit } from "../src/index.js";
import "../styles.css";

declare global {
  interface Window {
    __harness?: { ready: boolean; exportResult?: unknown };
  }
}

const fx = (name: string) => `/fixtures/${name}`;

const adapters: MediaAdapters = {
  resolveSrc: (s) => s,
  // The "broken" player pretends its source can't decode; the compat fallback
  // returns the working clip, exactly like prepare_media_playback.
  preparePlayback: async ({ id }) => {
    // "slow.mkv" resolves late so a source swap can race the compat copy.
    if (id === "slow.mkv") await new Promise((r) => setTimeout(r, 1500));
    return fx("clip.mp4");
  },
  cancelPlayback: () => {},
};

// Source-replacement probe: ONE player instance (no remount) whose src/kind
// the test changes through window.__harness.swap.
type Swap = { src: string; kind: "video" | "audio"; playbackId?: string };
function SwapProbe() {
  const [cur, setCur] = useState<Swap>({ src: fx("clip.mp4"), kind: "video" });
  (window as unknown as { __swap: (s: Swap) => void }).__swap = setCur;
  return (
    <div data-probe="swap">
      <MediaPlayer src={cur.src} playbackId={cur.playbackId} kind={cur.kind} title="swap" />
    </div>
  );
}

// Preview/original probe: a 400x200 four-quadrant original with a 100x50
// display variant. The recipe is authored in preview pixels.
const QUAD = ["#ff0000", "#00ff00", "#0000ff", "#ffff00"]; // TL TR BL BR
function quadrantUrl(w: number, h: number): Promise<string> {
  const c = document.createElement("canvas");
  c.width = w;
  c.height = h;
  const g = c.getContext("2d")!;
  QUAD.forEach((color, i) => {
    g.fillStyle = color;
    g.fillRect((i % 2) * (w / 2), Math.floor(i / 2) * (h / 2), w / 2, h / 2);
  });
  return new Promise((res) => c.toBlob((b) => res(URL.createObjectURL(b!)), "image/png"));
}
const exportProbe: { fn?: (ext?: "png") => Promise<{ buffer: ArrayBuffer; mime: string }> } = {};
(window as unknown as { __exportProbe: typeof exportProbe }).__exportProbe = exportProbe;
function PreviewOriginalProbe() {
  const [urls, setUrls] = useState<{ orig: string; prev: string } | null>(null);
  if (!urls) {
    void Promise.all([quadrantUrl(400, 200), quadrantUrl(100, 50)]).then(([orig, prev]) => {
      setImageEditSnapshot("/probe/quad.png", {
        edit: { ...initialImageEdit(), crop: { x: 50, y: 0, width: 50, height: 25 } },
        undo: [],
        redo: [],
      });
      setUrls({ orig, prev });
    });
    return null;
  }
  return (
    <div style={{ height: 240 }} data-probe="quad">
      <ImageViewer
        path="/probe/quad.png"
        src={urls.orig}
        displaySrc={urls.prev}
        registerSave={(fn) => {
          exportProbe.fn = fn as typeof exportProbe.fn;
        }}
      />
    </div>
  );
}

function App() {
  return (
    <StrictMode>
      <MediaAdaptersProvider adapters={adapters}>
        <div style={{ padding: 16, display: "grid", gap: 16, width: 640 }}>
          <MediaPlayer
            src={fx("clip.mp4")}
            kind="video"
            title="clip"
            tracks={[{ src: fx("subs.vtt"), label: "English", srclang: "en", kind: "subtitles", default: true }]}
          />
          <MediaPlayer src={fx("tone.wav")} kind="audio" title="tone" />
          <MediaPlayer src={fx("bad.mkv")} playbackId="bad.mkv" kind="video" title="compat" />
          <SwapProbe />
          <PreviewOriginalProbe />
          <div style={{ height: 300 }}>
            <ImageViewer path="/tmp/picture.png" src={fx("picture.png")} readOnly />
          </div>
        </div>
      </MediaAdaptersProvider>
    </StrictMode>
  );
}

createRoot(document.getElementById("root")!).render(<App />);

// Export round-trip probe: crop + rotate + text overlay on the fixture, then
// decode the result and read pixels back for a content-level check.
exportImageEdit(
  fx("picture.png"),
  {
    crop: { x: 16, y: 16, width: 64, height: 32 },
    rotation: 90,
    flipX: false,
    flipY: false,
    adjustments: {
      brightness: 100,
      contrast: 100,
      saturation: 100,
      hue: 0,
      grayscale: 0,
      sepia: 0,
      blur: 0,
    },
    text: [
      {
        id: "t1",
        text: "RK",
        x: 0.5,
        y: 0.5,
        width: 60,
        size: 24,
        color: "#ffffff",
        bold: true,
        angle: 0,
      },
    ],
    resize: { mode: "original" },
  },
  "png",
)
  .then(async (payload) => {
    const bitmap = await createImageBitmap(new Blob([payload.buffer], { type: payload.mime }));
    const probe = new OffscreenCanvas(bitmap.width, bitmap.height);
    const ctx = probe.getContext("2d")!;
    ctx.drawImage(bitmap, 0, 0);
    const data = ctx.getImageData(0, 0, bitmap.width, bitmap.height).data;
    let nonRed = 0;
    for (let i = 0; i < data.length; i += 4) {
      if (!(data[i] > 200 && data[i + 1] < 60 && data[i + 2] < 60)) nonRed++;
    }
    window.__harness!.exportResult = {
      ok: true,
      bytes: payload.buffer.byteLength,
      width: bitmap.width,
      height: bitmap.height,
      nonRedPixels: nonRed,
    };
  })
  .catch((e) => {
    window.__harness!.exportResult = { ok: false, error: String(e) };
  });

window.__harness = { ready: true };
