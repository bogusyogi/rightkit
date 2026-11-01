//! The owned media library's artifact index: classify files, index gallery roots with their
//! sidecars (eyes, jury, caller, facts), filter and page the result, and infer brand codes from a
//! host-registered catalogue ([`artifacts`]); lossy path normalisation ([`path_policy`]).
//!
//! Import with provenance, asset facts, the job store and FTS search live in
//! `rightkit-media-gen` (`import`, `provenance`, `host::asset_facts`, `store`, `search`), which
//! write the sidecars this index reads.

pub mod artifacts;
pub mod path_policy;
