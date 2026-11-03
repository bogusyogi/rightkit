//! Collects markdown stream source at newline boundaries.
//!
//! `MarkdownStreamCollector` buffers incoming token deltas and exposes a commit boundary at each
//! newline. The stream controllers (`streaming/controller.rs`) call `commit_complete_source()`
//! after each newline-bearing delta to obtain the completed prefix for re-rendering, leaving the
//! trailing incomplete line in the buffer for the next delta.
//!
//! On finalization, `finalize_and_take_source()` transfers the complete source buffer to the
//! controller (including a final line that may lack a trailing newline).
