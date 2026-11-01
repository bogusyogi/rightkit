import type { DocFormat } from "@rightkit/document-core/documents/types";

// Format badge — a solid colored file chip with the visible extension stacked vertically, so it
// stays legible at small sidebar sizes. Four-character labels use a deliberate 2x2 stack.
// Colors are distinct per format and dark enough for white badge text at small task/sidebar sizes.
const STYLE: Record<DocFormat, { label: string; color: string }> = {
  pdf: { label: "PDF", color: "#B83A2F" },
  md: { label: "MD", color: "#2F7D45" },
  mdx: { label: "MDX", color: "#7A5CAE" },
  html: { label: "HTML", color: "#B45424" },
  docx: { label: "DOCX", color: "#1F5FAE" },
  image: { label: "IMG", color: "#8A3B8E" },
  web: { label: "WEB", color: "#0F737A" },
  txt: { label: "TXT", color: "#68707A" },
  unknown: { label: "?", color: "#68707A" },
};

const EXTENSION_LABEL_ALIASES: Record<string, string> = {
  jpeg: "JPG",
  markdown: "MD",
  text: "TXT",
};

const EXTENSIONLESS_LABELS: Record<string, string> = {
  dockerfile: "DOCK",
  makefile: "MAKE",
};

const DOTFILE_LABELS: Record<string, string> = {
  ".env": "ENV",
  ".npmrc": "NPM",
  ".gitignore": "GIT",
  ".gitattributes": "GIT",
  ".editorconfig": "EDIT",
};

function badgeLabelFor(format: DocFormat, path?: string): string {
  if (!path || format === "web" || path.startsWith("untitled:"))
    return STYLE[format]?.label ?? STYLE.txt.label;
  const name = path.split(/[\\/]/).pop()?.toLowerCase() ?? "";
  const dotfile = DOTFILE_LABELS[name];
  if (dotfile) return dotfile;
  const extensionless = EXTENSIONLESS_LABELS[name];
  if (extensionless) return extensionless;
  const ext = name.includes(".") ? (name.split(".").pop() ?? "") : "";
  if (!ext) return STYLE[format]?.label ?? STYLE.txt.label;
  return (EXTENSION_LABEL_ALIASES[ext] ?? ext.toUpperCase()).slice(0, 4);
}

export function FileTypeIcon({
  format,
  path,
  size = 18,
}: {
  format: DocFormat;
  path?: string;
  size?: number;
}) {
  const s = STYLE[format] ?? STYLE.txt;
  const label = badgeLabelFor(format, path);
  const letters = label.split("");
  const w = size;
  const h = Math.round(size * 1.3); // a touch taller than wide — reads as a file/spine
  if (letters.length === 4) {
    const fontSize = Math.max(5, Math.min(size * 0.42, 8));
    return (
      <span
        aria-label={`${label} file`}
        role="img"
        style={{
          display: "inline-grid",
          gridTemplateColumns: "1fr 1fr",
          alignItems: "center",
          justifyItems: "center",
          width: w,
          height: h,
          background: s.color,
          borderRadius: Math.max(2, size * 0.16),
          color: "#fff",
          fontFamily: '"JetBrains Mono", ui-monospace, monospace',
          fontWeight: 800,
          lineHeight: 1,
          letterSpacing: 0,
          userSelect: "none",
          flexShrink: 0,
          padding: Math.max(1, Math.round(size * 0.12)),
        }}
      >
        {letters.map((c, i) => (
          <span key={i} style={{ fontSize }}>
            {c}
          </span>
        ))}
      </span>
    );
  }
  // Fit the stacked letters to the chip height, but never so big they crowd the width.
  const fontSize = Math.max(5, Math.min(((h - 3) / letters.length) * 0.86, w * 0.66));
  return (
    <span
      aria-label={`${label} file`}
      role="img"
      style={{
        display: "inline-flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        width: w,
        height: h,
        background: s.color,
        borderRadius: Math.max(2, size * 0.16),
        color: "#fff",
        fontFamily: '"JetBrains Mono", ui-monospace, monospace',
        fontWeight: 700,
        lineHeight: 1.02,
        letterSpacing: 0,
        userSelect: "none",
        flexShrink: 0,
      }}
    >
      {letters.map((c, i) => (
        <span key={i} style={{ fontSize }}>
          {c}
        </span>
      ))}
    </span>
  );
}
