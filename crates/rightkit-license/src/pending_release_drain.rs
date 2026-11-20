//! Drain the durable pending-release queue through the licence client and log the outcome
//! (feature `client`; declare with `#[cfg(feature = "client")] mod pending_release_drain;`).
//!
//! The client re-sends every unacknowledged entry; settled entries (2xx, 404, 410) are removed one
//! by one and network or server failures stay durable for the next launch, focus or foreground
//! retry. The queue contents (activation tokens in legacy rows) are never logged: only counts.

use crate::{LicenseClient, PendingReleaseQueue};

/// Retry every unacknowledged removal. `log` receives `(event, "delivered_count=<n>
/// pending_count=<n>")` only when at least one entry was attempted.
pub fn drain_pending_releases_logged(
    client: &LicenseClient,
    queue: &dyn PendingReleaseQueue,
    event: &str,
    log: impl Fn(&str, &str),
) -> Result<(), String> {
    let report = client
        .drain_pending_releases(queue)
        .map_err(|error| error.to_string())?;
    if report.attempted > 0 {
        log(
            event,
            &format!(
                "delivered_count={} pending_count={}",
                report.settled,
                report.kept + report.acknowledge_failed
            ),
        );
    }
    Ok(())
}
