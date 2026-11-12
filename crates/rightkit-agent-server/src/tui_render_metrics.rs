//! REN-006: bounded, content-free diagnostic projection of renderer counters.

use rightkit_terminal_ui::scheduler::RenderMetrics;
use std::sync::Arc;
use std::time::Duration;

pub(super) struct MetricsExport(Option<tokio::task::JoinHandle<()>>);

impl Drop for MetricsExport {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

pub(super) fn start(metrics: Arc<RenderMetrics>) -> MetricsExport {
    let Some(path) = crate::tui_projects::user_config_path()
        .map(|path| path.with_file_name("tui-render-metrics.json"))
    else {
        return MetricsExport(None);
    };
    MetricsExport(Some(tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last = None;
        loop {
            interval.tick().await;
            let snapshot = metrics.snapshot();
            if last == Some(snapshot) {
                continue;
            }
            let path = path.clone();
            let result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
                let payload = serde_json::json!({
                    "schemaVersion": crate::host_identity::schema("tui-render-metrics.v1"),
                    "redraws": snapshot.redraws,
                    "reflows": snapshot.reflows,
                    "coalesced": snapshot.coalesced_requests,
                    "immediateRequests": snapshot.immediate_requests,
                    "slowFrames": snapshot.slow_frames,
                    "inputLatencySamples": snapshot.input_latency_samples,
                    "inputLatencyMs": if snapshot.input_latency_samples == 0 { 0.0 } else {
                        snapshot.input_latency_micros as f64 / snapshot.input_latency_samples as f64 / 1000.0
                    },
                });
                let bytes = serde_json::to_vec(&payload)?;
                // Rebuildable diagnostics: replace atomically without fsync.
                rightkit_fs::atomic_write_with(
                    &path,
                    rightkit_fs::AtomicOptions {
                        create_parents: true,
                        durable: false,
                        ..rightkit_fs::AtomicOptions::default()
                    },
                    |file| std::io::Write::write_all(file, &bytes),
                )?;
                Ok(())
            }).await;
            match result {
                Ok(Ok(())) => last = Some(snapshot),
                other => tracing::warn!(error = ?other, "renderer metrics export failed"),
            }
        }
    })))
}
