//! Remote-image safety checks for a mail reader: header-only dimension and MIME validation
//! ([`dimensions`]) and the tracking-pixel host blocklist ([`trackers`]).

pub mod dimensions;
pub mod trackers;
