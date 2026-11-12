/// Parse an ISO 8601 datetime string to a Unix timestamp.
/// Handles: `2026-06-02T14:30:00Z`, `2026-06-02T14:30:00+05:30`, `2026-06-02T14:30:00` (UTC).
fn parse_iso8601(s: &str) -> Option<i64> {
    // Tool rule: no zone means UTC. The strict RFC 3339 parse (ranges, fraction, offset) is the owner's.
    let tail = s.get(19..)?;
    let zoned = tail.contains(['Z', 'z', '+', '-']);
    let input = if zoned {
        s.to_string()
    } else {
        format!("{s}Z")
    };
    rightkit_time::DateTime::parse_rfc3339(&input).map(|dt| dt.unix_seconds())
}
