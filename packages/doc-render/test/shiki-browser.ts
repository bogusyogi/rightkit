import { sanitizeShikiHtml, ShikiSanitizationError } from "../src/shikiSanitizer.js";

/** Called by shiki-browser.mjs after CDP network/execution observation starts. */
export function runShikiBrowserChecks() {
  const rootStyle = "--shiki-light:#123;--shiki-dark:#abcdef;--shiki-light-bg:#fff;--shiki-dark-bg:#000";
  const wrap = (body: string) => `<pre class="shiki shiki-themes github-light github-dark" style="${rootStyle}" tabindex="0"><code><span class="line">${body}</span></code></pre>`;
  const original = '<img src="https://shiki-invalid.example/source" onerror="window.__shikiPwned=1">';
  const failures = [
    wrap('<img src="https://shiki-invalid.example/image" onerror="window.__shikiPwned=1">'),
    wrap('<script>window.__shikiPwned=1</script>'),
    wrap('<svg><image href="https://shiki-invalid.example/svg"></image></svg>'),
    wrap('<iframe src="https://shiki-invalid.example/frame"></iframe>'),
    wrap('<span style="--shiki-light:url(https://shiki-invalid.example/css);--shiki-dark:#123">x</span>'),
  ];
  const host = document.createElement("main");
  host.style.cssText = "--shiki-light-font-style:italic;--shiki-dark-font-style:italic;--shiki-light-font-weight:bold;--shiki-dark-font-weight:bold;--shiki-light-text-decoration:underline;--shiki-dark-text-decoration:underline;font-style:italic;font-weight:bold";
  document.body.append(host);
  for (const input of failures) {
    const fallback = document.createElement("pre");
    try { fallback.innerHTML = sanitizeShikiHtml(input); throw new Error("Payload accepted"); }
    catch (error) {
      if (!(error instanceof ShikiSanitizationError)) throw error;
      fallback.textContent = original;
      fallback.dataset.notice = "Syntax highlighting could not render; showing source text.";
    }
    if (fallback.textContent !== original || fallback.childElementCount) throw new Error("Source fallback changed");
    host.append(fallback);
  }
  const surface = document.createElement("div");
  surface.innerHTML = sanitizeShikiHtml(wrap('<span>plain &lt;script&gt;</span><span style="--shiki-light:#aBcD;--shiki-dark:#12345678;--shiki-light-font-style:italic;--shiki-dark-font-style:inherit;--shiki-light-font-weight:inherit;--shiki-dark-font-weight:bold;--shiki-light-text-decoration:underline;--shiki-dark-text-decoration:line-through">font</span>'));
  host.append(surface);
  const root = surface.querySelector("pre")!;
  const [plain, decorated] = surface.querySelectorAll(".line > span");
  const assert = (pass: boolean, message: string) => { if (!pass) throw new Error(message); };
  for (const theme of ["light", "dark"]) {
    host.dataset.theme = theme;
    const text = getComputedStyle(plain);
    const fonts = getComputedStyle(decorated);
    const pre = getComputedStyle(root);
    assert(text.fontStyle === "normal" && text.fontWeight === "400" && text.textDecorationLine === "none", `Ambient font contamination: ${theme}`);
    assert(pre.backgroundColor === (theme === "light" ? "rgb(255, 255, 255)" : "rgb(0, 0, 0)"), `Root theme: ${theme}`);
    assert(text.color === (theme === "light" ? "rgb(17, 34, 51)" : "rgb(171, 205, 239)"), `Plain token theme: ${theme}`);
    assert(fonts.fontStyle === (theme === "light" ? "italic" : "normal"), `Font style: ${theme}`);
    assert(fonts.fontWeight === (theme === "light" ? "400" : "700"), `Font weight: ${theme}`);
    assert(fonts.textDecorationLine === (theme === "light" ? "underline" : "line-through"), `Decoration: ${theme}`);
  }
  host.dataset.theme = "light";
  const outside = document.createElement("pre");
  outside.className = "shiki";
  outside.style.fontStyle = "oblique";
  document.body.append(outside);
  assert(getComputedStyle(outside).fontStyle === "oblique", "Opt-in CSS affected ordinary .shiki");
  assert(!host.querySelector("img,svg,iframe,script"), "Executable/network elements mounted");
  return { rejected: failures.length, themes: ["light", "dark"], source: original };
}
