# RightKit execution plan: content-pipeline packages RK0 to RK10 (2026-10-08)

Status: started 2026-10-08 on Adrian's word ("validate this and then do it too"). Source of truth for requirements and acceptance: §2, §4 and §7 of `/Volumes/D/claude/docs/plans/2026-10-08-demo-recording-requirements.md` (R1 to R17). This plan assigns each package a scenario, a machine and an owner; it does not restate the requirements. RK10 (daemon host) comes from the PostRight execution plan §2.

Machines: **Mac** = Mac mini (this host, macOS 27, APFS). **XPS** = the Windows host (Windows 11; build recorded by Spike B). **Hetzner** = the Linux server (`ssh vendure-auto`).
Owners: **Mac lead** = RightKit Mac session; **Win lead** = RightKit Windows session. "Closes" means the named scenario is green on the installed build on that machine (decision 7); documents and screenshots are evidence only.

## Validation of the request (2026-10-08)

| Fact the request rests on | Checked against | Result |
|---|---|---|
| ScreenRight archive at HEAD `b4e2104` | `archive/screenright-2026-10-08` | present |
| rightkit-oauth 0.3.0 server mode | crates.io / `crates/rightkit-oauth` | now **published** (pointer said staged) |
| no Linux secrets backend | `crates/rightkit-secrets/src` (macos, windows, apple_helper only) | confirmed |
| rightkit-control embedded input dispatches window-coordinate events | `crates/rightkit-control/src/embedded/mac_view.rs`, `win_view.rs` | present |
| admission receipts carry no timestamps or coordinates | `crates/rightkit-control/src/admission.rs` | confirmed |
| rightkit-jobs `serve` is stdio | `crates/rightkit-jobs/src/contract.rs` `serve(reader, writer)` | confirmed |
| rightkit-tts interpolates word times | `crates/rightkit-tts/src/system.rs` | confirmed |
| rightkit-service is same-user sockets and pipes | `crates/rightkit-service` | confirmed |
| contracts directory `tools/rightkit/contracts/` | tree | **absent**; RK0 creates it |

## Packages

