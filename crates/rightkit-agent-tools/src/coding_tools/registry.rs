// Tool registry — owns the builtin catalog, scoped registrations, lazy
// promotion state, catalog generations, and toolset recommendations.
//
// Ordinary submodules (no shared include! scope): `native_types` defines
// `ToolRegistry` and the value types, `registry_impl` the methods and the
// builtin factory table, `lazy_loading` the deferred-promotion machinery,
// `catalog_generation` the snapshot/generation tracker, and
// `recommendation` the toolset-size advisory.

mod catalog_generation;
#[cfg(test)]
mod fixed_interface_tests;
mod lazy_loading;
mod native_types;
mod recommendation;
#[cfg(test)]
mod recommendation_tests;
mod registry_impl;
#[cfg(test)]
mod resource_selector_tests;
#[cfg(test)]
mod scoped_registration_tests;
#[cfg(test)]
mod tests;

pub use catalog_generation::*;
pub use lazy_loading::*;
pub use native_types::*;
pub use recommendation::*;
pub use registry_impl::*;
