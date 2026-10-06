import { HtmlRenderer, MarkdownRenderer } from "../src/renderers.js";

/** Slash / no-whitespace attribute separators that defeat whitespace-based regex sanitizers. */
export const SSR_PAYLOADS = [
  '<img src="x"/onerror="window.__pwned=1">',
  "<img/src=x/onerror=window.__pwned=1>",
  "<img src=x/onerror=window.__pwned=1>",
  '<img src="x"\u000conerror="window.__pwned=1">',
  '<svg/onload="window.__pwned=1">',
  '<details/open/ontoggle="window.__pwned=1">',
];

export function App() {
  return (
    <main>
      <MarkdownRenderer source={'# Docs\n\n[unsafe](javascript:alert(1)) [safe](https://example.com)\n\n' + SSR_PAYLOADS.join("\n\n")} />
      <HtmlRenderer html={'<p onclick="alert(1)">text</p><script>window.__xss=1</script><a href="javascript:alert(1)">bad</a><a href="https://example.com">good</a>'} />
      {SSR_PAYLOADS.map((payload, i) => <HtmlRenderer key={i} html={payload} />)}
    </main>
  );
}
