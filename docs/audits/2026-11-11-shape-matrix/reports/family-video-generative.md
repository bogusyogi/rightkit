# Best-shape review: Video editing and Generative media families

Read-only review, 11 Nov 2026. Trees as in the brief. `S/survey/coderight` does not exist, so CodeRight source was not available; its one module in this family was judged from the RightKit side only. Legion source not available. Each finding is verified by reading unless it says suspicion or not verified. 29 packages: the 28 assigned plus `rightkit-pipeline`, which the task named as a shared package.

Reference rule used: none of CutRight, GenRight, PostRight is live, so where they differ the judgement is on general shape, stated per package. Pulse is the reference for `rightkit-spend` approved budgets; ScrapeRight for `rightkit-playback`.

## Part 1. Packages two or more apps feed

### rightkit-ffmpeg (cargo, Video editing) - shape: gaps

1. **Exists as:** crates/rightkit-ffmpeg.
2. **Feeds (planned lines):** coderight 997, cutright 1,338, genright 971.
3. **In the package today:** Fresh RightKit core (resolve, verify, runner, probe, loudnorm); plus GenRight media probe and H.264 rule, CutRight delivery/intermediate policy, CodeRight agent media tools.
4. **Side by side:** Encoder choice is where CutRight and GenRight differ.
- CutRight (`cutright-project/src/render.rs:98-135`): explicit encoder must be listed; `auto` walks the platform order and *runs a short black-frame encode* per candidate (`hardware_usable`), no software and no MPEG-4 path, error `EncoderUnavailable`.
- GenRight (`genright-protocol/src/h264.rs`, now `rightkit-ffmpeg/src/h264_rule.rs:1-66`): takes the first listed platform encoder without a trial encode, retries once with the failing encoder excluded when ffmpeg's own output shows an init failure (`local_lipsync.rs:740-747`, `pipeline_workflows/compose.rs:427-440,657`), falls to MPEG-4 only when the caller set `allow_mpeg4_fallback`, and reports a typed `h264_encoder_unavailable` with listing details.
- RightKit holds both plus two more: `encoders.rs:203` `pick_h264`, `:247` `select_delivery`, `:283` `select_qualified_delivery`, `h264_rule.rs:57` `decide`.
Judgement: neither app is the reference alone. CutRight's pre-qualification (catches a listed encoder that cannot start, before a long render) and GenRight's typed failure + exclude-and-retry + opt-in MPEG-4 are complementary; `select_qualified_delivery` with an `allow_mpeg4` option and GenRight's error shape covers both. `select_delivery` (accepts MF/VideoToolbox unqualified) is the weaker CutRight variant and can go.
Container probe: GenRight `genright-gen/src/media.rs` is pure Rust header parsing; CutRight uses ffprobe through `cutright-project/src/probe.rs`. Different jobs (provider result sniffing vs deep stream facts); both belong, in `quickprobe` and `probe`. Verified by reading. CodeRight side: source tree absent (`S/survey/coderight` does not exist), judged from the RightKit module only.
5. **Best shape:** Core (resolve/verify/run/probe/loudnorm/content digest/staging) + one encoder inventory and one selection policy + quickprobe. No agent tools, no second copy of the rule elsewhere. Today's crate is the right core with the listed excess. Verdict: The binary resolution, runner and probe core is shared and sound. Around it sit three H.264 selection policies, a CodeRight agent-tool module that is not ffmpeg, and a container probe that is duplicated in rightkit-media-gen.
6. **Per app:**
   - genright: **fits** - h264.rs and media.rs were donated (h264_rule.rs, quickprobe.rs); GenRight then deletes genright-protocol/h264.rs and genright-gen/media.rs
   - cutright: **fits** - cutright-project/render.rs:98-135 already calls rk::pick_h264 + hardware_usable; the 1,338 planned lines are .mjs/.py qualification scripts, not library code
   - coderight: **gaps** - engine/crates/media/lib.rs is present as feature agent_media; source tree not available to compare; module belongs in rightkit-agent-tools
   - heardright: **n/a** - not reviewed in this family
