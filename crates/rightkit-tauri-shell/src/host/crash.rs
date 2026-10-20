//! Native renderer-error sink. Caller policy runs before the owner's redaction/rate gate.
use std::sync::Arc;

use rightkit_crash::{record_window_error, CrashConfig, WindowErrorGate, WindowErrorReport};
use tauri::{Manager, Webview, Wry};

use super::{blocking, managed, HostError, ServiceId};
use crate::caller_gate;

pub struct CrashHostService {
    config: CrashConfig,
    gate: WindowErrorGate,
}

impl CrashHostService {
    pub fn new(config: CrashConfig) -> Self {
        Self {
            config,
            gate: WindowErrorGate::default(),
        }
    }

    pub fn with_gate(mut self, gate: WindowErrorGate) -> Self {
        self.gate = gate;
        self
    }

    /// No path or recording-consent state is returned to the renderer.
    pub fn record(&self, report: WindowErrorReport) {
        let _ = record_window_error(&self.config, &self.gate, &report);
    }
}

/// Resolve native caller policy before returning the recording service.
fn crash_service(webview: &Webview<Wry>, action: &str) -> Result<Arc<CrashHostService>, HostError> {
    let host = managed(webview.app_handle())?;
    let caller = caller_gate::gather(webview)?;
    host.service_for(ServiceId::CRASH, action, &caller.facts())
}

#[tauri::command]
pub async fn rightkit_crash_window_error(
    webview: Webview<Wry>,
    report: WindowErrorReport,
) -> Result<(), HostError> {
    let service = crash_service(&webview, "window_error")?;
    blocking(move || {
        service.record(report);
        Ok(())
    })
    .await
}
