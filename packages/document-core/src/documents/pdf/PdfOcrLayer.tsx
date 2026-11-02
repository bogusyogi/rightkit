import type { CSSProperties } from "react";
import type { OcrLine } from "./pdfOcr.js";

export function PdfOcrLayer({ lines }: { lines: OcrLine[] }) {
  return (
    <div
      role="group"
      aria-label="Recognized text"
      className="pdfOcrLayer"
      style={{
        position: "absolute",
        inset: 0,
        overflow: "hidden",
        // Above the page canvas. PdfViewer appends the canvas AFTER this layer and styles it
        // `absolute`, so at an equal z-index the opaque scan would paint over this transparent
        // text and swallow the pointer events that make it selectable — i.e. the recognized text
        // would exist in the DOM but be unusable, which is the entire point of the layer.
        zIndex: 2,
        pointerEvents: "none",
        lineHeight: 1,
      }}
    >
      {lines.map((line, index) => {
        const style: CSSProperties = {
          position: "absolute",
          left: `${line.rect.x * 100}%`,
          top: `${line.rect.y * 100}%`,
          width: `${line.rect.width * 100}%`,
          height: `${line.rect.height * 100}%`,
          color: "transparent",
          whiteSpace: "pre",
          userSelect: "text",
          pointerEvents: "auto",
          cursor: "text",
          overflow: "hidden",
        };
        return (
          <span key={`${index}-${line.text}`} style={style}>
            {line.text}
          </span>
        );
      })}
    </div>
  );
}
