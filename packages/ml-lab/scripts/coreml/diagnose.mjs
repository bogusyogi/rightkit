// Why does arxiv-2512.12818 render at ~1s/page in pdf.js but 15ms in PDFKit?
//
// Per-page timing plus operator-list composition. If the cost concentrates in a
// few pages with heavy image or shading operators, it is a content pathology
// (potentially fixable in the pdf.js path). If it is spread evenly across
// ordinary text pages, it is pdf.js being slow at this document generally, and
// the native-renderer argument stands.

import { createCanvas } from "@napi-rs/canvas";
import * as pdfjs from "pdfjs-dist/legacy/build/pdf.mjs";

const { OPS } = pdfjs;
const opName = Object.fromEntries(Object.entries(OPS).map(([k, v]) => [v, k]));

const file = process.argv[2];
const maxPages = Number(process.argv[3] ?? 5);
const SCALE = 150 / 72;

const loadingTask = pdfjs.getDocument({
  url: file,
  isEvalSupported: false,
  standardFontDataUrl: "./node_modules/pdfjs-dist/standard_fonts/",
  cMapUrl: "./node_modules/pdfjs-dist/cmaps/",
  cMapPacked: true,
});
const doc = await loadingTask.promise;

console.log(`${file}  ${doc.numPages} pages`);
console.log("pg   render   getOps   ops  top operators");
console.log("-".repeat(78));

for (let p = 1; p <= Math.min(doc.numPages, maxPages); p++) {
  const page = await doc.getPage(p);
  const viewport = page.getViewport({ scale: SCALE });

  const tOps = performance.now();
  const list = await page.getOperatorList();
  const opsMs = performance.now() - tOps;

  const counts = {};
  for (const fn of list.fnArray) counts[opName[fn] ?? fn] = (counts[opName[fn] ?? fn] ?? 0) + 1;
  const top = Object.entries(counts)
    .sort((a, b) => b[1] - a[1])
    .slice(0, 4)
    .map(([k, v]) => `${k}:${v}`)
    .join(" ");

  const canvas = createCanvas(Math.ceil(viewport.width), Math.ceil(viewport.height));
  const ctx = canvas.getContext("2d");
  const t0 = performance.now();
  await page.render({ canvas, canvasContext: ctx, viewport }).promise;
  const renderMs = performance.now() - t0;

  console.log(
    `${String(p).padStart(2)}  ${renderMs.toFixed(0).padStart(6)}ms ${opsMs.toFixed(0).padStart(6)}ms ` +
      `${String(list.fnArray.length).padStart(5)}  ${top}`
  );
  page.cleanup();
}

await loadingTask.destroy();
