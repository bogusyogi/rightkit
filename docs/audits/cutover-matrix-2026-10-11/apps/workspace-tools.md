# Workspace Tools cutover audit

Fresh scoped source review covers 326 Node, Python, shell & Remotion files under `/Volumes/D/claude/tools`, excluding `rhook`, dependencies, output, caches, assets, fixtures, tests & generated reports. No active `@rightkit/*` production import or caller exists.

- `npm:@rightkit/theme` **N** — Remotion consumes `brand-palettes.ts`; evaluate shared token export while preserving animation/typography policy.

Counts: 0 Y, 1 N, 0 M, 96 NA. Theme is only compatible active overlap; Cargo & Swift APIs are runtime-incompatible for scoped Node/Python/Remotion toolkit, while AX, build-control, hooks & QA lack matching supported caller contracts.