| Id | Package | Owner | Machine(s) | Closing scenario | Depends on |
|---|---|---|---|---|---|
| RK0 | Contracts: content brief (revisions, stage ids, `product_demo` + `scenario`), delivery envelope (video, stills, carousel, text), `record-session.v1`, `record-event.v1` (redaction policy), `demo-plan.v1`, daemon API v1. JSON Schema in `tools/rightkit/contracts/` plus a Rust crate `rightkit-contracts` (serde types, validation, version negotiation) | Mac lead | Mac, XPS, Hetzner | the same fixture set validates in three installed engines (one per machine); an incompatible major version is rejected before any work starts | none |
| RK1 | rightkit-oauth server mode with a Linux secrets backend (rightkit-secrets `linux` backend: Secret Service where present, else an encrypted file store bound to a host key with 0600 perms and the existing redaction rules); Meta long-lived token exchange as a daemon job | Mac lead | Hetzner | clock-advance, restart-during-callback and revocation scenarios on Hetzner | RK7 (native build there), RK10 |
| RK2 | Spike A (Mac): off-screen-visible CutRight window to MP4 plus events through the ScreenRight helper under rightkit-control; app-hidden as stretch. Spike B (XPS): true `GraphicsCaptureItem` for the HWND of an off-screen-visible WebView2 window; app-owned render surface as the only fallback | A: Mac lead. B: Win lead | A: Mac. B: XPS | visibility decision table per state (occluded, off-screen, app-hidden, minimized) as supported or unsupported; OCR step id and frame-counter proof (R1); overlap leak test (R12); XPS build recorded | none |
| RK3 | `rightkit-record`: macOS Swift helper (owned through rightkit-process, signed like `rightkit-cloudkit-helper`), Windows Rust helper on the windows crate, dispatch telemetry, pre-encode masking, typed audio tracks, bundle writer and reader with dependencies, `bundle.verify`, rightkit-qa `record: true` | Mac lead (core, macOS); Win lead (Windows helper) | Mac, XPS | R1, R2, R3, R4, R12, R13, R14 green on installed recorder runs on both machines | RK0, RK2 |
| RK4 | Alignment capability, two routes on one annotated fixture set: paid (ElevenLabs character timings to script tokens) and local (QTS audio through rightkit-asr, ASR words aligned to script with pronunciation normalisation, edit-distance lexical confidence, timing confidence, coverage floor) | Mac lead | Mac, XPS | annotated fixtures within 50 ms on both machines; ambiguous anchors rejected; every designated control-name anchor returned | none (paid route spends: record per-run cost; read `docs/rules/paid-compute.md` first) |
| RK5 | Shared review capability: deterministic checks, declared holds, repair caps, advisory judge | Mac lead | Mac | planted-defect and declared-hold scenarios | RK0 |
| RK6 | Channel memory schema | Mac lead | Mac | PostRight P3 and GenRight G7 fixtures validate | RK0 |
| RK7 | Linux native build route: a RightKit broker installed on Hetzner (Linux backend of build-control), builds `postright-engine` natively | Mac lead | Hetzner | `postright-engine doctor` green on Hetzner; binary hash recorded in a receipt | read `docs/rules/README.md` and the Hetzner runbook first |
| RK8 | Scenario runtime: shared Rust interpreter and state machine, GUI worker in a qualified login session, authenticated worker enrolment, capability discovery, work leases, cancellation, bundle transfer, reconnect; `perform` separated from `observe_until` | Mac lead | Mac (second macOS user session) and XPS | isolated control and recovery: interrupt after an input, reconnect, finish once, no duplicated action, no wrong-session attachment; Adrian's session foreground, focus and project unchanged (R11 closes later through P5) | RK0 |
| RK9 | Composition contract: evaluated layers for video and stills, cursor sprites with hotspots, procedural backgrounds, rounded clipping, shadows, temporal blur, canvas-space badges and captions, independently timed webcam | Mac lead | Mac, XPS | combined screen, cursor, webcam and caption composition survives edit, reopen, preview and export | RK0 |
| RK10 | Daemon host capability: HTTPS listener on rightkit-http (server side), device enrolment and authentication, bounded blake3-keyed resumable uploads, route authorisation with three rules (device-authenticated mutations, OAuth callbacks, signed webhooks), lifecycle events | Mac lead | Hetzner | deployed-binary qualification on Hetzner: each authorisation rule positive and negative, interrupted upload resumes, restart preserves state | RK0, RK7 |

## Waves

1. **Now, in parallel:** RK0 (contracts), RK2 Spike A (Mac lead) and Spike B (Win lead), RK4 local route on fixtures, RK7 (Hetzner broker), RK9 contract definition.
2. **After RK0 and RK2:** RK3, RK8, RK5, RK6.
3. **After RK7:** RK10, then RK1.

Integration gates R5 (through C13) and R11 (through P5) close later in the app plans; they are never preconditions of RK3 or RK8.

## Rules that apply to every package

- Rust at runtime; the only declared exception is the signed macOS Swift capture helper (requirements decision 5). No Node, Python or Electron at runtime.
- Never record or drive Adrian's live session; recording scenarios use `launch_offscreen` or a second user session.
- Build every Rust crate through `rightkit cargo`; Hetzner through its own RightKit broker (RK7), never cross-compiled.
- Every new crate gets a catalog owner declaration (rightkit-ownership.json) before first publish.
- Paid calls (ElevenLabs in RK4) record per-run cost; no unbounded loops.

## Status

| Id | State |
|---|---|
| RK0 | built (c5119392); Mac 18/0, XPS check-fixtures 55/0 (broker a6665c04); Hetzner run waits on RK7; publish waits on vendoring schemas into the crate |
| RK2 A | in progress (Mac lead) |
| RK2 B | closed as design decision (3451a1f9): Windows.Graphics.Capture CreateForWindow on an off-screen-visible window; 60.0/30.0 fps delivered; hidden/minimized unsupported; encode off the callback thread (RK3 requirement); live windows-rs run is RK3's first step |
| RK4 | local route scaffolded (rightkit-align, 28/0, synthetic fixtures); real QTS audio on Mac and XPS and the paid route still open |
| RK7 | in progress (Mac lead) |
| RK9 | model scaffolded (rightkit-compose, 24/0); spring easing and click effects not yet modelled |
| RK1, RK3, RK5, RK6, RK8, RK10 | waiting on their dependencies |
