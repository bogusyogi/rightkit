# RightKit cutover inventory

97 components × 29 apps/tools = 2813 classified cells. Fresh production-source audit; Y/N/M/NA carry the definitions below.

[Interactive matrix](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/matrix.html) · [Excel workbook](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/rightkit-cutover-matrix.xlsx) · [CSV](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/matrix.csv) · [JSON evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/matrix.json)

| Code | Meaning |
|---|---|
| Y | Applicable capability uses RightKit; no active competing local mechanism found within owner scope. |
| N | Applicable capability remains locally implemented. |
| M | Shared & overlapping local mechanisms both active. |
| NA | Irrelevant to current feature/runtime, with reason recorded. |

Source status & delivery status are separate. A local dependency override or stale pin does not turn Y into M. Product-specific adapters, schemas & UX remain local where shared owner does not supply that behavior.

## App inventory

| App/tool | Y | N | M | NA | Evidence & handoff |
|---|---:|---:|---:|---:|---|
| CodeRight | 22 | 16 | 3 | 56 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/coderight.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/coderight.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/coderight.md) |
| HeardRight | 31 | 17 | 4 | 45 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/heardright.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/heardright.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/heardright.md) |
| ViewRight | 24 | 8 | 1 | 64 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/viewright.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/viewright.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/viewright.md) |
| MailRight | 17 | 15 | 1 | 64 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/mailright.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/mailright.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/mailright.md) |
| ScrapeRight | 38 | 8 | 2 | 49 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/scraperight.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/scraperight.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/scraperight.md) |
| CutRight | 31 | 4 | 0 | 62 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/cutright.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/cutright.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/cutright.md) |
| GenRight | 25 | 5 | 1 | 66 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/genright.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/genright.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/genright.md) |
| VoiceRight | 1 | 13 | 0 | 83 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/voiceright.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/voiceright.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/voiceright.md) |
| WorkRight | 0 | 3 | 0 | 94 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/workright.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/workright.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/workright.md) |
| Pulse | 7 | 16 | 0 | 74 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/pulse.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/pulse.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/pulse.md) |
| SwiftRight | 0 | 0 | 0 | 97 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/swiftright.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/swiftright.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/swiftright.md) |
| Ember | 8 | 13 | 0 | 76 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/ember.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/ember.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/ember.md) |
| Legion | 1 | 11 | 0 | 85 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/legion.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/legion.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/legion.md) |
| RustRight | 3 | 4 | 0 | 90 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/rustright.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/rustright.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/rustright.md) |
| SellRight | 2 | 4 | 0 | 91 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/sellright.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/sellright.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/sellright.md) |
| RightSites | 2 | 6 | 0 | 89 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/rightsites.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/rightsites.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/rightsites.md) |
| Xtool | 0 | 0 | 0 | 97 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/xtool.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/xtool.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/xtool.md) |
| PostRight | 17 | 1 | 0 | 79 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/postright.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/postright.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/postright.md) |
| LightCraft | 0 | 8 | 0 | 89 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/lightcraft.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/lightcraft.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/lightcraft.md) |
| SEO | 0 | 1 | 0 | 96 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/seo.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/seo.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/seo.md) |
| Rhook | 1 | 4 | 0 | 92 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/rhook.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/rhook.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/rhook.md) |
| Groundwork | 0 | 0 | 0 | 97 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/groundwork.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/groundwork.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/groundwork.md) |
| KDP formatter | 0 | 0 | 0 | 97 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/kdp.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/kdp.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/kdp.md) |
| Render tools | 0 | 0 | 0 | 97 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/render.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/render.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/render.md) |
| Workspace Tools | 0 | 1 | 0 | 96 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/workspace-tools.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/workspace-tools.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/workspace-tools.md) |
| SEO Operations | 0 | 0 | 0 | 97 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/seo-ops.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/seo-ops.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/seo-ops.md) |
| Waitlist | 0 | 0 | 0 | 97 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/waitlist.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/waitlist.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/waitlist.md) |
| ScreenRight (archive) | 1 | 1 | 0 | 95 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/screenright.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/screenright.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/screenright.md) |
| Membrane (retained) | 5 | 5 | 0 | 87 | [Report](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/membrane.md) · [Full cells](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/membrane.json) · [Handoff](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/handoffs/membrane.md) |

## Evidence & scope

- [Validation receipt](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/VALIDATION.md)
- [Cross-app migration queue](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/MIGRATION-QUEUE.md)
- [Source scope & exclusions](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/SCOPE.md)
- [Frozen app corpus](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/corpus.json)
- [Structural validation & concurrent source drift](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/evidence/validation.json)
- [Evidence hashes & quoted source context](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/evidence/citation-snapshots.json)
- [Reconciliation decisions](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/RECONCILIATION.md)
- [Unsupported-host reuse opportunities](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/UNSUPPORTED-HOST-REUSE.md)

This audit describes inspected source routing. It does not certify installed binaries, runtime performance, build success or package publication. Use each N/M task to migrate callers, preserve product policy, remove proven overlap & verify affected targets.
