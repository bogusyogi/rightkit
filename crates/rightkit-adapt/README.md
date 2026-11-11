# rightkit-adapt

Governed behavioural learning from agent transcripts: **Taste** (user-backed preference learning) and **Insights** (failure and gotcha learning), deterministic and model-free, with typed admission gates (proposal eligibility, durable admission, context admission), lineage, delivery, attribution and remediation records.

Moved into RightKit from Pulse's `membrane-adapt` (engine) on 2026-11-01 as `rightkit-adapt-taste`; renamed `rightkit-adapt` in 0.2.0 (2026-11-11), because Pulse's adapt engine is the reference for the name. CodeRight's post-session learning, which held this name before (0.1.x), is `rightkit-agent-learning`; the two never shared a type. `rightkit-adapt-taste` 0.1.2 re-exports this crate for one release.
