// H.264 encoders the bundled ffmpeg can emit for Motion renders. Mirrors
// `choose()` in src-tauri/src/motion/encode.rs (h264 = true candidates).
const H264_ENCODERS: ReadonlySet<string> = new Set(['h264_videotoolbox', 'h264_mf', 'libopenh264']);

// True when a render's encoder name produced H.264, which the webview <video>
// element plays. Matches the exact name: `libopenh264` is H.264 without the
// `h264` prefix. Any other name (e.g. `mpeg4`) is not playable in-app.
export function isH264Encoder(name: string | null | undefined): boolean {
  return name != null && H264_ENCODERS.has(name);
}
