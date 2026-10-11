# Send receive collision skeptic review

Existing `reasoning.data-safety` finding `send-f1-partial-path-overwrite` is technically reproducible as a conditional data-loss hazard, with narrower reachability than finding text suggests.

- `crates/rightkit-send/src/receive.rs:468-486` sanitizes sender-controlled `file_name`, atomically reserves only `final_path`, then derives deterministic hidden `part_path`.
- `crates/rightkit-send/src/receive.rs:485-493` opens hidden path with `create(true).truncate(true)`, so pre-existing `.name<temp_suffix>` is truncated before bytes, checksum, or rename.
- `crates/rightkit-send/src/proto.rs:179-211` proves final-name collision handling is atomic, but it does not reserve/check derived partial path.
- `crates/rightkit-send/src/receive.rs:544-549,552-569` removes both paths on stream, checksum, or rename failure, so cleanup can also remove a pre-existing partial file.

Skeptic verdict: retain as `HARDENING_GAP`, medium severity, confidence likely. Sender cannot directly write receiver save storage; exploitation needs a stale/manual partial file whose predictable name matches accepted sender filename, or a local concurrent actor racing before open. Fix by reserving partial path with `create_new` plus retry, or using unpredictable per-transfer temp name, and track ownership before cleanup. Add regression test precreating expected partial path and proving it survives.
