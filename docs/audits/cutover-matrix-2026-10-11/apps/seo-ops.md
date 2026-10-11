# SEO Operations cutover audit

Current operative SEO Operations source has **97 NA, 0 Y, 0 N, 0 M**. No active source route reaches RightKit.

Targets are PowerShell orchestration, isolated Python operations, & dashboard web source. Scope includes `performance-weekly.py`, `keyword_tracking.py`, `store-ahrefs-key.py`, `daily-seo.ps1`, `run-portfolio.ps1`, `publish-dashboard.ps1`, & `_dash` Python/TypeScript/TSX/MJS sources. Reports, editorial, data, credentials, output, research, server-context, & sibling SEO CLI stay excluded.

[run-portfolio.ps1](/Volumes/D/claude/workflows/seo-ops/run-portfolio.ps1:20) resolves sibling SEO engine; [line 24](/Volumes/D/claude/workflows/seo-ops/run-portfolio.ps1:24) selects Python runtime. [performance-weekly.py](/Volumes/D/claude/workflows/seo-ops/performance-weekly.py:113) owns Python collection. Dashboard sources import only local framework modules: [seo-page.tsx](/Volumes/D/claude/workflows/seo-ops/_dash/seo-page.tsx:1) & [ts-llms-index.ts](/Volumes/D/claude/workflows/seo-ops/_dash/ts-llms-index.ts:1).

Potential overlaps with Rust HTTP, filesystem, secrets, process, jobs, logs, & QA are separate reuse gaps: current Python/PowerShell runtime has no supported bridge, FFI, sidecar, manifest, or production caller. Those rows remain NA under runtime-compatibility rules.

Source hashes: `run-portfolio.ps1 c7959690`; `daily-seo.ps1 90295844`; `publish-dashboard.ps1 32c44290`; `performance-weekly.py 7400880d`; `_dash/seo-page.tsx c587c8b9`.

Full 97-row evidence & self-review: [seo-ops.json](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/seo-ops.json:1).

