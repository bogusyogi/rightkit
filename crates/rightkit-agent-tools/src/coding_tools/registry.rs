// Tool registry — owns the builtin catalog, scoped registrations, lazy
// promotion state, catalog generations, and toolset recommendations.
//
// Ordinary submodules (no shared include! scope): `native_types` defines
// `ToolRegistry` and the value types, `registry_impl` the methods and the
// builtin factory table, `lazy_loading` the deferred-promotion machinery,
// `catalog_generation` the snapshot/generation tracker, and
// `recommendation` the toolset-size advisory.

mod catalog_generation;
mod lazy_loading;
mod native_types;
mod recommendation;
mod registry_impl;

pub use catalog_generation::*;
pub use lazy_loading::*;
pub use native_types::*;
pub use recommendation::*;
pub use registry_impl::*;
