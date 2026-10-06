// Headless-Chrome e2e harness. Bundled by browser-e2e.mjs (esbuild) and served
// over loopback; real media decodes for real in Chrome. Audio is muted by the
// launcher (--mute-audio); assertions read DOM/media-element state only.
import { createRoot } from "react-dom/client";
import { StrictMode } from "react";
import {
  ImageViewer,
  MediaAdaptersProvider,
  MediaPlayer,
  exportImageEdit,
  type MediaAdapters,
} from "../src/index.js";
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
  preparePlayback: async ({ id }) => ({ "bad.mkv": fx("clip.mp4") }[id] ?? fx("clip.mp4")),
  cancelPlayback: () => {},
};

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
