import { hostAppName } from "../../../appName.js";
const escapeHtml = (value: string) =>
  value.replace(
    /[&<>"']/g,
    (char) =>
      ({
        "&": "&amp;",
        "<": "&lt;",
        ">": "&gt;",
        '"': "&quot;",
        "'": "&#39;",
      })[char] ?? char,
  );

const validLanguage = (value: string) =>
  /^[A-Za-z]{2,8}(?:-[A-Za-z0-9]{1,8})*$/.test(value) ? value : "en";

export function buildMarkdownStandaloneHtml(args: {
  fragment: string;
  title: string;
  description?: string;
  language?: string;
}): string {
  return `<!doctype html>
<html lang="${validLanguage(args.language ?? "en")}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="generator" content="${hostAppName()}">
<meta name="description" content="${escapeHtml(args.description ?? "")}">
<title>${escapeHtml(args.title)}</title>
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data: blob: https: http:; style-src 'unsafe-inline'; font-src data:; base-uri 'none'; form-action 'none'">
<style>html{color-scheme:light}body{margin:0;background:#fff}*,*::before,*::after{box-sizing:border-box}img,svg{max-width:100%;height:auto}pre{white-space:pre-wrap;overflow-wrap:anywhere}table{border-collapse:collapse}@media print{body{background:#fff}}</style>
</head>
<body>
${args.fragment}
</body>
</html>
`;
}
