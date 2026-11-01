//! Automatic editing for Right Suite video apps: transcript-driven cuts (retakes, fillers,
//! silences), the editorial engine, named cut variants of one source, and scoped editing rules
//! with their resolution matrix. Moved from CutRight (`cutright-cuts`, `-editorial`,
//! `-variants`, `-rules`).

#[cfg(feature = "engine-commands")]
pub mod cmd;
pub mod cuts;
pub mod editorial;
pub mod rules;
pub mod variants;
