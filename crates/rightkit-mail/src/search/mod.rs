//! Gmail-style mail search: the operator vocabulary and query compiler over `rightkit-search`
//! ([`query`]), the flat [`SearchTerms`] accumulator, per-message SQL predicates for a mail
//! schema ([`predicates`]) and keyset paging ([`paging`]).

pub mod paging;
pub mod predicates;
pub mod query;
pub mod search_terms;

pub use paging::{SearchCursor, SEARCH_PAGE_SIZE};
pub use search_terms::SearchTerms;

#[cfg(test)]
mod ast_tests;
