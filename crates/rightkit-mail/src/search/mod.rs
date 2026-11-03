//! Gmail-style mail search: the operator vocabulary and query compiler over `rightkit-search`
//! ([`query`]), the flat [`SearchTerms`] accumulator, per-message SQL predicates for a mail
//! schema ([`predicates`]), keyset paging ([`paging`]) and the candidate-driven planner
//! ([`planner`]) that runs it all against a host database through [`SearchStore`].

pub mod attachment_path;
pub mod candidates;
pub mod dense;
pub mod enrich;
pub mod operator_first;
pub mod operator_first_probe;
pub mod paging;
pub mod planner;
pub mod predicates;
pub mod query;
pub mod scope;
pub mod search_terms;
pub mod snippet;
pub mod store;
pub mod thread_sql;

pub use paging::{SearchCursor, SEARCH_PAGE_SIZE};
pub use planner::{search_threads_page, search_threads_scoped};
pub use search_terms::SearchTerms;
pub use store::{SearchRow, SearchStore};
