# Content-pipeline requests to RightKit (2026-10-08)

Status: not started. Adrian is fixing other RightKit problems first; no execution until he says so.

The three content apps (GenRight, CutRight, PostRight) and the demo-recording capability gate on RightKit packages RK0 to RK10. The authoritative list, with each package's acceptance, is §7 of `/Volumes/D/claude/docs/plans/2026-10-08-demo-recording-requirements.md`. The first gate everywhere is a RightKit execution plan that gives each RK package a scenario and a machine; it does not exist yet.

Read in order:
1. `/Volumes/D/claude/docs/plans/2026-10-08-demo-recording-requirements.md` (R1 to R17, architecture, gates, RK0 to RK10)
2. `/Volumes/D/claude/postright/docs/plans/2026-10-08-postright-execution-plan.md` §2 and §5 (daemon, RK1, RK7, RK10)
3. `/Volumes/D/claude/cutright/docs/plans/2026-10-08-cutright-execution-plan.md` C12, C13 (RK9 composition contract, review capability)
4. `/Volumes/D/claude/genright/docs/plans/2026-10-08-genright-execution-plan.md` G5, G6, G7, G12 (motion package, review capability, brief schema, alignment)
5. `/Volumes/D/claude/docs/plans/2026-10-08-donor-ledger.md` (sources behind the aliases)
6. The six review files in `/Volumes/D/claude/docs/plans/2026-10-08-*review*.md`, for the reasoning behind each gate.

Existing RightKit facts the requests rest on: rightkit-oauth 0.3.0 server mode is staged with no Linux secrets backend; rightkit-control's embedded input path already dispatches window-coordinate events into the webview; rightkit-automation captures single windows to PNG on both OSes; rightkit-http is outbound only; rightkit-jobs `serve` is stdio; rightkit-tts interpolates word times; rightkit-llm carries text only; rightkit-service is same-user sockets and pipes.
