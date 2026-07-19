# Council Room build manifest

## Anchor

- Surface: local operational review console, not a landing page.
- Task: understand who is participating, follow the review, see unresolved state, and optionally interject.
- Visual source: CodeRight's near-black workspace, restrained neutral borders, compact technical typography, and state-led accents.
- Signature: a phase rail over a three-lane chamber — participants, discussion, room state.

## Regions

1. Local room header and connection state.
2. Review phase rail.
3. Participant and budget rail.
4. Discussion timeline and empty/live/error/sealed states.
5. Human interjection composer.
6. Room-state and findings rail.

## Implementation

- Replaced the unstyled debug HTML with a responsive, dependency-free interface in `src/server.rs`.
- Preserved the existing authenticated loopback endpoints and one-click capability link.
- Replaced untrusted `innerHTML` rendering with DOM construction and `textContent`.
- Added visible empty, live, disconnected/retry, and sealed states.
- Added reduced-motion handling and a 180 ms transform/opacity-only event entrance.
- Bundle delta: 0 external dependencies; inline CSS/JS only.

## Verification artifacts

- `qa/handoff-desktop.png`
- `qa/handoff-tablet.png`
- `qa/handoff-mobile.png` (headless capture crop limitation recorded in audit)
- `qa/live-interjection.png`
- `qa/disconnected.png`
- `qa/sealed.png`
- `qa/throttled-desktop.png`

