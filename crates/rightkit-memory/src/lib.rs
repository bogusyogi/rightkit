//! `rightkit-memory` — the RightKit memory engine, Pulse's Cortex. One dependency for an app that
//! embeds the engine:
//!
//! - the crate root is `rightkit-memory-core`: memory tiers ([`MemoryTier`], [`MemoryEntry`]),
//!   retrieval ranking and routing, the context planner, review contracts, transcript chunks and
//!   token accounting, with no I/O;
//! - [`store`] is `rightkit-memory-store`: the SQLite memory store ([`store::MemDb`]), scopes,
//!   installation identity, absorbed records, temporal facts and context telemetry.
//!
//! An app that reaches an engine running elsewhere (through the membrane CLI) uses
//! `rightkit-memory-client` instead; its tiers and rows are these types.
//!
//! ```
//! use rightkit_memory::MemoryTier;
//!
//! assert_eq!(MemoryTier::parse("semantic"), Some(MemoryTier::Semantic));
//! assert!(MemoryTier::Semantic.rank() > MemoryTier::Working.rank());
//! ```

pub use rightkit_memory_core::*;

/// The durable store: `rightkit-memory-store`.
pub mod store {
    pub use rightkit_memory_store::*;
}
