import { createRoot } from "react-dom/client";
import { useCallback, useState } from "react";
import { configurePdfJs, PdfViewer, usePdfSearch, type PdfSource } from "../src/index.js";
import { HtmlRenderer, MarkdownRenderer } from "../../doc-render/src/renderers.js";
import "../../doc-render/src/styles.css";
import "../src/styles.css";

declare global { interface Window { __sizes?: { alpha: number; omega: number } } }
configurePdfJs({ workerSrc: "/pdf.worker.min.mjs" });

/** Minimal valid one-page PDF with correct xref offsets; same text length => identical byte size. */
function makePdf(text: string): Uint8Array {
  const stream = `BT /F1 36 Tf 40 100 Td (${text}) Tj ET`;
  const objs = [
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
    `<< /Length ${stream.length} >>\nstream\n${stream}\nendstream`,
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
  ];
  let out = "%PDF-1.4\n"; const offsets: number[] = [];
  objs.forEach((body, i) => { offsets.push(out.length); out += `${i + 1} 0 obj\n${body}\nendobj\n`; });
  const xref = out.length;
  out += `xref\n0 ${objs.length + 1}\n0000000000 65535 f \n` + offsets.map((o) => `${String(o).padStart(10, "0")} 00000 n \n`).join("");
  out += `trailer\n<< /Size ${objs.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  return new TextEncoder().encode(out);
}

/** Consumer-owned toolbar control that uses the public search hook from the viewer's toolbar slot. */
function ExternalFind() {
  const search = usePdfSearch();
  return <span id="external-find">
    <input id="external-find-input" aria-label="External find" />
    <button id="external-find-go" type="button" onClick={() => { void search.find((document.getElementById("external-find-input") as HTMLInputElement).value); }}>Go</button>
    <button id="external-find-next" type="button" onClick={() => search.next()}>Next hit</button>
    <output id="external-find-result">{search.result.count}:{search.result.activeIndex}</output>
  </span>;
}

const ALPHA = makePdf("ALPHA");
const OMEGA = makePdf("OMEGA");
// Identical function source text, different captured documents.
const makeLoader = (bytes: Uint8Array) => async () => bytes;

function ReplaceApp() {
  const [bytesSource, setBytesSource] = useState<PdfSource>({ kind: "bytes", data: ALPHA });
  const [mode, setMode] = useState<"bytes" | "loader">("bytes");
  const [loaderSource, setLoaderSource] = useState<PdfSource>({ kind: "loader", load: makeLoader(ALPHA) });
  window.__sizes = { alpha: ALPHA.byteLength, omega: OMEGA.byteLength };
  return <main style={{ height: "100vh" }}>
    <button id="mode-loader" type="button" onClick={() => setMode("loader")}>Loader mode</button>
    <button id="swap-bytes" type="button" onClick={() => setBytesSource({ kind: "bytes", data: OMEGA })}>Replace bytes</button>
    <button id="swap-loader" type="button" onClick={() => setLoaderSource({ kind: "loader", load: makeLoader(OMEGA) })}>Replace loader</button>
    <div style={{ height: "calc(100vh - 40px)" }}><PdfViewer source={mode === "bytes" ? bytesSource : loaderSource} initialView={{ fit: null, scale: 1 }} /></div>
  </main>;
}

// Delimiter-collision pair: both used to key as `url:<origin>/fixtures/c:1:2`.
function CollideApp() {
  const [second, setSecond] = useState(false);
  const source: PdfSource = second
    ? { kind: "url", url: `${location.origin}/fixtures/c`, revision: "1:2" }
    : { kind: "url", url: `${location.origin}/fixtures/c:1`, revision: "2" };
  return <main style={{ height: "100vh" }}>
    <button id="swap-collide" type="button" onClick={() => setSecond(true)}>Switch to colliding source</button>
    <div style={{ height: "calc(100vh - 40px)" }}><PdfViewer source={source} initialView={{ fit: null, scale: 1 }} /></div>
  </main>;
}

function App() {
  const [source, setSource] = useState<"report" | "memo">("report");
  const onLoadingChange = useCallback((loading: boolean) => {
    const state = window as unknown as { __loadingEvents?: boolean[] };
    (state.__loadingEvents ??= []).push(loading);
  }, []);
  return <main style={{ height: "100vh", display: "grid", gridTemplateRows: "60% 40%" }}>
    <PdfViewer
      source={{ kind: "url", url: source === "report" ? "/fixtures/report.pdf" : "/fixtures/memo.pdf" }}
      initialView={{ fit: null, scale: .8 }}
      onLoadingChange={onLoadingChange}
      toolbar={<><ExternalFind /><button id="replace-source" type="button" onClick={() => setSource((value) => value === "report" ? "memo" : "report")}>Replace source</button></>}
    />
    <section id="doc-fixtures">
      <MarkdownRenderer source={'# Shared renderer\n\n[unsafe](javascript:alert(1)) and [safe](https://example.com)'} />
      <HtmlRenderer html={'<p onclick="alert(1)">safe text</p><script>window.__xss = true</script><a href="javascript:alert(2)">bad</a><a href="https://example.com">good</a>'} />
    </section>
  </main>;
}

createRoot(document.getElementById("root")!).render(({ replace: <ReplaceApp />, collide: <CollideApp /> } as Record<string, JSX.Element>)[new URLSearchParams(location.search).get("case") ?? ""] ?? <App />);
(window as unknown as { __harness?: boolean }).__harness = true;
