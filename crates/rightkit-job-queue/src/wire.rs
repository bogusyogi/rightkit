//! Framed wire constants shared by the dispatcher (`contract`) and the supervisor client.
//! Store-free: compiled with the `protocol` feature.

/// Bumped on breaking changes to frames, built-ins, or `job.*` commands.
pub const CONTRACT_VERSION: u32 = 1;
/// Largest frame the contract reads or writes unless a caller overrides it.
pub const DEFAULT_FRAME_LIMIT: usize = 16 * 1024 * 1024;
/// Ceiling on a configured frame limit (256 MiB). A limit above it (`usize::MAX` is the
/// "no limit" some apps passed) is treated as this: every reader allocates up to the limit for one
/// line, so an unbounded limit is an unbounded allocation driven by the peer.
pub const MAX_FRAME_LIMIT: usize = 256 * 1024 * 1024;

/// `limit` bounded to [`MAX_FRAME_LIMIT`].
pub fn finite_frame_limit(limit: usize) -> usize {
    limit.min(MAX_FRAME_LIMIT)
}
