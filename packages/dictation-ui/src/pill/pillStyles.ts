export const CSS = `
/* Typed custom property for the processing pulse and launch bloom. Recording
   deliberately uses only its static blue border. */
@property --rim{ syntax:'<number>'; inherits:true; initial-value:0; }

.hr-pill-root{
  /* Pill accent is BLUE (Adrian, 2026-07-02) — deliberately distinct from the
     app's ember brand: the pill is the live voice surface and owns its own cool
     accent. Var names kept as --ember to avoid a noisy rename. */
  --ember:#5AA0FF; --ember-rest:rgba(90,160,255,.35); --edge:rgba(90,160,255,.28);
  --coffee:#080808; --coffee-2:#121212; --fg:#F5F2EE; --muted:#9D9791; --err:#E5675B;
  /* Swift HRTokens.success / .error (PillTokens.swift:68-69) — the status and
     error capsules are filled with these, not tinted text on coffee. */
  --ok:#3FD06A;
  /* Swift's PillPanel controller uses smoothstep (t² × (3 − 2t)) for every
     capsule resize. These stops are that curve sampled at 10% intervals. */
  --hr-smoothstep:linear(0,.028,.104,.216,.352,.5,.648,.784,.896,.972,1);
  /* Critically damped equivalent of SwiftUI PillMotion.settled (.15, 1, 0). */
  --hr-aux-settled-spring:linear(0,.124,.343,.547,.704,.815,.89,.938,.969,.987,1);
  position:fixed; inset:0; display:flex; align-items:flex-end; justify-content:center;
  background:transparent; -webkit-user-select:none; user-select:none; overflow:visible;
  font-family:'Hanken Grotesk',system-ui,-apple-system,sans-serif;
}
/* The pill window is a FIXED envelope larger than any single state (see
   pill_resize / webview_create_pill in Rust). The pill is bottom-anchored inside
   it so morphs and hover docks grow UPWARD into the transparent headroom while
   the bottom edge stays pinned above the dock/taskbar — on both platforms now
   (Windows already bottom-anchored; macOS joined it when the window stopped
   hugging content). */
.hr-pill-root.win{align-items:flex-end}
.hr-wrap{ display:inline-flex; flex-direction:column; align-items:center; }

.hr-pill{
  display:inline-flex; align-items:center; justify-content:center; gap:9px;
  position:relative; isolation:isolate; overflow:hidden;
  height:28px; padding:0 11px; border-radius:999px;
  background:var(--coffee);
  /* Keep the near-black fill out from underneath the antialiased blue edge.
     Without this, transparent WebView compositing turns the fill beneath the
     rounded border into a dark fringe on light desktop backgrounds. */
  background-clip:padding-box;
  color:var(--fg); white-space:nowrap;
  border:1px solid rgba(255,255,255,.12);
  transition:border-color 140ms ease, background-color 140ms ease, opacity 140ms ease;
}
.hr-pill::before{
  content:none;
}
.hr-pill > *{position:relative; z-index:1}
/* RIM GLOW — shared by the idle launch bloom and processing pulse. Recording
   keeps only its static blue border because the orange bars already own live
   audio feedback. A single ::after layer avoids an external drop shadow.
   overflow:hidden + isolation:isolate on .hr-pill (above) clip it to the pill's own
   bounds, which also keeps it inside the window (the NSPanel hugs the pill exactly,
   see PillOverlay's ResizeObserver comment) without needing an outer glow that would
   get cut off at the window edge. */
.hr-pill::after{
  content:""; position:absolute; inset:0; z-index:0; border-radius:inherit;
  pointer-events:none; opacity:var(--rim, 0);
  box-shadow:
    inset 0 0 0 1px var(--ember),
    inset 0 0 var(--intro-blur,9px) var(--intro-spread,1px)
      var(--intro-glow,rgba(90,160,255,.55));
  /* !important: the app-global reduced-motion reset in index.css leaks into
     this shared WebView bundle and would otherwise kill the launch handoff. */
  transition:opacity var(--rim-ms,80ms) ease-out !important;
}
/* Recording: 32px at rest; on hover it grows to the unchanged 44px action dock. */
/* Compact recording matches Swift's 96x32 compactRecordingSize. Only this
   unhovered recording state was tightened. Border 1.5px at accent .82 per Swift's
   borderWidth/borderColor for .recording (PillRootView.swift:158,140). */
.hr-pill.recording{ width:96px; height:32px; padding:0; gap:6px;
  border-width:1.5px; border-color:rgba(90,160,255,.82); }
/* Orange bars are the sole audio-reactive treatment. */
.hr-pill.recording::after{
  opacity:0;
  box-shadow:none;
  transition:none !important;
}
/* Swift sets the timer in the DISPLAY face (Tanker) at 18pt, not mono
   (RecordingCore.timer, PillRootView.swift:236-243). Tabular numerals stay on
   so the mm:ss width does not jitter between digits. */
.hr-pill .timer{
  font-family:'Tanker','Spline Sans Mono',ui-monospace,monospace;
  font-size:18px; font-weight:400; letter-spacing:0; font-variant-numeric:tabular-nums;
  line-height:1; color:var(--fg);
}
/* RECORDING BARS — 1:1 with the Swift pill's RecordingBars (PillRootView.swift
   :250-264): 6 capsule bars, 2.2px wide, 2.2px gap, inside a fixed 25x22 frame,
   filled EMBER. Ember here is deliberate and is the one warm accent on an
   otherwise blue pill: it marks live voice. Heights come from React per frame
   (recordingBarPixelHeight), so no CSS animation drives them — the mic does. */
.hr-bars{
  display:inline-flex; align-items:center; justify-content:center; gap:2.2px;
  width:25px; height:22px; flex:none;
}
.hr-bars i{width:2.2px; border-radius:1.1px; background:#FF5630; flex:none}
/* PROCESSING WAVE — Swift ProcessingWaveform (PillRootView.swift:266-285): 11
   white bars, 2px wide, 2px gap, 44x22 frame. Mic-independent sweep: there is
   no live signal while transcribing, so it reads as "thinking", not reacting. */
.hr-wave{
  display:inline-flex; align-items:center; justify-content:center; gap:2px;
  width:44px; height:22px; flex:none;
}
.hr-wave i{width:2px; border-radius:1px; background:#fff; flex:none}

/* IDLE — match Swift exactly at rest: #080808 fill, no interior bloom, and the
   effective 60% blue hairline produced there by accent(30/31 = .9677419355,
   PillRootView.swift:153) × stroke(.62, PillRootView.swift:77). This was
   raised from .48 (pill-contrast-parity, 2026-09-01): #5AA0FF at .48 over
   #080808 is only 2.482:1, under the WCAG 3:1 graphical-object bar; .48 was
   accent(.78) × stroke(.62). .60 clears the bar at 3.289:1 (min alpha for
   exactly 3:1 is .5603). Recording, error, and status borders are
   unchanged — see their own Swift terms. */
.hr-pill.idle{
  width:64px; height:10px; padding:0; gap:8px;
  background:var(--coffee); --rim:0;
  border-color:rgba(90,160,255,.60); border-width:1.5px;
}
.hr-pill.idle.hover,
.hr-pill.idle.preparing{ --rim:0; }
.hr-pill.idle.preparing{
  border-color:var(--ember-rest);
  animation:hrPrepPulse 1.2s ease-in-out infinite;
}
/* First-launch bloom: an ember glow that swells from nothing INSIDE the pill
   and fades out. overflow:hidden on .hr-pill keeps every bit of it contained. */
/* The launch bloom is driven by JS (rAF + inline custom properties) in PillOverlay, NOT this
   CSS — the @keyframes version never rendered on Windows WebView2 (reduced-motion
   + transparent-overlay compositing). This rule only zeroes the idle rim so it
   doesn't show under the JS bloom; the bloom itself drives this SAME ::after rim.
   Do NOT re-add an animation property here — a running CSS animation outranks
   inline style and would suppress the JS bloom. */
.hr-pill.idle.intro{ --rim:.5; }
/* Swift uses a FIXED 124x44 here (PillPanel.swift:382), but hover docks must
   size from their rendered controls with no fixed horizontal slack — LOCKED by
   assert-release-contracts (ca1b13a1). width:auto also removes the clipping risk
   a hard width carries: .hr-pill is overflow:hidden, so a web timer wider than
   Tanker's metrics would cut off the stop/send buttons. Height still matches. */
.hr-pill.idle.hover{
  width:auto; height:44px; padding:0 4px; gap:0;
  /* The native window takes one geometry step so hit testing stays reliable.
     Paint the capsule on a compositor-only layer inside that final frame: this
     removes the one-frame shell snap without streaming intermediate resizes. */
  background:transparent;
  border-color:transparent;
}
.hr-pill.idle.hover::before{
  content:""; position:absolute; inset:0; z-index:0; border-radius:inherit;
  pointer-events:none; background:var(--coffee); background-clip:padding-box;
  border:1px solid rgba(90,160,255,.68);
  transform-origin:50% 100%;
  animation:hrIdleShellExpand 150ms var(--hr-smoothstep) both;
}
/* Scale factors are the collapsed capsule over the hover capsule. The HEIGHT
   ratio is a real constant (10/44 — both are fixed in CSS above). The WIDTH
   ratio is NOT: the hover dock is width:auto, so its width is whatever the
   mounted buttons measure (~84px for the two-button dock), not the 124px Swift
   fixed size the old hardcoded .5161 was computed against. That mismatch made
   the shell start ~43px wide instead of 64px and snap outward — the broken idle
   hover. PillOverlay measures the dock in a layout effect and publishes the real
   ratio as --idle-collapse-sx; the literal here is only a pre-measure fallback. */
@keyframes hrIdleShellExpand{
  from{transform:scale(var(--idle-collapse-sx,.7619),.2273); opacity:.82}
  to{transform:scale(1); opacity:1}
}
/* Hover dock: two icon buttons with the SAME circular treatment as recording/
   success — contained round hover halos (no bleed onto the neighbour), and the
   pill hugs them (width:auto) so the states read consistently. */
.hr-pill.idle.hover .hr-actions{ gap:4px }
.hr-pill.idle.hover .hr-btn{ width:36px; height:36px; border-radius:50% }
/* Swift PillIconButton: a 17x17 glyph centred in a 36x36 circular hit target
   (PillRootView.swift:299-300). The previous 30px glyph filled the circle and
   read much heavier than the mac pill. */
.hr-pill.idle.hover .hr-btn svg{ width:17px; height:17px }
.hr-hover-anchor{
  position:absolute!important; left:50%; bottom:0; z-index:0!important;
  transform:translateX(-50%); pointer-events:none;
}
.hr-pill.idle .hr-hover-anchor{width:64px;height:10px}
/* MUST equal the compact .hr-pill.recording box: this anchor is what
   reportPillHitbox measures while un-hovered, so it is the rect the native
   hover gate tests the cursor against. Stale values here = hover firing on the
   wrong area. */
.hr-pill.recording .hr-hover-anchor{width:96px;height:32px}
/* Recording / done hover: same big-icon treatment for the action buttons. The
   pill auto-hugs its content (bars+timer / check + actions) so no spare space —
   just enlarge the icons to match the idle dock. */
/* Swift RecordingControls: HStack spacing 3, buttons 36x36, glyph 17x17
   (PillRootView.swift:151,163,299-300). */
.hr-pill.recording.hover .hr-actions,
.hr-pill.success.hover .hr-actions{ gap:3px }
.hr-pill.recording.hover .hr-btn,
.hr-pill.success.hover .hr-btn{ width:36px; height:36px }
.hr-pill.recording.hover .hr-btn svg,
.hr-pill.success.hover .hr-btn svg{ width:17px; height:17px }
/* ONE shared action-dock geometry for every hover state: same height + same edge
   padding so idle, recording, and success are visually identical docks. */
/* Swift recordingHoverSize is a fixed 189x44 with (44-36)/2 = 4px edge padding
   (PillPanel.swift:131, PillRootView.swift:163). */
/* Content-sized for the same locked reason as the idle dock above; Swift's
   measured 189px is not imposed as a floor either, since that is still fixed
   horizontal slack. Height and padding match Swift. */
.hr-pill.recording.hover{ width:auto; height:44px; padding:0 4px; gap:3px; }
.hr-pill.success.hover{ width:auto; height:44px; padding:0 4px; }
@keyframes hrPrepPulse{0%,100%{opacity:.72} 50%{opacity:1}}
.hr-spinner{width:16px; height:16px; border-radius:50%; border:2px solid rgba(90,160,255,.28); border-top-color:var(--ember); animation:hrSpin .7s linear infinite !important}
.hr-prep-label{font-size:12px; font-weight:600; color:var(--fg); padding-right:8px}
@keyframes hrSpin{to{transform:rotate(360deg)}}

/* PROCESSING — small ROUND pill (same 36px circle as success/error, via the
   shared .glyph class) with a blue spinner and a gently pulsing blue rim:
   thinking, not reacting. */
/* Swift processingCapsuleSize is 72x32 (PillRootView.swift:14) holding the
   44x22 wave — NOT the old 36px round glyph. */
.hr-pill.processing{
  width:72px; height:32px; padding:0; gap:0;
  animation:hrRimPulse 1.6s ease-in-out infinite !important;
}
@keyframes hrRimPulse{0%,100%{--rim:.3} 50%{--rim:.6}}

/* STATUS — a GREEN success capsule, matching the Swift pill exactly
   (PillRootView.swift capsuleBackground -> HRTokens.success, statusContent text
   at black .86, borderColor .black for .status). Adrian re-decided this on
   2026-07-25, superseding the earlier blue-edge lock (fdc09772, 2026-07-09):
   "it's green for [success]... the blue border is just too bright and big".
   The contract in assert-release-contracts was rewritten to lock THIS intent.
   Deliberately still text-only — no checkmark glyph, because the legacy-success
   downgrade contract forbids reintroducing one. */
.hr-pill.status{
  height:32px; min-width:74px; width:var(--status-width,74px); padding:0 13px; gap:0;
  --rim:0;
  background:var(--ok);
  /* Swift: borderColor .black at opacity(0.62 + 0.38 * introBloom), and
     introBloom is 0 at rest -> black .62, 1px (borderWidth is 1 for every mode
     except recording). NOT .86 — that is statusContent's TEXT opacity, and
     reusing it here produces exactly the too-heavy edge Adrian objected to. */
  border-color:rgba(0,0,0,.62);
}
/* Cancellation is terminal but not successful: retain the readable status
   capsule while restoring its red semantic treatment. */
.hr-pill.status.cancelled{
  background:var(--err);
}
.hr-status-label{
  font-size:13px; font-weight:750; letter-spacing:0; line-height:1;
  color:rgba(0,0,0,.86);
  animation:hrFadeIn .22s cubic-bezier(.22,1,.36,1) both;
}

/* STATUS / ERROR: terminal success is text ("Pasted", "Sent", "Saved", ...);
   errors use the same filled capsule treatment as macOS. */
.hr-pill.status, .hr-pill.error{ --rim:0; }
/* No hover scale on processing/error: Swift only expands idle and recording
   (hoverExpandable, PillRootView.swift:47) — terminal states hold still. */

/* ERROR — match macOS statusCapsuleSize: 32px red capsule, 12px warning
   icon, 7px spacing, 13px horizontal padding. */
.hr-pill.error{
  height:32px; min-width:82px; width:auto; padding:0 13px; gap:0;
  background:var(--err); border-color:rgba(0,0,0,.62);
}
.hr-error-content{display:inline-flex; align-items:center; gap:7px; color:rgba(0,0,0,.86)}
.hr-error-label{font-size:13px; font-weight:750; letter-spacing:0; line-height:1}
.hr-ico{width:20px; height:20px; display:inline-flex; align-items:center; justify-content:center; flex:none}
.hr-ico svg{width:12px; height:12px; fill:currentColor}
/* Status label fades in rather than popping when the text swaps. */
@keyframes hrFadeIn{from{opacity:0} to{opacity:1}}

/* Action buttons (idle / recording hover). */
.hr-actions{
  display:inline-flex; align-items:center; gap:4px;
  animation:hrHoverDockIn 150ms var(--hr-smoothstep) both;
}
@keyframes hrHoverDockIn{
  from{opacity:0; transform:translateY(2px) scale(.97)}
  to{opacity:1; transform:translateY(0) scale(1)}
}
.hr-idle-actions,.hr-recording-actions,.hr-hover-control{
  opacity:0; pointer-events:none; visibility:hidden; position:absolute;
  transform:scale(.96);
  transition:opacity 150ms var(--hr-smoothstep), transform 150ms var(--hr-smoothstep), visibility 0s linear 150ms;
}
.hr-pill.hover .hr-idle-actions,
.hr-pill.hover .hr-recording-actions,
.hr-pill.hover .hr-hover-control{
  opacity:1; pointer-events:auto; visibility:visible; position:relative; transform:scale(1);
  transition-delay:0s, 0s, 0s;
}
.hr-pill.hover.collapsing .hr-idle-actions,
.hr-pill.hover.collapsing .hr-recording-actions,
.hr-pill.hover.collapsing .hr-hover-control{
  opacity:0; pointer-events:none; visibility:hidden; position:absolute; transform:scale(.96);
  transition-delay:0s, 0s, 260ms;
}
.hr-btn{
  width:26px; height:26px; display:inline-flex; align-items:center; justify-content:center;
  border:0; padding:0; border-radius:50%; cursor:pointer; background:transparent; color:var(--fg);
  transition:background-color 120ms ease, color 120ms ease, transform 120ms ease;
}
.hr-btn:disabled{pointer-events:none}
.hr-btn{position:relative}
.hr-hover-control{position:absolute}
.hr-btn:hover,.hr-btn.jshover{background:rgba(255,255,255,.12); transform:scale(1.06)}
.hr-btn:active{transform:scale(.97)}
.hr-btn svg{width:16px; height:16px; fill:currentColor; transition:transform 120ms ease}
/* Per-action colour: the icon is tinted at rest; on hover the button fills with
   its action colour and the glyph flips to black. start/send = green, cancel +
   stop = red (destructive/halt), recent = utility grey. */
.hr-btn.c-green{color:#3FD06A}
.hr-btn.c-green:hover,.hr-btn.c-green.jshover{background:#3FD06A; color:#000}
.hr-btn.c-red{color:var(--err)}
.hr-btn.c-red:hover,.hr-btn.c-red.jshover{background:var(--err); color:#000}
/* Swift stop.fill tint is amber #FF9F0A (PillRootView.swift:156), not blue-on-red. */
.hr-btn.c-orange{color:#FF9F0A}
.hr-btn.c-orange:hover,.hr-btn.c-orange.jshover{background:#FF9F0A; color:#000}
/* Swift idle dock tints: house = ember, clock = white (PillRootView.swift:79,89). */
.hr-btn.c-ember{color:#FF5630}
.hr-btn.c-ember:hover,.hr-btn.c-ember.jshover{background:#FF5630; color:#000}
.hr-btn.c-white{color:#fff}
.hr-btn.c-white:hover,.hr-btn.c-white.jshover{background:#fff; color:#000}
.hr-btn.c-blue{color:var(--ember)}
.hr-btn.c-blue:hover,.hr-btn.c-blue.jshover{background:var(--ember); color:#041022}
.hr-btn.c-grey{color:var(--muted)}
.hr-btn.c-grey:hover,.hr-btn.c-grey.jshover{background:rgba(255,255,255,.16); color:var(--fg)}
@keyframes hrTipIn{to{opacity:1; transform:translateY(0)}}
.hr-btn.play svg{fill:currentColor; stroke:none}
.hr-btn.text{width:auto; padding:0 9px; font-size:12px; font-weight:600; gap:5px}

.hr-popover{
  width:286px; margin-bottom:8px; padding:10px;
  border-radius:13px; color:var(--fg); background:rgba(16,16,15,.92);
  border:1px solid rgba(90,160,255,.28);
  text-align:left; backdrop-filter:blur(18px) saturate(135%);
  -webkit-backdrop-filter:blur(18px) saturate(135%);
  animation:hrPopoverIn 130ms cubic-bezier(.22,1,.36,1) both;
}
.hr-popover.screenshot{width:260px}
.hr-popover.notice{width:auto; max-width:180px; padding:7px 10px; text-align:center}
.hr-popover.hint{width:206px; padding:8px 9px}
.hr-popover-title{font-size:12px; font-weight:750; line-height:1.2; color:rgba(247,244,239,.94)}
.hr-popover-copy{margin-top:3px; font-size:11.5px; font-weight:600; line-height:1.28; color:rgba(247,244,239,.76)}
.hr-popover-copy.compact{margin-top:0; font-size:11px; line-height:1.2}
.hr-popover-actions{display:flex; flex-wrap:wrap; gap:6px; margin-top:9px}
.hr-popover.hint .hr-popover-actions{margin-top:7px}
.hr-popover-actions button{
  height:24px; border:0; border-radius:999px; padding:0 8px; cursor:pointer;
  color:rgba(247,244,239,.92); background:rgba(90,160,255,.20); font:750 11px/1 'Hanken Grotesk',system-ui,sans-serif;
}
.hr-popover-actions button.ghost{color:rgba(247,244,239,.62); background:transparent}
.hr-popover-actions button:hover{background:rgba(90,160,255,.30)}
@keyframes hrPopoverIn{from{opacity:0; transform:translateY(5px) scale(.985)} to{opacity:1; transform:translateY(0) scale(1)}}

/* The status capsule is an OPAQUE green fill, so these fallbacks only need to
   restate it — repainting it coffee here would put black text on black. */
@supports not ((backdrop-filter:blur(1px)) or (-webkit-backdrop-filter:blur(1px))){
  .hr-pill.status{background:var(--ok)}
  .hr-popover{background:rgba(16,16,15,.96)}
}

@media (prefers-reduced-transparency: reduce), (prefers-contrast: more){
  .hr-pill.status{background:var(--ok)}
  .hr-popover{
    background:rgba(16,16,15,.98);
    backdrop-filter:none; -webkit-backdrop-filter:none;
  }
}

@media (forced-colors: active){
  .hr-pill,.hr-popover{
    background:Canvas; color:CanvasText;
    border:1px solid ButtonText;
    backdrop-filter:none; -webkit-backdrop-filter:none;
  }
  .hr-pill::before,.hr-pill::after{content:none}
  .hr-btn,.hr-popover-actions button{color:ButtonText; background:Canvas}
}

@media (prefers-reduced-motion: reduce){
  /* Functional STATUS motion stays on — WCAG 2.3.3 exempts essential feedback:
     the loading spinner (hrSpin), the launch bloom (hrIntroBloom), the recording
     rim BREATHING with your voice (the ::after transitions driven by live --rim),
     and the processing pulse (hrRimPulse). Windows deliberately overrides its
     global Animation effects setting for the complete pill choreography: this
     is a product surface, not incidental browser animation. macOS retains its
     accessibility fallback for decorative hover/dock easing. */
  .hr-pill-root:not(.win) .hr-pill,.hr-pill-root:not(.win) .hr-idle-actions,.hr-pill-root:not(.win) .hr-recording-actions,.hr-pill-root:not(.win) .hr-hover-control{transition:none}
  .hr-pill-root:not(.win) .hr-actions,.hr-pill-root:not(.win) .hr-pill.idle.hover::before{animation:none}
}
`;
