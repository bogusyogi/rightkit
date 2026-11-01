// QA-only harness page for /qa-media.html (dev server). Renders real
// MediaPlayer instances against on-disk fixture media streamed through
// /api/artifact — the same path the app uses in browser mode.
// The fixture folder is passed as an absolute path in the query string:
//   /qa-media.html?root=<absolute path to fixtures/qa-media>
// The dev route serves it only when the server runs with AI_MEDIA_CONSOLE_ALLOW_FIXTURES=1
// (scripts/dev-artifact-route.mjs header).
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import '../styles.css';
import { MediaPlayer } from '@rightkit/media-ui';
import '@rightkit/media-ui/styles.css';

const root = new URLSearchParams(window.location.search).get('root') ?? '';
const url = (name: string) => `/api/artifact?path=${encodeURIComponent(`${root}/${name}`)}`;

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <div style={{ padding: 24, display: 'grid', gap: 20, maxWidth: 720, fontFamily: 'system-ui' }}>
      <h1 style={{ fontSize: 18 }}>Media QA harness</h1>
      {root ? null : <p>Open this page with <code>?root=&lt;absolute path to fixtures/qa-media&gt;</code>. The dev server needs AI_MEDIA_CONSOLE_ALLOW_FIXTURES=1 to serve it.</p>}
      {root ? (
        <>
      <section>
        <h2 style={{ fontSize: 14 }}>Video (h264 + aac + vtt)</h2>
        <MediaPlayer src={url('clip.mp4')} kind="video" title="QA clip" info="320×180 · 4s" tracks={[{ src: url('subs.vtt'), label: 'English', srclang: 'en', default: true }]} />
      </section>
      <section>
        <h2 style={{ fontSize: 14 }}>Audio (wav)</h2>
        <MediaPlayer src={url('tone.wav')} kind="audio" title="QA tone" info="330Hz · 5s" />
      </section>
      <section>
        <h2 style={{ fontSize: 14 }}>Broken src (error path)</h2>
        <MediaPlayer src={url('missing.mp4')} kind="video" title="missing file" />
      </section>
        </>
      ) : null}
    </div>
  </StrictMode>
);
