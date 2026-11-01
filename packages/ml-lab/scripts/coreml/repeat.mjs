// Is page 5's cost one-time (cacheable font/glyph compile) or paid per render?
// A one-time cost can be hidden by prewarming; a recurring one cannot.
import { createCanvas } from "@napi-rs/canvas";
import * as pdfjs from "pdfjs-dist/legacy/build/pdf.mjs";
const loadingTask = pdfjs.getDocument({ url: process.argv[2], isEvalSupported: false,
  standardFontDataUrl: "./node_modules/pdfjs-dist/standard_fonts/",
  cMapUrl: "./node_modules/pdfjs-dist/cmaps/", cMapPacked: true });
const doc = await loadingTask.promise;
const pageNo = Number(process.argv[3]);
const page = await doc.getPage(pageNo);
const viewport = page.getViewport({ scale: 150 / 72 });
for (let run = 1; run <= 3; run++) {
  const canvas = createCanvas(Math.ceil(viewport.width), Math.ceil(viewport.height));
  const t0 = performance.now();
  await page.render({ canvas, canvasContext: canvas.getContext("2d"), viewport }).promise;
  console.log(`  page ${pageNo} run ${run}: ${(performance.now() - t0).toFixed(0)}ms`);
}
await loadingTask.destroy();
