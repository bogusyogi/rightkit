# Council Room watch surface — ship brief

## Task truth

Council Room helps Adrian supervise a multi-model review by making who is participating, what each
seat said, what remains unresolved, and whether human intervention is needed obvious within seconds.

## Existing system

The standalone page follows CodeRight's established neutral dark shell: `#0b0b0a` background,
`#181817` panels, `#242423` raised surfaces, `#2f2f2d` lines, `#f2f1ed` primary text,
`#9a978f` muted text, monochrome primary actions, green success, red danger, and amber warning.

## Registers considered

1. Operational/dense — black/red, table-first, compressed sans, no motion. Fast but reads like
   infrastructure monitoring and makes the conversation secondary.
2. Calm/editorial — warm paper, serif/sans, wide reading column, crossfades. Readable but visually
   disconnected from CodeRight and poor for live state scanning.
3. Technical chamber — CodeRight charcoal, monochrome command controls, mono metadata, debate
   timeline, semantic state color, one incoming-message transition. Selected because it keeps the
   conversation primary while preserving operational clarity and sibling identity.

## Workspace signature

The **debate spine** groups every room event into a human-readable timeline with seat identity,
message kind, phase, reply relationship, and readable prose. Participant activity stays visible on
the left; decision/finding state stays visible on the right; intervention remains anchored below the
discussion.

## Palette logic

The near-black environment fits a long-running desktop review beside CodeRight. Near-white is the
command accent; green means active/complete, amber means awaiting attention, and red means failed or
disconnected. Color never carries state alone.

