# Motion plan

- Register: product
- Language: Technical
- Persistent objects: none; this is a task workspace, not a narrative page.
- Pattern: a newly received timeline event enters with 180ms opacity + 6px translateY so the user
  can locate what changed. Button press uses an 80ms 1px translateY confirmation.
- Engine: CSS only; bundle delta 0 KB.
- Reduced motion: removes translation and all nonessential transitions.
- Restraint test: without the new-event entrance, locating the live update is slower; all other
  candidate animation was removed as decorative.

