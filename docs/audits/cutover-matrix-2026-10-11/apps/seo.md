# SEO cutover audit

Reviewed current source at `be14bf9b23f56e96c3b170b91436f12effacaa3a` in `/Volumes/D/claude/workflows/seo`; scope excludes sibling SEO operations checkout.

- Targets: Python CLI, Python stdio MCP, standalone Node render-gap CLI
- Cells: 97 total — 1 N, 96 NA, 0 Y, 0 M
- Shared routes: none

## Concrete gap

`npm:@rightkit/qa-chrome` is N. [render_gap.mjs](/Volumes/D/claude/workflows/seo/scripts/render_gap.mjs:146) owns local browser discovery, spawn, CDP-port setup & kill cleanup; [@rightkit/qa-chrome](/Volumes/D/claude/rightkit/packages/qa-chrome/src/index.ts:125) exports compatible Node resolver/launcher APIs. Adoption is optional; preserve SEO raw-CDP signal comparison.

## Runtime decisions

Rust crates & Swift packages are NA: current SEO targets have no Rust/Swift target, FFI, sidecar, or process bridge. Remaining Node packages are NA: source has no Node package manifest or `@rightkit/*` import. A supported host bridge could make selected owners reviewable later; this audit does not treat its absence as a required migration.

## Evidence

Published plugin launchers call `python3 seo.py mcp` in [.mcp.codex.json](/Volumes/D/claude/workflows/seo/.mcp.codex.json:3). [seo.py](/Volumes/D/claude/workflows/seo/seo.py:8) dispatches Python scripts; [mcp_server.py](/Volumes/D/claude/workflows/seo/scripts/mcp_server.py:75) hosts JSON-RPC stdio. JSON contains all 97 catalog rows, source searches & adversarial review.