7. **Boundary problems:**
   - agent_media (651 lines, feature-gated, depends on rightkit-agent-policy + tokio): tesseract, scrape, yt-dlp and ffmpeg shell-out tools for an agent; only rightkit-agent-tools uses it; it is not ffmpeg library code
   - Three H.264 pickers in one crate: encoders::pick_h264 (callback), encoders::select_delivery / select_qualified_delivery (doc says "matching CutRight's policy"), h264_rule::decide (GenRight: MPEG-4 only on opt-in, typed h264_encoder_unavailable); rightkit-motion::pick_encoder is a fourth wrapper and rightkit-media-gen/src/host/h264.rs is a near copy of h264_rule.rs (19 differing lines)
   - quickprobe.rs and rightkit-media-gen/src/media.rs are both GenRight media.rs (two packages, same code; the ffmpeg copy is the extended one)
   - rightkit-timeline::ffmpeg::Encoder duplicates encoders::Encoder (minus OpenH264)
   - CutRight tools/qualify/*.mjs, tools/ffmpeg-editing/*.mjs mapped EXTEND are app qualification scripts (APPTEST), not package code
   - `native` module (native-library catalogue for asr/ort/vad) is default-on in an ffmpeg crate
8. **Before publication:**
   - Move agent_media to rightkit-agent-tools; drop the agent-policy/tokio optional deps
   - One encoder decision API: keep pick_h264 + qualify_encoder as the mechanism and one policy function with options {qualify, allow_software, allow_mpeg4}; h264_rule keeps only the error code/details/is_init_failure; delete select_delivery vs select_qualified_delivery split
   - Delete rightkit-media-gen media.rs and host/h264.rs in favour of this crate
   - rightkit-timeline uses encoders::Encoder

Depth: deep. Confidence: high.

### rightkit-compose (cargo, Video editing) - shape: gaps

1. **Exists as:** crates/rightkit-compose.
2. **Feeds (planned lines):** cutright 5,679, genright 614.
3. **In the package today:** Fresh composition model (ticks, layers, eval; ScreenRight camera curve) + CutRight cutright-compose as feature `native` + GenRight still_finish as feature `still-finish`.
4. **Side by side:** Side by side the two feeders do not overlap in code or behaviour.
- CutRight `cutright-compose` (`src/lib.rs`, `mapper.rs`, `decode.rs`, `snapshot.rs`): immutable `ComposeSnapshot` (project + revision + timeline hash), `compose_frame` maps timeline semantics to `FrameDescriptor`s and rejects what it cannot map, ffmpeg rawvideo decode with a pixel LRU, frame identity/cache/scheduler, streamed PCM mix, A/V clock. Present in `rightkit-compose/src/native/*` (coverage 2,115 of 3,217 distinctive lines; the missing lines are tests).
- GenRight `genright-engine/src/still_finish.rs`: composite a PNG logo at a corner of each PNG, write under a naming template, refuse existing outputs. Present as `rightkit-compose/src/still_finish.rs:1-8`.
- GenRight `genright-studio/src/pipeline_workflows/compose.rs:1-4`: "assemble image cards and video clips, lay narration + optional music bed, burn or mux subtitles, loudness-normalize, then verify" through ffmpeg filter graphs. Mapped to rightkit-media-pipeline, not here.
The earlier line match (CutRight present, GenRight "present" via still_finish) hides that GenRight's composition capability is elsewhere. Which is the better general shape for "make a video from parts": CutRight's (timeline model -> RenderPlan -> typed ffmpeg args, with undo, revisions and a preview that agrees with export) is the general one; GenRight's compose step is a fixed recipe that could be expressed as a generated timeline. Verified by reading both.
5. **Best shape:** rightkit-compose = model + evaluate only. Preview/decode adapter in its own crate. GenRight compose workflow becomes a producer of a rightkit-timeline document rendered by the shared path (longer-term), or stays a media-pipeline step. Today's crate is not that shape: it carries two unrelated features. Verdict: The pure composition document and evaluate() are app-neutral. The two app donations are different capabilities sharing a word: CutRight's is a timeline-to-wgpu frame pipeline, GenRight's is logo stamping on PNG stills, which has nothing to do with the model.
6. **Per app:**
   - cutright: **fits** - native/ is cutright-compose nearly line for line (decode, mapper, base_eval, snapshot, pcm, clock, scheduler)
   - genright: **gaps** - still_finish.rs fits as code but is in the wrong crate; GenRight's real "compose" (pipeline_workflows/compose.rs: ffmpeg assemble of cards, clips, narration, music, subtitles, loudnorm) lives in rightkit-media-pipeline and does not use the composition model
7. **Boundary problems:**
   - still_finish (518 lines, image + crypto + time deps) is a keyless stills finisher returning serde_json::Value with stable codes: belongs with media-pipeline finish steps or a small rightkit-stills, not here
   - Two tick types on the same 705,600,000/s clock: rightkit-compose::ticks::Ticks(u64) and rightkit-timeline::time::Ticks(i64); native/ imports the timeline one while the model uses its own
   - feature `native` makes compose depend on compositor + timeline + video-project + ffmpeg: it is a preview/render adapter for the CutRight project model, not part of the composition model; candidate for its own crate together with the missing playback session
   - GenRight ffmpeg compose and CutRight timeline render (rightkit-timeline::ffmpeg) are two "assemble clips to a file" paths with no shared layer
8. **Before publication:**
   - Move still_finish out (media-pipeline or media-gen finish)
   - One Ticks/Fps/Rational definition used by compose and timeline
   - Split `native` into a crate (e.g. rightkit-video-preview) that also receives CutRight playback.rs session logic

Depth: deep. Confidence: high.

### rightkit-motion (cargo, Generative media) - shape: gaps

1. **Exists as:** crates/rightkit-motion.
2. **Feeds (planned lines):** cutright 626, genright 2,619.
3. **In the package today:** Fresh neutral design (beats, sheet, FrameSource render driver, encoder) that GenRight already builds on; GenRight project_server/composition doc donated; CutRight contactsheet and thumbnail engine commands added under feature engine-commands.
4. **Side by side:** - GenRight (`genright-engine/src/motion_service.rs:1-13`): render an HTML composition to MP4: loopback project server, managed headless Chrome, per-frame deterministic seek + CDP screenshot, frames piped to ffmpeg through the `rightkit-motion` encode driver. It imports the crate today (`motion_encode.rs:13,83,96`, `motion_service.rs:32,552`). `motion_server.rs` and `motion_doc.rs` are in the crate (`project_server.rs`, `composition.rs`).
- CutRight (`cutright-engine/src/contactsheet_cmd.rs`, `thumb_cmd.rs`): `contactsheet.make {dir, revision,...}` tiles frames of the render recorded for one timeline revision and refuses a stale revision; `media.thumbnail` extracts cached JPEG frames for UI strips. Both are in `rightkit-motion/src/cmd/` (203/210 and 124/133 distinctive lines).
These are not two implementations of one capability. The only real overlap is contact sheets: the neutral `sheet.rs:1-14` and CutRight's command differ in tile size policy (above). GenRight's is the reference for motion; CutRight's revision binding is the better behaviour for a review sheet and should be kept, in the project command layer, on top of `sheet`. Verified by reading.
5. **Best shape:** beats + sheet + render/encoder/frame + project_server + browser_source. No engine commands. Today's crate is that plus the misplaced cmd/ module. Verdict: The FrameSource driver + project server + browser source is a clean seam and GenRight already adopts it. The CutRight commands in cmd/ are project-bound engine handlers with a cutright schema name and are not motion.
6. **Per app:**
   - genright: **fits** - motion_service.rs/motion_encode.rs already call rightkit_motion::{render, pick_encoder, probe_inventory}; motion_server.rs and motion_doc.rs are in the crate; studio motion.rs (templates, scaffold, plan) is not, by design or omission
   - cutright: **wrong** - contactsheet_cmd.rs and thumb_cmd.rs are present verbatim in cmd/ but they are video-project commands (revision-bound contact sheet, thumbnail cache), mapped to the wrong package
7. **Boundary problems:**
   - cmd/contactsheet.rs + cmd/thumb.rs pull rightkit-engine-host, rightkit-video-project, rightkit-cache into a motion crate and write schema `cutright.contactsheet.v1`
   - Two contact-sheet implementations in one crate: sheet.rs (pure plan + `assemble`, 2x3 tiles of 960x540, min tile 480 px because "330 px tiles miss defects") and cmd/contactsheet.rs (ffmpeg tile filter, default 320 px tiles, up to 64); the CutRight default is below the minimum the neutral module enforces
   - encoder.rs pick_encoder repeats the H.264/MPEG-4 rule of rightkit-ffmpeg::h264_rule
   - GenRight studio.motion templates/scaffold/plan (653 lines) has no home; media-pipeline/src/motion holds the service
8. **Before publication:**
   - Move cmd/ to the video-project command layer; have it lay out tiles with sheet::plan_sheets
   - Remove the engine-commands feature and its four optional deps
   - pick_encoder delegates to the single rightkit-ffmpeg policy
   - Decide owner of HyperFrames templates/plan (media-pipeline motion)

Depth: deep. Confidence: high.

### rightkit-spend (cargo, Generative media) - shape: gaps

1. **Exists as:** crates/rightkit-spend.
2. **Feeds (planned lines):** cutright 4,345, genright 2,755, postright 2,272, pulse 1,655.
3. **In the package today:** Fresh generic ledger (0.2.3); PostRight shims absorbed in 0.1.1, GenRight ledger semantics in 0.2.0, Pulse approved budgets in 0.2.2.
4. **Side by side:** All three money apps already sit on the crate (`pulse/core/Cargo.toml:63`, `postright/crates/engine/Cargo.toml:44`, `genright/crates/genright-gen/Cargo.toml:22`, all `=0.2.0`), so the comparison is between three app layers over one ledger, not three ledgers.
- GenRight (`genright-gen/src/ledger.rs:1-55`): USD floats at the command surface, three allowances reserved in one transaction (daily, monthly, device epoch), carry-over of open reservations across midnight computed by lowering today's limit (`limits_in`), its own `genright_spend_attempts` table (line 332), invoice reconciliation tables (1488, 1499), a process-global caller budget (2194).
- PostRight (`engine/src/spend.rs:1-40`): integer micro-units end to end, allowance keyed venture/channel + month, price rows with verifier and 60-day age rule, covered overruns auto-acknowledged in the commit transaction, billed-unknown commits, safety factor / variance / ceiling model (lines 180-202, 1338-1527).
- Pulse (`core/src/budget.rs:1-40`): owner-approved budget, HMAC seal keyed from the OS store, allowances issued with a TTL (832-850), idempotent reports with `final_report` (969-975) and `partial_open` accounting (326-341), append-only audit.
- CutRight: no spend code. `cutright-ledger/src/lib.rs:1-11` states what it is.
The crate today has native forms of GenRight's (`group.rs` `reserve_multi`, `set_device_allowance`; `reconcile.rs`; `budget.rs`; `ReserveOptions::carry_over`) and Pulse's (`approved.rs`) and PostRight's (`commit_unknown`, account, currency, `_in`/`_on`, strict). Remaining difference: `report_approved_spend` (`approved.rs:614`) commits the reservation on the first report; Pulse records several partial reports against one open allowance and expires each allowance on its own clock. Verified by reading.
5. **Best shape:** Integer micro-unit ledger keyed by opaque scope+period, app supplies estimates, prices and identity. Today's crate is that shape except for the two Pulse items. Not a place for decision ledgers or token budgets. Verdict: The reserve/commit ledger with multi-cap, device epoch, caller budget, invoice reconcile and sealed approved budgets is the right general shape and no app code sits in it. Pulse (the highest-priority feeder) still needs partial reports and a per-allowance lifetime that the approved-budget API lacks.
6. **Per app:**
   - pulse: **gaps** - core/src/budget.rs is already a layer on the crate (=0.2.0); crate approved.rs covers create/approve/seal/audit, but Pulse report(.., final_report) takes several reports per allowance and issue() has a per-allowance TTL; report_approved_spend commits once
   - genright: **fits** - ledger.rs is a layer on 0.2.0 written before reserve_multi, set_device_allowance, reconcile_invoice and Budget existed; all four are now native, so the layer shrinks to settings, estimate rounding and the job-attempt map
   - postright: **fits** - engine/spend.rs keeps price catalogue, safety factor, variance and ceiling model, which the crate documents as the app/rightkit-llm side; covered-overrun auto-acknowledge maps to with_overrun_gate or stays as is
   - cutright: **n/a** - cutright-ledger is not a spend ledger (decision ledger, edit plans, palette, prefs, relink); mis-mapped
7. **Boundary problems:**
   - cutright-ledger (4,345 lines) mapped here is an editorial decision ledger, agent edit-plan contract, command palette, preference recommender and source relink: none of it is spend, and none of it is in RightKit today
   - pulse membrane-core/src/budget.rs (191 lines) is a token attention budget, not money: mis-mapped
   - rightkit-media-gen/src/ledger.rs (2,278 lines) is GenRight ledger.rs moved whole: it re-implements three-cap reserve, device-allowance epochs, invoice reconciliation and caller budgets beside the crate that now has them (two packages, same capability)
   - PostRight price rows and their 60-day qualification: crate docs point at rightkit-llm catalogue; not decided which package owns non-LLM prices
8. **Before publication:**
   - Add non-final (partial) spend reports and a per-reservation expiry to the approved-budget API (Pulse report final_report, issue ttl_seconds)
   - Re-map cutright-ledger out of spend (decision ledger/edit-plan to video-project or a new rightkit-decisions; palette to a UI/command package)
   - Reduce rightkit-media-gen ledger.rs to a thin layer over reserve_multi / set_device_allowance / reconcile_invoice / Budget
   - Apps pin =0.2.0; publish 0.2.3 and move the three adopters

Depth: deep. Confidence: high.

### rightkit-review (cargo, Video editing) - shape: gaps

1. **Exists as:** crates/rightkit-review.
2. **Feeds (planned lines):** cutright 5,035, genright 683, postright 2,409.
3. **In the package today:** Fresh shared core (Subject, RuleSet, Verdict, disposition, repair loop, ladder, qa, delivery) that CutRight and PostRight already call; CutRight cutright-qa moved as feature render-qa; PostRight phone surface as feature phone; Legion code review as features code/minimize.
4. **Side by side:** Three feeders, three different layers on the same core:
- CutRight `cutright-qa` (`src/run.rs`, `critic.rs:5-20`, `report.rs:17-45`): probes a final render (decode, black/freeze/silence, loudness, geometry, captions, optional retranscription), produces `rightkit_review::qa::VerdictRecord`, content-addressed receipt, and builds platform packages that refuse an unqualified render. Uses `rightkit_review::{disposition, Finding, RepairCap, Verdict}` and `rightkit_review::delivery`.
- PostRight `engine/src/gate.rs:1-25`: deterministic *text* gate (scripts, hooks, captions) with per-scope immutable policy revisions, proof-backed claims and two registered extension rules; `review_surface.rs:1-25` is the phone approve/reject/pause surface.
- GenRight `genright-studio/src/eyes.rs:1-40`: a human approval sidecar bound to the artifact hash; no rules, no verdicts.
No two implement the same capability, so there is no behavioural conflict to settle; the core API both apps call is identical. The comparison that matters is CutRight `PlatformRules` (`render_qa/package.rs:86-96`) against `rightkit-platform-rules` (`src/lib.rs:1-19`): the latter (sourced, revisioned, stale-blocking records) is the better shape and PostRight's direction. Verified by reading.
5. **Best shape:** Core + media `delivery`/`qa` rules + render probe that needs only a file and ffmpeg; phone surface optional; packaging elsewhere. Core is already that; render-qa is not. Verdict: The verdict/rule/disposition core is the shared shape and both apps use it unchanged. The render-qa feature is CutRight's whole QA + social packaging with embedded platform limits and a hard dependency on the CutRight project model.
6. **Per app:**
   - cutright: **fits** - cutright-qa is in render_qa/ (run, package, probe, critic, receipt, spec, retranscribe)
   - postright: **fits** - engine/gate.rs is an app layer over rightkit_review text rules with its own policy store and two extension rules; review_surface.rs is feature phone; both adopt as is
   - genright: **n/a** - eyes.rs (human-eyes approval sidecar) is an approval, now in rightkit-approvals/src/eyes.rs; mapping to review was wrong
7. **Boundary problems:**
   - render_qa/package.rs embeds per-platform limits (aspects, max duration, title/description/tag lengths) for YouTube/Shorts/Reels/TikTok as compiled JSON; rightkit-platform-rules exists with the rule "code never holds a platform limit"
   - Social packaging (package_render, zip, thumbnail, metadata) is publishing, not review: overlaps PostRight/social-publish territory
   - render-qa depends on rightkit-video-project (worker::spawn_ffmpeg, probe::spawn_ffprobe) and rightkit-timeline (Preset, Timeline) only to run ffmpeg and read presets, so a GenRight render cannot be QA-gated without a CutRight project
   - Schema ids `cutright.qa.receipt`, `cutright.social-package`, `cutright.qa.geometry` in a shared package
   - One crate with five optional subsystems (render-qa, phone, code, minimize + core) and unrelated dependency sets (daemon+ring, evidence contracts, video-project)
8. **Before publication:**
   - render_qa takes a file + a spec, running ffmpeg through rightkit-ffmpeg directly; drop the video-project/timeline deps
   - Move package.rs to the publishing side (social-publish or video-project exports) and read limits from rightkit-platform-rules
   - Rename cutright.* schema ids (no live users)
   - Consider splitting code/minimize (Legion) into rightkit-code-review; they share nothing with media review

Depth: deep. Confidence: high.

### rightkit-pipeline (cargo, Generative media) - shape: wrong

1. **Exists as:** crates/rightkit-pipeline.
2. **Feeds (planned lines):** cutright 13,753, postright 2,521, genright 1,701.
3. **In the package today:** CutRight cutright-genright moved whole (GenRightClient, Orchestrator, Journal, ModelCatalog, generated_source) + channel_recipe.
4. **Side by side:** - CutRight -> GenRight (`cutright-genright/src/orchestrator.rs:1-11`, `engine.rs:1-12,30-44`): journal the request before invoking, one request id = one job, recover by lookup then idempotent resubmit, import = copy, verify sha256, record, then `job ack`; completion by polling `job status`; refuses engines below contract v2 or missing capabilities.
- PostRight -> CutRight/GenRight (`postright/crates/engine/src/dispatch.rs:1-30`): engine path from an env var, absolute only; attempt row `running` before spawn; a dead owner's attempt becomes `interrupted` and is not rerun without `retry_interrupted`; recorded pid/group/start identity, kill only on identity match; envelopes accepted in one transaction.
- GenRight callee (`genright-engine/src/brief.rs`, header now at `rightkit-media-pipeline/src/brief.rs`): `brief run --project --input`, one JSON on stdout, exit codes 0/1/2, envelope validated before write. CutRight's `brief_cmd.rs:1-28` implements the same contract over render + QA.
Where they overlap (spawn an engine, bound it, survive a crash), PostRight's is the stricter and more general process discipline, CutRight's is the better protocol handshake and idempotency model. The package should carry both; today it carries CutRight's only. Verified by reading.
5. **Best shape:** See actions; today's crate is one of four needed parts. Verdict: Not in the assigned list but flagged in the task: it holds one app's client for one other app's CLI. The three feeders implement three sides of engine-to-engine calls and only CutRight's is here.
6. **Per app:**
   - cutright: **fits** - its own crate; 8,597 lines of engine commands (genright_cmd 3,868, batch, recipe, brief) are not ported
   - postright: **wrong** - engine/dispatch.rs (dispatch a content brief to cutright-engine or genright-engine, attempt rows, identity-checked sweep) has 26 matching lines; nothing here serves it
   - genright: **wrong** - engine/brief.rs is the callee side (`brief run`), now in rightkit-media-pipeline/src/brief.rs, not here
7. **Boundary problems:**
   - Types are named for the callee (GenRightClient, DEFAULT_CONSUMER = "cutright")
   - Owned-engine-process mechanics exist twice: CutRight engine.rs (stdin/arg size switch, output caps, contract-version + capability check) and PostRight dispatch.rs (absolute path only, attempt row before spawn, process-identity sweep, no silent rerun); neither is shared
   - `brief run` executors exist per engine (CutRight brief_cmd.rs 1,170 lines unported; GenRight in media-pipeline) with one CLI contract and no shared skeleton
   - channel_recipe feature pulls captions + color + audio into an orchestration crate
   - Name collides in meaning with rightkit-media-pipeline
8. **Before publication:**
   - Redraw as engine-to-engine calling: (a) owned engine call with PostRight's attempt/sweep discipline and CutRight's contract/capability handshake, (b) generate client (submit, wait, import, journal) named for the contract, (c) brief dispatcher, (d) shared `brief run` skeleton (CLI, envelope validation and writing) with per-engine deliverable producers
   - Move channel_recipe to the video side

Depth: deep. Confidence: medium.

### rightkit-playback (cargo, Video editing) - shape: gaps

1. **Exists as:** crates/rightkit-playback.
2. **Feeds (planned lines):** cutright 1,819, scraperight 276.
3. **In the package today:** ScrapeRight compatibility-remux playback (source registry, cancel registry, fingerprinted cache, Tauri commands).
4. **Side by side:** - ScrapeRight (`crates/engine/src/playback.rs:1-11`): remux container, convert audio to AAC, copy video bytes, explicit error for unsupported video codec, derivative cached by content fingerprint; mechanics from `rightkit-playback` (`compatibility_copy`, `PlaybackRegistry`).
- CutRight (`apps/cutright/src-tauri/src/playback.rs:1-13`): "the canonical revision, evaluated by `cutright_timeline::compile`, decoded and composited in Rust, delivered to the webview as RGBA frames"; audio through `rightkit_audio::playback::OutputSession` as master clock; a plain file plays as a one-clip timeline.
Different capabilities; no reference question. CutRight's model (preview through the same compile path as export) is the right general shape for any app that edits; ScrapeRight's is right for any app that only plays a saved file. Verified by reading.
5. **Best shape:** Two packages: rightkit-playback (file compatibility playback, as today) and a preview-session crate over compose/compositor/timeline. Verdict: For what it is (make a file playable in a webview without re-encoding video) it is complete and ScrapeRight already uses it. CutRight's planned 1,819 lines are a different capability, a native timeline preview session, and none of it is in RightKit although @rightkit/video-ui calls its commands.
6. **Per app:**
   - scraperight: **fits** - crates/engine/src/playback.rs is a thin policy layer over compatibility_copy/PlaybackRegistry
   - cutright: **wrong** - apps/cutright/src-tauri/src/playback.rs is decode + wgpu composite + audio feeder + prefetch + A/V clock for a timeline revision; 17 of 1,059 distinctive lines match; adopting this crate would not give it a preview
7. **Boundary problems:**
   - CutRight playback.rs is mapped EXTEND here but belongs with rightkit-compose `native` (it is that module's only real consumer): session, prefetch workers, audio feeder, finishing refresh
   - @rightkit/video-ui lib/media.ts:651-673 invokes playback_open/_control/_frame/_status/_close/_refresh_finishing: the Rust side of those commands exists only in the CutRight app shell
8. **Before publication:**
   - Do not extend this crate with timeline preview; create the preview-session crate (see rightkit-compose) with the Tauri commands behind a feature, as this crate does with tauri_commands
   - Publish as is for ScrapeRight

Depth: deep. Confidence: high.

### rightkit-media-pipeline (cargo, Generative media) - shape: gaps

1. **Exists as:** crates/rightkit-media-pipeline.
2. **Feeds (planned lines):** genright 9,149.
3. **In the package today:** GenRight pipeline_workflows (+compose, translate, dubbing, fanout, media, finish, asr), production stack, brief run, motion service.
4. **Side by side:** Only GenRight feeds it by plan; the cross-app comparison is under rightkit-pipeline and rightkit-compose. Read lib.rs, production.rs and brief.rs headers, compose.rs header.
5. **Best shape:** Workflow DAG engine + steps; brief, production planning and motion service elsewhere. Verdict: Editable step DAGs over media-gen with fan-out and native subtitle/compose steps is a reasonable package. It also carries the engine's `brief run`, the HyperFrames motion service and the deterministic "production stack" planner, three different things.
6. **Per app:**
   - genright: **fits** - 4,707 of 5,127 distinctive lines present
   - cutright: **n/a** - its brief_cmd/recipe_cmd/batch_cmd were mapped to rightkit-pipeline
   - postright: **n/a** - dispatcher side, see rightkit-pipeline
7. **Boundary problems:**
   - brief.rs (1,473) is the callee side of the content-brief contract; CutRight implements the same CLI in brief_cmd.rs; no shared skeleton
   - pipeline_workflows/compose.rs builds ffmpeg filter graphs for assembling a video, separate from rightkit-timeline rendering
   - production.rs (campaign factory, brand-kit check, packs) is content planning: nearer rightkit-content-plan / PostRight than media
   - motion/ (812 + 147) is the engine motion service; rightkit-motion is the library
   - API is handle(ctx, name, &Value) by `studio.*` command name
8. **Before publication:**
   - Move brief.rs to the shared brief-run skeleton (see rightkit-pipeline)
   - Decide owner of production.rs against rightkit-content-plan
   - Rename so it is distinguishable from rightkit-pipeline

Depth: deep. Confidence: medium.

## Part 2. Single-feeder and unfed packages

### rightkit-video-project (cargo, Video editing) - shape: gaps

1. **Exists as:** crates/rightkit-video-project.
2. **Feeds (planned lines):** cutright 14,524.
3. **In the package today:** CutRight cutright-project moved (project, ingest, render, source, probe, worker, loudness, lock, audio_sync, still).
4. **Side by side:** Single feeder. Read lib.rs, render.rs encoder path, reframe_track against rightkit-reframe, and the absent-file list from coverage. No second implementation exists in another app; GenRight has no project model (verified by crate list).
5. **Best shape:** project + source + ingest + render + probe + worker + loudness, plus a command layer. Today's crate is the library half. Verdict: A sound project layer (directory layout, lock, BLAKE3 source identity, ingest, render orchestration) that a second editing app could adopt. Half the planned lines, the engine command layer, are not ported, and one module is duplicated in rightkit-reframe.
6. **Per app:**
   - cutright: **gaps** - cutright-project is present; media_cmd.rs (4,167), bundle_cmd.rs (1,428), project_cmd.rs (1,030), outputs/storage/slot cmds are absent (43, 31, 21 matching lines)
   - genright: **n/a** - no project model; could use it for finishing hand-off
7. **Boundary problems:**
   - reframe_track.rs (602 lines) is the same module as rightkit-reframe/src/track.rs (51 differing lines); nothing outside its own crate imports the video-project copy
   - Engine command handlers are inconsistent across the family: autoedit, shorts and motion carry `engine-commands` features with cmd/ modules; video-project, captions and color do not, and rightkit-engine-host has a `project-cmd` feature depending back on video-project
   - Docs and ids say CutRight (`cutright.project`, `cutright.render.v1`, `cutright.derived`, env `CUTRIGHT_FFMPEG`/`CUTRIGHT_FFPROBE`)
   - cutright-ledger (decision ledger, edit plans, plan intents, source relink) depends on cutright-project + cutright-timeline and has no RightKit home
8. **Before publication:**
   - Delete reframe_track.rs here
   - Pick one rule for engine commands (recommended: a `cmd` module per domain crate behind `engine-commands`, engine-host stays free of video deps) and port media/bundle/project/outputs/storage/slot commands under it
   - Rename ids and env prefixes to an app-supplied prefix
   - Give decision ledger + edit plan + relink a home (here as modules, or rightkit-decisions)

Depth: deep. Confidence: medium.

### rightkit-timeline (cargo, Video editing) - shape: gaps

1. **Exists as:** crates/rightkit-timeline.
2. **Feeds (planned lines):** cutright 8,011.
3. **In the package today:** CutRight cutright-timeline + cutright-fcpxml + cutright-otio merged.
4. **Side by side:** Single feeder; read lib.rs, ffmpeg.rs head, time.rs against compose ticks.rs. Verified the duplicate types by reading.
5. **Best shape:** As today minus the duplicate types; presets as data. Verdict: Typed actions with inverses, revision store, render plan and typed ffmpeg args are a good general timeline core. It defines its own tick, encoder and platform-preset types that other RightKit crates also define.
6. **Per app:**
   - cutright: **fits** - model, action, plan, ffmpeg, title, store, fcpxml, otio all present (3,403 of 4,428 distinctive lines; rest is tests)
   - genright: **n/a** - would become a consumer if compose workflows emit timelines
7. **Boundary problems:**
   - time::Ticks(i64) vs rightkit-compose::ticks::Ticks(u64), same clock, two types
   - ffmpeg::Encoder (VideoToolbox, Nvenc, MediaFoundation) duplicates rightkit-ffmpeg::encoders::Encoder
   - ffmpeg::Preset (YouTube, Reels, TikTok, Square) is a platform list in code; rightkit-review render_qa Platform and rightkit-captions safe_zones.json are two more
   - ffmpeg.rs (1,220 lines) is render-plan-to-ffmpeg and title.rs builds drawtext: rendering concerns inside the model crate, though pure (no execution)
8. **Before publication:**
   - Share Ticks/Fps with compose (one small time module, either crate re-exports)
   - Use rightkit-ffmpeg Encoder
   - Presets as data (name, size, safe zone id) resolved against rightkit-platform-rules ids

Depth: deep. Confidence: medium.

### rightkit-autoedit (cargo, Video editing) - shape: gaps

1. **Exists as:** crates/rightkit-autoedit.
2. **Feeds (planned lines):** cutright 14,279.
3. **In the package today:** CutRight cutright-cuts, -editorial, -variants, -rules merged, plus cmd/ (autoedit, rules, autonomy, mic, style).
4. **Side by side:** Single feeder. Over 5,000 planned lines but read only at lib.rs, module list and Cargo level; internals of cuts/editorial not read. Findings above marked as suspicion where not read.
5. **Best shape:** cuts + editorial + variants + rules as today; commands under the family rule. Verdict: Transcript-driven cut planning with scoped rules is reusable by any app with a transcript and a timeline. Read at structure level only; the cmd/ layer and CutRight schema ids are the visible issues.
6. **Per app:**
   - cutright: **fits** - 5,190 of 7,927 distinctive lines present; missing are tests and plan_cmd/editplan_cmd
   - heardright: **n/a** - has transcripts but no editing
7. **Boundary problems:**
   - cmd/mic.rs (213 lines, microphone command) sits in an auto-edit crate: suspicion that it is recording, not editing (not read)
   - editplan_cmd.rs / plan_cmd.rs (805 lines) not ported: they drive cutright-ledger edit plans, which have no home
   - rules/ladder.rs here and rightkit-review/src/ladder.rs are both "autonomy ladder" modules (review doc cites cutright-rules::ladder): possible duplicate, not verified
   - Schema ids cutright.editorial.v1, cutright.variant, cutright.variant-compare.v1
8. **Before publication:**
   - Compare rules/ladder.rs with rightkit-review ladder and keep one
   - Move or justify cmd/mic.rs
   - Rename schema ids
   - Port edit-plan commands once the decision ledger has a home

Depth: light. Confidence: low.

### rightkit-shorts (cargo, Video editing) - shape: gaps

1. **Exists as:** crates/rightkit-shorts.
2. **Feeds (planned lines):** cutright 4,992.
3. **In the package today:** CutRight cutright-shorts moved, plus cmd/moments (OCR index, search).
4. **Side by side:** Single feeder; read lib.rs, llm.rs constants, Cargo features.
5. **Best shape:** config + score + propose + ranking + short; no compiled model id. Verdict: Scoring profile as versioned JSON and LLM ranking validated onto word boundaries is reusable. A default model id is compiled in.
6. **Per app:**
   - cutright: **fits** - 1,928 of 2,935 distinctive lines present
   - postright: **n/a** - likely future consumer
7. **Boundary problems:**
   - llm.rs:30 DEFAULT_MODEL = "claude-sonnet-5-5": a model choice in a library; rightkit-llm owns the catalogue
   - cmd/moments (OCR index + text search over a project) is moment search, not shorts; pulls ocr + search + engine-host
   - Depends on rightkit-autoedit and rightkit-captions for short.rs (accepted proposal to timeline actions)
8. **Before publication:**
   - Model id from caller or rightkit-llm default
   - Move moments search to the project command layer
   - Rename cutright.shorts.proposals.v1

Depth: light. Confidence: medium.

### rightkit-reframe (cargo, Video editing) - shape: gaps

1. **Exists as:** crates/rightkit-reframe.
2. **Feeds (planned lines):** cutright 4,222.
3. **In the package today:** CutRight cutright-reframe moved (YuNet face detection via ONNX Runtime, tracker, plan, approval, render).
4. **Side by side:** Single feeder; read lib.rs, models.rs, compared track.rs with the video-project copy (verified duplicate).
5. **Best shape:** As today, sole owner of the track module. Verdict: Evidence -> plan -> approval -> render with a pinned, hashed model download is a clean package. Its smoothing core also exists in rightkit-video-project.
6. **Per app:**
   - cutright: **fits** - 1,662 of 2,501 distinctive lines present
7. **Boundary problems:**
   - track.rs duplicated as rightkit-video-project/src/reframe_track.rs
   - The "no vertical render without an approved plan" invariant is a product rule compiled into the library (plan::approve): acceptable as a safety default, should be stated as overridable or not
   - Schema ids cutright.reframe.*, cutright.transcript.v1
8. **Before publication:**
   - Single copy of track.rs (here)
   - Rename schema ids
   - Decide whether approval is mandatory at library level

Depth: light. Confidence: medium.

### rightkit-color (cargo, Video editing) - shape: best

1. **Exists as:** crates/rightkit-color.
2. **Feeds (planned lines):** cutright 4,108.
3. **In the package today:** CutRight cutright-color moved.
4. **Side by side:** Single feeder; read lib.rs and Cargo only.
5. **Best shape:** As today. Verdict: Pure plan-to-filter-chain library (colour space detection, HDR/Apple Log to SDR, cube LUTs, auto correct, shot match, grade) with only ffmpeg and fs deps; nothing product-specific found beyond doc wording.
6. **Per app:**
   - cutright: **fits** - library present; colour_cmd.rs (765 lines) is command layer, unported
   - genright: **n/a** - could use sdr/space for finishing
7. **Boundary problems:**
   - colour_cmd.rs awaits the family command-layer decision
8. **Before publication:**
   - none for the library; port colour_cmd under the chosen command rule

Depth: light. Confidence: medium.

### rightkit-captions (cargo, Video editing) - shape: gaps

1. **Exists as:** crates/rightkit-captions.
2. **Feeds (planned lines):** cutright 3,177.
3. **In the package today:** CutRight cutright-captions moved; words/align/export are re-exports of rightkit-transcript.
4. **Side by side:** Single feeder; read lib.rs and the three re-export stubs (verified they forward to rightkit-transcript).
5. **Best shape:** segment + style + styled + burn + placement; platform data external. Verdict: Cue building, style presets and drawtext burn-in on top of rightkit-transcript and rightkit-typeset is the right layering. Platform safe zones are shipped as crate data, a third place platform facts live.
6. **Per app:**
   - cutright: **gaps** - library present (855 of 1,787); captions_cmd, caption_sets_cmd, caption_styles_cmd, caption_placement (1,045 lines) unported
   - heardright: **n/a** - SRT/VTT export already via rightkit-transcript
7. **Boundary problems:**
   - data/safe_zones.json per platform vs rightkit-platform-rules vs timeline Preset
   - Depends on rightkit-timeline for presets only (suspicion, from Cargo)
8. **Before publication:**
   - Safe zones keyed by platform-rules ids or moved there
   - Port caption commands under the chosen command rule
   - Rename cutright.caption-cue.v1

Depth: light. Confidence: medium.

### rightkit-compositor (cargo, Video editing) - shape: gaps

1. **Exists as:** crates/rightkit-compositor.
2. **Feeds (planned lines):** cutright 3,088.
3. **In the package today:** CutRight cutright-compositor, -gpu, -effects, -masks merged (adapted from OpenCut Classic, MIT).
4. **Side by side:** Single feeder; read Cargo and the public surface of gpu/context.rs against rightkit-gpu context.rs/backend.rs (verified no dependency).
5. **Best shape:** Compositor over a shared device context. Verdict: A self-contained wgpu layer compositor with no RightKit dependencies. It creates its own device context while rightkit-gpu exists to be the suite's one wgpu context.
6. **Per app:**
   - cutright: **fits** - 1,713 of 1,748 distinctive lines present
7. **Boundary problems:**
   - gpu/context.rs (516 lines) builds instance/adapter/device itself; rightkit-gpu (backend policy, crash sentinel, exit gate, memory budget) has no dependents and its docs say CutRight moves to it
   - Both pin wgpu 30
8. **Before publication:**
   - GpuContext::from(rightkit_gpu::GpuContext) so device creation, backend policy and the crash sentinel come from rightkit-gpu; keep pipelines/samplers here

Depth: light. Confidence: medium.

### rightkit-gpu (cargo, Video editing) - shape: gaps

1. **Exists as:** crates/rightkit-gpu.
2. **Feeds (planned lines):** no surveyed app.
3. **In the package today:** Adapted from Ember crates/gpu; no surveyed app feeds it.
4. **Side by side:** No app code to compare. Read lib.rs and signatures.
5. **Best shape:** As designed, once it has a consumer. Verdict: Right idea (one device policy for the suite) but nothing in RightKit or the surveyed apps depends on it, so the shape is unproven.
6. **Per app:**
   - cutright: **gaps** - would adopt through rightkit-compositor
   - genright: **n/a** - local generation runs out of process
7. **Boundary problems:**
   - Unused by rightkit-compositor
8. **Before publication:**
   - Wire into rightkit-compositor before publishing as the adopted shape

Depth: light. Confidence: low.

### @rightkit/video-ui (npm, Video editing) - shape: wrong

1. **Exists as:** packages/video-ui.
2. **Feeds (planned lines):** cutright 16,336.
3. **In the package today:** CutRight src/ moved whole (generated index exporting every module).
4. **Side by side:** Single feeder; read package.json, index.ts, directory layout, lib/media.ts invoke sites, product-string counts.
5. **Best shape:** Editor component library + adapter interface. Today's package is not that. Verdict: It is the CutRight application frontend, not a component library: Home, Settings, legal config, demo mode, app name, GenRight client, and Tauri invokes of commands that exist only in the CutRight shell.
6. **Per app:**
   - cutright: **fits** - 8,694 of 9,022 distinctive lines present
   - genright: **wrong** - could not take the timeline or player without the CutRight engine contract, settings backend and playback commands
7. **Boundary problems:**
   - Depends on app-update, legal, legal-ui, license, onboarding, settings-ui, tauri-shell: an app shell's dependency set
   - lib/media.ts:651-677 invokes playback_* and media_scope_allow Tauri commands with no RightKit Rust side
   - lib/genright.ts (253 lines) is the CutRight->GenRight UI client inside a video package
   - views/Home, SettingsView, legal/legalConfig, demo/*, appName are shell
   - editor/vendor/hyperframes vendored helpers also relevant to media-gen-ui motion
8. **Before publication:**
   - Split: (1) editor components (Timeline, Inspector, ColourInspector, CaptionInspector, TranscriptPanel, Player, RangePreview) taking an injected engine adapter with no Tauri import; (2) the CutRight shell (views, settings, legal, demo) stays in the app
   - Replace the generated export-everything index with a deliberate API
   - Move genright.ts to a generation client package shared with media-gen-ui

Depth: deep. Confidence: high.

### RightKitMedia (swift, Video editing) - shape: missing

1. **Exists as:** not in RightKit.
2. **Feeds (planned lines):** cutright 1,859.
3. **In the package today:** not in RightKit.
4. **Side by side:** Single feeder; file list and sizes only.
5. **Best shape:** Undecided: depends on whether the Rust render path replaces the worker. Verdict: No RightKitMedia module in swift/RightKitSwift/Sources or Package.swift. CutRight's macOS media worker (AVFoundation timeline render, Vision, audio finish) has 46 matching lines anywhere.
6. **Per app:**
   - cutright: **gaps** - sidecars/macos-media-worker/*.swift (TimelineRenderService 683 lines) unported
   - heardright: **n/a** - uses RightKitAudio/RightKitSpeech
7. **Boundary problems:**
   - rightkit-reframe states "all Rust, no Swift" and replaced the Vision anchor worker; the Swift render/vision services may be superseded by the Rust path (suspicion; decide before porting)
   - silero-vad-macos.swift duplicates rightkit-vad (suspicion, 8 lines)
8. **Before publication:**
   - Decide whether the macOS worker is still CutRight's render path; if superseded mark DROP, otherwise create the module from AssetService/AudioService/VisionService with the wire Protocol.swift

Depth: light. Confidence: medium.

### rightkit-media-gen (cargo, Generative media) - shape: wrong

1. **Exists as:** crates/rightkit-media-gen.
2. **Feeds (planned lines):** genright 34,258.
3. **In the package today:** GenRight genright-gen moved whole with a `host` seam, plus studio voices, avatars, balances, stock, search.
4. **Side by side:** Single feeder (CutRight and PostRight are callers, not implementers). Read lib.rs, api.rs surface, ledger.rs against rightkit-spend, media.rs and host/h264.rs against rightkit-ffmpeg (diffed), providers/mod.rs.
5. **Best shape:** Provider trait, typed GenRequest/Job/Asset API, catalogue and routing; everything else delegated. Today's crate is the app. Verdict: A 34,600-line crate that is the GenRight engine: 18 provider adapters, jobs, catalogue, routing, library, credentials, spend layer, voices, and private copies of code other RightKit crates own. Its API is JSON Value in and out, so a second app adopts it only as "GenRight embedded".
6. **Per app:**
   - genright: **fits** - 11,442 of 18,501 distinctive lines present
   - cutright: **wrong** - consumes generation through the engine CLI (rightkit-pipeline); a typed Rust API (GenRequest -> job -> asset) is not offered
   - postright: **wrong** - same; dispatches briefs to the engine binary
7. **Boundary problems:**
   - ledger.rs (2,278 lines): three-cap reserve, device epochs, invoice reconciliation, caller budget over reserve_on, with tables genright_spend_attempts / genright_spend_meta; rightkit-spend has all four natively
   - media.rs duplicates rightkit-ffmpeg quickprobe; host/h264.rs duplicates rightkit-ffmpeg h264_rule (19 differing lines)
   - voices.rs (2,094) and qts/ and providers/qts.rs are the TTS product layer the plan maps to rightkit-tts
   - library.rs (1,257) is a second artifact library beside rightkit-library
   - providers/local_blender.rs (2,040), local_wan2gp, local_image, local_lipsync, musicgen, realesrgan are local generation, planned for rightkit-local-gen, which this crate depends on
   - providers::orthic_root() and a temp fallback named "Orthic Labs" (local_blender.rs:158); "genright-approval-v1" context string
   - api.rs: every function is (ctx, &Value) -> GenResult<Value>
8. **Before publication:**
   - Split by owner: provider trait + remote adapters + routing + catalogue (media-gen core); jobs on rightkit-job-queue; spend layer thinned onto rightkit-spend; voices to rightkit-tts; local providers to rightkit-local-gen; library onto rightkit-library; delete media.rs and host/h264.rs
   - Add a typed request/result API beside the JSON command surface
   - Remove organisation and app names from paths, table names and context strings (host supplies them)

Depth: deep. Confidence: high.

### @rightkit/media-gen-ui (npm, Generative media) - shape: wrong

1. **Exists as:** packages/media-gen-ui.
2. **Feeds (planned lines):** genright 21,626.
3. **In the package today:** GenRight src/ moved whole (generated index).
4. **Side by side:** Single feeder; read package.json, index.ts, directory tree, largest files, product strings. Compared MaskBrush with media-ui MaskPainter (see @rightkit/media-ui).
5. **Best shape:** Component library + typed client. Not today's package. Verdict: The GenRight application frontend including a 4,003-line browser mock of the engine, legal documents, onboarding/license/app-update wiring and brand data.
6. **Per app:**
   - genright: **fits** - 10,752 of 13,694 distinctive lines present
   - cutright: **wrong** - needs a generate panel and job list (it has its own PromptPanel + genright.ts) and cannot take them without the whole studio
7. **Boundary problems:**
   - lib/browser-mock-api.ts (4,003 lines) is test/demo scaffolding exported from a published package
   - studio-data/brands/brands.json and production/templates.json duplicate rightkit-prompt-recipes/data and name the owner's brands
   - components/MaskBrush + lib/mask-export duplicate @rightkit/media-ui MaskPainter + mask.ts
   - legal/documents, lib/license, lib/onboarding, lib/app-update*, appName are shell
   - CutRight video-ui has its own PromptPanel/genright.ts for the same engine
8. **Before publication:**
   - Split into components a second app can mount (Generate form from gen-params, JobCard/Jobs, Gallery, Voices, ModelsPanel) over an injected API client; shell stays in GenRight
   - Move the mock to a dev-only entry or the app
   - Use MaskPainter; delete MaskBrush
   - One generation client shared with video-ui

Depth: deep. Confidence: high.

### rightkit-local-gen (cargo, Generative media) - shape: gaps

1. **Exists as:** crates/rightkit-local-gen.
2. **Feeds (planned lines):** genright 12,312.
3. **In the package today:** GenRight genright-local-image moved (profiles, runtime, store, machine fit, memory, artifacts) + service_host/http.
4. **Side by side:** Single feeder; over 5,000 lines but read at module-list and Cargo level only, plus coverage rows showing where the providers went.
5. **Best shape:** All local generation runtimes behind the media-gen Provider trait, or an honest narrower name. Verdict: Local image generation runtime with machine fit and memory accounting is a coherent package. It is named for all local generation but holds only the image runtime; the other local providers are in rightkit-media-gen.
6. **Per app:**
   - genright: **fits** - 4,945 of 7,227 distinctive lines present; the absent ones are local_lipsync, musicgen, local_image, realesrgan providers, which are in rightkit-media-gen instead
7. **Boundary problems:**
   - Planned content split across two crates (see media-gen)
   - model_store.rs/paths.rs beside rightkit-models; remote-compute/models holds a second local model catalogue
   - lib.rs is 1,950 lines
8. **Before publication:**
   - Either rename to rightkit-local-image or receive the local providers from media-gen
   - One local model catalogue (with remote-compute/models and rightkit-models)

Depth: light. Confidence: medium.

### rightkit-prompt-recipes (cargo, Generative media) - shape: wrong

1. **Exists as:** crates/rightkit-prompt-recipes.
2. **Feeds (planned lines):** genright 12,135.
3. **In the package today:** GenRight genright-studio recipes, video_templates, contracts, characters, swap_templates, kdp, brands, catalog moved with their JSON data.
4. **Side by side:** Single feeder; read lib.rs, kdp.rs header, data directory, brands.json head. recipes.rs/contracts.rs internals not read.
5. **Best shape:** Pure prompt-contract and recipe library with data packs. Verdict: Prompt contracts per model and the recipe composer are reusable. The crate also ships the owner's brand list and a Kindle colouring-page workflow, and every module is a `studio.<area>.*` command handler bound to rightkit_media_gen::Context.
6. **Per app:**
   - genright: **fits** - 6,566 of 6,911 distinctive lines present
   - cutright: **wrong** - would inherit brand codes and KDP to get prompt contracts
   - postright: **n/a** - has its own brand/venture model
7. **Boundary problems:**
   - data/brands/brands.json names real brands ("Damned Designs", "Rotten Hand") with folder-matching segments: owner data in a library
   - kdp.rs + data/kdp: one product workflow (locked NB2 prompt, 2K, 1:1)
   - characters.rs data lists trainers, base models, LoRAs: catalogue data that ages; contracts/*.json per model likewise (should be versioned data, as platform-rules does)
   - Depends on rightkit-media-gen and rightkit-approvals to run commands
8. **Before publication:**
   - Keep contracts + validator + recipe composer + video templates as a pure library over data
   - Brands become host-supplied data; KDP moves to the GenRight app (or a recipes data pack)
   - Command handlers move to the engine layer

Depth: deep. Confidence: medium.

### rightkit-blender-worker (npm, Generative media) - shape: gaps

1. **Exists as:** packages/blender-worker (@rightkit/blender-worker, Python source, GPL-3.0-or-later).
2. **Feeds (planned lines):** genright 4,386.
3. **In the package today:** GenRight workers/blender moved 2026-11-01 (npm wrapper around Python).
4. **Side by side:** Single feeder; read README, package.json, file list, grep counts.
5. **Best shape:** Generic Blender job worker + data packs. Verdict: Job protocol, schemas and scene authoring are a usable worker. It also ships one product's geometry and look development.
6. **Per app:**
   - genright: **fits** - 3,216 of 3,493 distinctive lines present in RightKit (coverage tgt shows 0 only because the package name differs from the planned crate name)
7. **Boundary problems:**
   - fenrir_geometry.py, fenrir registration reference, recipes/samples/fenrir-sample-product.json, stonewash.py + build_stonewash_library, editorial_look/premium_look/approved_materials: 296 "fenrir" and 164 "stonewash" occurrences; job.schema.json mentions fenrir
   - Python package named `renderright`
   - GPL-3.0 package beside MIT/Apache crates; providers/local_blender.rs (2,040 lines, in media-gen) is its MIT-side driver: licence boundary is the process boundary and should be stated
   - Planned as cargo `rightkit-blender-worker`; exists as npm
8. **Before publication:**
   - Split generic worker (contract, worker, scene, pngcheck, install, qualify, schemas) from product look-dev (fenrir, stonewash, product_* modules) which becomes a host-supplied assets/recipes pack via RIGHTKIT_BLENDER_ASSETS_DIR
   - Fix the plan name to the npm package

Depth: light. Confidence: medium.

### rightkit-remote-compute (cargo, Generative media) - shape: wrong

1. **Exists as:** crates/rightkit-remote-compute.
2. **Feeds (planned lines):** genright 2,681.
3. **In the package today:** GenRight model_management/remote.rs + rental.rs, plus models/ (local model catalogue, installs, governor, hardware, blender/sdcpp/wan2gp).
4. **Side by side:** Single feeder; read lib.rs and module list.
5. **Best shape:** remote + rental only. Verdict: Rented GPU worker lifecycle (SSH, Vast.ai) with a time-billed spend bracket is a clear package. Half the crate is local model management, and worker records live in media-gen's settings table.
6. **Per app:**
   - genright: **fits** - remote.rs 628/889, rental.rs 640/664 present
   - heardright: **n/a** - local models through rightkit-models
7. **Boundary problems:**
   - models/ (2,667 lines: catalogue, install, memory governor, hardware fit, `studio.models.*` commands, qts install) is local, not remote
   - lib.rs get_json/set_json read rightkit_media_gen::store settings: remote compute cannot be used without media-gen's database
   - rental.rs reserves and settles spend: should be on rightkit-spend directly (not read in detail; suspicion that it goes through media-gen ledger)
8. **Before publication:**
   - Move models/ to rightkit-local-gen (or rightkit-models for catalogue/install)
   - Worker registry behind a small store trait
   - Spend bracket on rightkit-spend

Depth: light. Confidence: medium.

### rightkit-tts (cargo, Generative media) - shape: best

1. **Exists as:** crates/rightkit-tts.
2. **Feeds (planned lines):** genright 4,338.
3. **In the package today:** RightKit runtime (system voices, QTS worker client, model manifest, chunking) that GenRight's genright-qts already wraps (=0.2.3).
4. **Side by side:** Single feeder; read lib.rs and the genright-qts Cargo description.
5. **Best shape:** As today, plus the voice library if moved. Verdict: Clean feature boundaries (system / client / engine) and GenRight already adopts it through a 700-line facade. The 3,284-line studio voices layer was not put here but in rightkit-media-gen.
6. **Per app:**
   - genright: **fits** - genright-qts is a facade over the crate; voices.rs is product layer
   - heardright: **n/a** - Swift RightKitTTS; not compared
7. **Boundary problems:**
   - voices.rs (voice library, cloning, studio commands) planned EXTEND here; it is in rightkit-media-gen/src/voices.rs (2,094 lines). Either is defensible; the plan and the code disagree
   - rightkit-media-gen/src/qts/* re-exports rightkit_tts under GenRight paths: a facade inside a shared crate
8. **Before publication:**
   - Decide owner of the voice library (recommended: a `voices` module here, since it needs no provider code) and drop the qts facade from media-gen

Depth: light. Confidence: medium.

### rightkit-tts-qwen (cargo, Generative media) - shape: best

1. **Exists as:** crates/rightkit-tts-qwen.
2. **Feeds (planned lines):** no surveyed app.
3. **In the package today:** Vendored Qwen3-TTS native inference (GGUF/GGML); no app feeds it directly.
4. **Side by side:** No app code. Read lib.rs header only.
5. **Best shape:** As today. Verdict: Engine crate behind rightkit-tts `engine`; no app code and no app decisions found at the level read.
6. **Per app:**
   - genright: **fits** - through rightkit-tts engine feature
7. **Boundary problems:** none found.
8. **Before publication:** none.

Depth: light. Confidence: low.

### rightkit-tts-qwen-sys (cargo, Generative media) - shape: best

1. **Exists as:** crates/rightkit-tts-qwen-sys.
2. **Feeds (planned lines):** no surveyed app.
3. **In the package today:** Generated bindings (macOS arm64, Windows x64) + build script.
4. **Side by side:** Not read beyond file list.
5. **Best shape:** As today. Verdict: FFI crate; nothing to compare.
6. **Per app:**
   - genright: **fits** - transitive
7. **Boundary problems:**
   - Bindings exist for two targets only
8. **Before publication:** none.

Depth: light. Confidence: low.

### @rightkit/media-ui (npm, Video editing) - shape: gaps

1. **Exists as:** packages/media-ui.
2. **Feeds (planned lines):** genright 282, scraperight 698.
3. **In the package today:** Brand-neutral MediaPlayer, image viewer/editor, MaskPainter, VoiceCallBar (RightKit implementation).
4. **Side by side:** GenRight `src/components/MaskBrush.tsx:1-30` (tools brush/eraser, size, invert, clear; exports B/W PNG at source pixel size via onSave) against `packages/media-ui/src/MaskPainter.tsx` + `mask.ts` (grep: no `invert`). ScrapeRight `src/lib/mediaSelection.ts:1-20` is transcribe/audio/video/images mode logic, unrelated to media-ui. Verified by reading.
5. **Best shape:** As today plus invert. Verdict: Component library with injected adapters and no shell dependencies: the shape the two big UI packages should follow. MaskPainter lacks the invert control GenRight's MaskBrush has.
6. **Per app:**
   - genright: **gaps** - MaskBrush.tsx (brush, eraser, size, invert, clear) vs MaskPainter (brush, erase, size, clear): no `invert` in MaskPainter.tsx or mask.ts; GenRight already imports canvasToArrayBuffer/validateExportCanvasSize from the package
   - scraperight: **n/a** - mediaSelection.ts is download-mode selection for the queue, lives in @rightkit/library-ui; tests are app tests; mis-mapped
   - heardright: **n/a** - not compared
   - viewright: **n/a** - not compared in this family
7. **Boundary problems:**
   - ScrapeRight mediaSelection/mediaAdapters mapped ADOPT here belong to library-ui (already there)
   - media-gen-ui still exports its own MaskBrush
8. **Before publication:**
   - Add invert to MaskPainter/exportMaskPng
   - Remove MaskBrush from media-gen-ui

Depth: deep. Confidence: medium.

## Family summary

Counts by shape: best 4, gaps 18, missing 1, wrong 6 (of 29).

| Package | Shape | Main reason |
|---|---|---|
| rightkit-spend | gaps | The reserve/commit ledger with multi-cap, device epoch, caller budget, invoice reconcile and sealed approved budgets is the right general shape and no app code sits in it |
| rightkit-ffmpeg | gaps | The binary resolution, runner and probe core is shared and sound |
| rightkit-compose | gaps | The pure composition document and evaluate() are app-neutral |
| rightkit-motion | gaps | The FrameSource driver + project server + browser source is a clean seam and GenRight already adopts it |
| rightkit-review | gaps | The verdict/rule/disposition core is the shared shape and both apps use it unchanged |
| rightkit-playback | gaps | For what it is (make a file playable in a webview without re-encoding video) it is complete and ScrapeRight already uses it |
| rightkit-pipeline | wrong | Not in the assigned list but flagged in the task: it holds one app's client for one other app's CLI |
| rightkit-video-project | gaps | A sound project layer (directory layout, lock, BLAKE3 source identity, ingest, render orchestration) that a second editing app could adopt |
| rightkit-timeline | gaps | Typed actions with inverses, revision store, render plan and typed ffmpeg args are a good general timeline core |
| rightkit-autoedit | gaps | Transcript-driven cut planning with scoped rules is reusable by any app with a transcript and a timeline |
| rightkit-shorts | gaps | Scoring profile as versioned JSON and LLM ranking validated onto word boundaries is reusable |
| rightkit-reframe | gaps | Evidence -> plan -> approval -> render with a pinned, hashed model download is a clean package |
| rightkit-color | best | Pure plan-to-filter-chain library (colour space detection, HDR/Apple Log to SDR, cube LUTs, auto correct, shot match, grade) with only ffmpeg and fs deps; nothing product-specific found beyond doc wording. |
| rightkit-captions | gaps | Cue building, style presets and drawtext burn-in on top of rightkit-transcript and rightkit-typeset is the right layering |
| rightkit-compositor | gaps | A self-contained wgpu layer compositor with no RightKit dependencies |
| rightkit-gpu | gaps | Right idea (one device policy for the suite) but nothing in RightKit or the surveyed apps depends on it, so the shape is unproven. |
| @rightkit/video-ui | wrong | It is the CutRight application frontend, not a component library: Home, Settings, legal config, demo mode, app name, GenRight client, and Tauri invokes of commands that exist only in the CutRight shell. |
| RightKitMedia | missing | No RightKitMedia module in swift/RightKitSwift/Sources or Package.swift |
| rightkit-media-gen | wrong | A 34,600-line crate that is the GenRight engine: 18 provider adapters, jobs, catalogue, routing, library, credentials, spend layer, voices, and private copies of code other RightKit crates own |
| @rightkit/media-gen-ui | wrong | The GenRight application frontend including a 4,003-line browser mock of the engine, legal documents, onboarding/license/app-update wiring and brand data. |
| rightkit-local-gen | gaps | Local image generation runtime with machine fit and memory accounting is a coherent package |
| rightkit-prompt-recipes | wrong | Prompt contracts per model and the recipe composer are reusable |
| rightkit-media-pipeline | gaps | Editable step DAGs over media-gen with fan-out and native subtitle/compose steps is a reasonable package |
| rightkit-blender-worker | gaps | Job protocol, schemas and scene authoring are a usable worker |
| rightkit-remote-compute | wrong | Rented GPU worker lifecycle (SSH, Vast.ai) with a time-billed spend bracket is a clear package |
| rightkit-tts | best | Clean feature boundaries (system / client / engine) and GenRight already adopts it through a 700-line facade |
| rightkit-tts-qwen | best | Engine crate behind rightkit-tts `engine`; no app code and no app decisions found at the level read. |
| rightkit-tts-qwen-sys | best | FFI crate; nothing to compare. |
| @rightkit/media-ui | gaps | Component library with injected adapters and no shell dependencies: the shape the two big UI packages should follow |

### Findings that cut across packages

1. **Same code in two RightKit packages (violates "no duplicate code").** Verified: GenRight container probe in `rightkit-ffmpeg/src/quickprobe.rs` and `rightkit-media-gen/src/media.rs`; H.264 rule in `rightkit-ffmpeg/src/h264_rule.rs` and `rightkit-media-gen/src/host/h264.rs`; reframe track in `rightkit-reframe/src/track.rs` and `rightkit-video-project/src/reframe_track.rs`; `Ticks` in `rightkit-timeline` (i64) and `rightkit-compose` (u64); H.264 `Encoder` enum in `rightkit-timeline` and `rightkit-ffmpeg`; mask painter in `media-ui` and `media-gen-ui`; brands data in `rightkit-prompt-recipes/data` and `media-gen-ui/src/studio-data`; wgpu device context in `rightkit-compositor` and `rightkit-gpu`. Same capability re-implemented: `rightkit-media-gen/src/ledger.rs` against `rightkit-spend` group/reconcile/budget.
2. **The earlier line match misread four shared packages.** `rightkit-spend`: CutRight's 4,345 lines are a decision ledger, not spend, and are in no package. `rightkit-compose`: GenRight's "present" code is logo stamping; its real compose is in media-pipeline. `rightkit-motion`: CutRight's present code is contact sheet and thumbnail commands, not motion. `rightkit-playback`: CutRight's absent code is a native timeline preview, a different capability that `@rightkit/video-ui` needs and RightKit lacks.
3. **Where CutRight and GenRight really differ.** Encoder selection: CutRight pre-qualifies with a trial encode, GenRight has typed failure, exclude-and-retry and opt-in MPEG-4; best shape is both in one policy, not either app's. Assembling a video: CutRight's timeline -> render plan -> typed ffmpeg args (with a preview that shares the path) is the general shape; GenRight's compose step is a fixed recipe. Contact sheets: the neutral `sheet` module's tile-size floor is the better default, CutRight's revision binding the better behaviour. Engine-to-engine calls: PostRight's process discipline plus CutRight's handshake and idempotency.
4. **Whole applications published as packages.** `@rightkit/video-ui` and `@rightkit/media-gen-ui` are the two app frontends with a generated export-everything index; `rightkit-media-gen` is the GenRight engine. They move the lines out of the apps but a second app cannot adopt them without the first app's product. `@rightkit/media-ui` (injected adapters, no shell deps) shows the intended shape.
5. **Platform facts in three places** (`rightkit-timeline` Preset, `rightkit-captions` safe_zones.json, `rightkit-review` render_qa PlatformRules) beside `rightkit-platform-rules`, whose rule is that code holds no platform limit.
6. **Engine command handlers have no single rule.** autoedit, shorts and motion carry `engine-commands` features; video-project, captions and color do not, so about 9,300 lines of CutRight command code (media_cmd, bundle_cmd, project_cmd, captions, colour) are unported; the generative crates dispatch on `studio.*` names over `serde_json::Value`.
7. **Product and owner content in shared packages.** `cutright.*` schema ids across the video crates (free to rename, no live users), `genright_*` table names and "Orthic Labs" paths in media-gen, real brand names and a KDP workflow in prompt-recipes, Fenrir and Stonewash look-dev in the Blender worker, a compiled default LLM model id in shorts.
8. **Unhomed code:** CutRight decision ledger / edit plans / relink (4,345), CutRight playback session (1,819), PostRight dispatch (2,521), CutRight macOS Swift worker (1,859, may be superseded), GenRight motion templates (653).

### Limits of this review

Deep reading was done for every package with two or more feeders and for the cross-package comparisons. For single-feeder packages over 5,000 planned lines (`rightkit-autoedit`, `rightkit-local-gen`, and the internals of `rightkit-video-project`, `rightkit-timeline`, `rightkit-prompt-recipes`, `rightkit-media-pipeline`) reading was at public-surface, module and dependency level, not function by function; their entries say so. Nothing was built or run.
