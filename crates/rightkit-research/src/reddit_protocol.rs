//! Payload shapes for the Reddit thread reader (channel store schema v12). The reader is OFF unless the
//! engine runs with `RIGHTKIT_REDDIT_READER=on`. It never logs in, posts, votes or sends a credential:
//! `reddit_reader.fetch` reads one public thread listing and stores the threads it sees. Reddit is
//! draft-only in the product, so nothing here carries a post, a vote, a comment body or a token.
use serde::{Deserialize, Serialize};

pub use rightkit_social_publish::protocol::Scope;

/// `reddit_reader.status` output. Read in any role; makes no network call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedditReaderStatus {
    pub enabled: bool,
    pub reason: String,
}

/// `reddit_reader.fetch` input. `subreddit` is the bare name, without the `r/` prefix. `base_url` falls
/// back to the `platform_endpoints` setting for `reddit`; a non-loopback host needs `allow_remote`.
/// `now` is the observation instant (UTC `YYYY-MM-DDTHH:MM:SSZ`) and is required: the reader never reads
/// the system clock. There is no credential field.
#[derive(Debug, Clone, Deserialize)]
pub struct RedditFetchInput {
    pub scope: Scope,
    pub subreddit: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub now: Option<String>,
}

/// One stored thread. `permalink` is always an https `reddit.com` URL, whatever base the listing came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedditThread {
    pub scope: Scope,
    pub thread_id: String,
    pub subreddit: String,
    pub title: String,
    pub author: String,
    pub permalink: String,
    /// UTC instant the thread was created, `YYYY-MM-DDTHH:MM:SSZ`.
    pub created_at: String,
    pub score: i64,
    pub comment_count: i64,
    /// The `now` of the first fetch that stored this thread. Later fetches never rewrite it.
    pub fetched_at: String,
}

/// `reddit_reader.fetch` output. `seen` is the listing length; `stored + already_stored = seen`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedditFetchOutput {
    pub scope: Scope,
    pub subreddit: String,
    pub fetched_at: String,
    pub seen: i64,
    pub stored: i64,
    pub already_stored: i64,
}

/// `reddit_reader.list` input: every stored thread in one scope.
#[derive(Debug, Clone, Deserialize)]
pub struct RedditListInput {
    pub scope: Scope,
}

/// `reddit_reader.list` output, newest thread first (created_at descending, then thread id descending).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedditListOutput {
    pub scope: Scope,
    pub threads: Vec<RedditThread>,
}
