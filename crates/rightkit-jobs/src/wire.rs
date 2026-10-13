//! Framed wire constants shared by the dispatcher (`contract`) and the supervisor client.
//! Store-free: compiled with the `protocol` feature.

/// Bumped on breaking changes to frames, built-ins, or `job.*` commands.
pub const CONTRACT_VERSION: u32 = 1;
/// Largest frame the contract reads or writes unless a caller overrides it.
pub const DEFAULT_FRAME_LIMIT: usize = 16 * 1024 * 1024;
