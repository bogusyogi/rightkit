//! The host seam for running search SQL.
//!
//! The planner in this crate builds SQL text and bound parameters; a host (the mail app) owns
//! the database, so it implements [`SearchStore`] over its connection. Parameters are always
//! text: SQLite applies column affinity, so numeric bounds compare numerically.

/// A result row the planner decorates after the main query: it needs the thread key to look up
/// match details and a few setters for the highlight fields.
pub trait SearchRow {
    /// Account the thread belongs to.
    fn account_id(&self) -> &str;
    /// Thread id (unique per account).
    fn thread_id(&self) -> &str;
    /// Id of the message that matched the full-text query, once known.
    fn matching_message_id(&self) -> Option<&str>;
    /// Record how many messages of the thread match.
    fn set_match_count(&mut self, count: i64);
    /// Record the best matching message and a plain-text excerpt around the match.
    fn set_matching_message(&mut self, message_id: String, excerpt: Option<String>);
    /// Record the full-text snippet (match spans wrapped in `\u{1}` and `\u{2}`).
    fn set_matching_snippet(&mut self, snippet: String);
}

/// What the search planner needs from the database that holds the mail schema.
pub trait SearchStore {
    /// The thread row type the host returns.
    type Row: SearchRow;

    /// Current time in milliseconds since the Unix epoch (clock for the `SNOOZED` scope).
    fn now_ms(&self) -> i64;

    /// Run a thread-list query built from [`super::thread_sql`] and map each row into the host's
    /// thread type, in result order.
    fn query_threads(&self, sql: &str, params: &[String]) -> Result<Vec<Self::Row>, String>;

    /// Run a query that returns one integer in its first column of its first row.
    fn query_count(&self, sql: &str, params: &[String]) -> Result<i64, String>;

    /// Run a query and return every row with every column rendered as text (integers in
    /// decimal, NULL as an empty string).
    fn query_text_rows(&self, sql: &str, params: &[String]) -> Result<Vec<Vec<String>>, String>;
}
