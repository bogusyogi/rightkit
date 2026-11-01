# rightkit-text-polish

Transcript polish: deterministic L0 cleanup (numbers, dates, inline edits, formatting, casing),
spoken control and AI-transform command parsing, vocabulary replacements and learning.

Moved into RightKit from HeardRight (`heardright_core`: `text_pipeline`, `vocabulary`,
`vocab_learn`) on 2026-11-01.

- `text_pipeline::deterministic_polish` / `deterministic_polish_tail`: pure local cleanup of a
  finished transcript. `deterministic_polish_with_brand` adds a host-supplied
  `BrandHomophones` table that repairs multi-word ASR corruptions of a product name, each phrase
  guarded against its literal reading.
- `text_pipeline::parse_control_command`, `parse_ai_transform_command` and friends: spoken
  control tails ("... send it") and AI transform commands.
- `vocabulary`: user terms and replacement rules; `vocab_learn`: suggestions learned from the
  user's own edits.

No models, no network, no disk. LLM cleanup (L1-L3), provider keys and screen-context vocabulary
are not in this release; they stay in the app until their platform dependencies are published.
