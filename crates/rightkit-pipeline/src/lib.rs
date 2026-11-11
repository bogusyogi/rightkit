//! Deprecated: this crate is `rightkit-media-gen-client`, named for what it holds (the client of
//! the generation engine contract). Every item re-exports from there, for one release.
//!
//! `channel_recipe` (feature `channel-recipe` in 0.1.x) is `rightkit_video_project::channel_recipe`
//! (feature `channel-recipe` of `rightkit-video-project`); the process call is
//! `rightkit_engine_host::call`.

pub use rightkit_media_gen_client::*;

/// The generation engine client under its 0.1.x name.
#[deprecated(
    since = "0.2.0",
    note = "use rightkit_media_gen_client::GenerationClient"
)]
pub type GenRightClient<R> = rightkit_media_gen_client::GenerationClient<R>;
