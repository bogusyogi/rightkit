//! `rightkit-mail`: mail-domain building blocks with no database or network of their own.
//!
//! - [`search`]: Gmail-style query language, SQL predicates and keyset paging.
//! - [`fusion`]: reciprocal rank fusion of keyword and vector rankings.
//! - [`trust`]: anti-phishing heuristics over `Authentication-Results`, links and senders.
//! - [`images`]: remote-image MIME, magic-byte and dimension checks plus a tracker blocklist.

pub mod fusion;
pub mod images;
pub mod search;
pub mod trust;
