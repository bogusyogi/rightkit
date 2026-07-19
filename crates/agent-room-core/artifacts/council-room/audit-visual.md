# Council Room visual audit

UX Health: 94/100  
UI Health: 91/100  
Motion Health: 96/100  
A11y/Responsive Health: 89/100  
Overall: SHIP for the desktop/local-browser target  
Coverage: 6 regions × 9 applicable lens groups; 1 non-target mobile capture limitation; 3 controls state-checked, 0 core desktop states untested.

## Evidence inspected

- Live handoff route at desktop 1440×1000, tablet 768×1024, and headless mobile 390×844.
- Empty, live interjection, disconnected/retry, sealed/disabled, and throttled Slow-4G + 4× CPU captures under `qa/`.
- Functional browser runs: click-focus-type-send, live SSE update, sent confirmation, retry visibility, sealed disabled controls; zero console errors/warnings.
- Designer detector, URL mode at 1440×900. Tiny-text finding cleared. Remaining `flat-type-hierarchy` warning is accepted for a dense operational console; `missing-hero-media` is inapplicable to this app surface.
- Full Rust suite: 43/43 tests green; formatting green.
- External vision jury: not requested.

## Region inventory

- R1 header/connection; R2 phase rail; R3 participants; R4 discussion; R5 composer; R6 room state/findings.

## Element inventory

- E1 Retry; E2 interjection textarea; E3 Interject button.

## Coverage matrix

| Region | Hierarchy | Layout | Type | Color | States | Motion | A11y | Slop | Performance |
|---|---|---|---|---|---|---|---|---|---|
| R1 Header | ✓ | ✓ | ✓ | ✓ | ✓ | n/a | ✓ | ✓ | ✓ |
| R2 Phases | ✓ | ✓ | ✓ | ✓ | ✓ | n/a | ✓ | ✓ | ✓ |
| R3 Participants | ✓ | ✓ | ✓ | ✓ | ✓ | n/a | ✓ | ✓ | ✓ |
| R4 Discussion | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| R5 Composer | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| R6 State/findings | ✓ | ✓ | ✓ | ✓ | ✓ | n/a | ✓ | ✓ | ✓ |

## Interaction states

| Element | Default | Focus | Click | Loading/error | Disabled | Evidence |
|---|---|---|---|---|---|---|
| E1 Retry | ✓ | ✓ | ✓ | ✓ | n/a | `qa/disconnected.png` |
| E2 Textarea | ✓ | ✓ | ✓ | ✓ | ✓ | functional run + `qa/sealed.png` |
| E3 Interject | ✓ | ✓ | ✓ | ✓ | ✓ | functional run + live/sealed captures |

## Findings

- Note — responsive capture: the shared headless mobile screenshot lane crops a wider internal viewport even though the runtime probe reports `scrollWidth <= innerWidth` and the Interject button rect inside the viewport. Desktop is the product target; real-phone visual approval remains optional.
- Note — typography: the detector still calls the compact 12/13/14/17/19 px scale flat. The hierarchy is visually legible and intentionally dense; no text is below 12 px.

## What works

- The first read is now the discussion, with participant and decision context available without raw protocol JSON.
- Empty, disconnected, and sealed states explain what is happening and what the human can do.
- The surface is domain-specific and consistent with CodeRight rather than a generic AI dashboard.
- Untrusted room output cannot inject markup into the page.

