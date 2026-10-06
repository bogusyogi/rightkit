import { createRoot } from "react-dom/client";
import { useCallback, useState } from "react";
import { configurePdfJs, PdfViewer } from "../src/index.js";
import { HtmlRenderer, MarkdownRenderer } from "../../doc-render/src/renderers.js";
import "../../doc-render/src/styles.css";
import "../src/styles.css";

configurePdfJs({ workerSrc: "/pdf.worker.min.mjs" });

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
      toolbar={<button id="replace-source" type="button" onClick={() => setSource((value) => value === "report" ? "memo" : "report")}>Replace source</button>}
    />
    <section id="doc-fixtures">
      <MarkdownRenderer source={'# Shared renderer\n\n[unsafe](javascript:alert(1)) and [safe](https://example.com)'} />
      <HtmlRenderer html={'<p onclick="alert(1)">safe text</p><script>window.__xss = true</script><a href="javascript:alert(2)">bad</a><a href="https://example.com">good</a>'} />
    </section>
  </main>;
}

createRoot(document.getElementById("root")!).render(<App />);
(window as unknown as { __harness?: boolean }).__harness = true;
