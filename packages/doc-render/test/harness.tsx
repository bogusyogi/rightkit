import { createRoot } from "react-dom/client";
import { HtmlRenderer, MarkdownRenderer } from "../src/renderers.js";
import "../src/styles.css";

function App() {
  return <main><MarkdownRenderer source={'# Docs\n\n[unsafe](javascript:alert(1)) [safe](https://example.com)'} /><HtmlRenderer html={'<p onclick="alert(1)">text</p><script>window.__xss=1</script><a href="javascript:alert(1)">bad</a><a href="https://example.com">good</a>'} /></main>;
}
createRoot(document.getElementById("root")!).render(<App />);
(window as unknown as { __harness?: boolean }).__harness = true;
