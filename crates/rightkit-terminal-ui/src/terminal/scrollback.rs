//! Terminal-specific scrollback strategy selection (donor
//! `codex-rs/tui/src/tui/scrollback.rs` `ScrollbackStrategy`), adapted to the
//! the host app seam: the donor drives `custom_terminal::Terminal`, while
//! the host app's `Surface` owns geometry and `terminal::history` owns the wire
//! protocol. This module keeps only the *decision* logic; insertion and
//! viewport growth run through [`super::history`].
