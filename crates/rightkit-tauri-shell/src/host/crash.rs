//! Native renderer-error sink. Caller policy runs before the owner's redaction/rate gate.
//!
//! The command itself is [`crate::crash_commands::rightkit_crash_window_error`], which takes this
//! service's path while one is registered ([`crate::crash_commands::crash_route`]).
use std::sync::Arc;

use rightkit_crash::{CrashConfig, WindowErrorGate, WindowErrorReport};
use tauri::{Manager, Webview, Wry};

use super::{managed, HostError, ServiceId};
use crate::caller_gate;
use crate::crash_commands::CrashSink;

pub struct CrashHostService {
    sink: CrashSink,
}

impl CrashHostService {
    pub fn new(config: CrashConfig) -> Self {
        Self {
            sink: CrashSink::new(config, WindowErrorGate::default()),
        }
    }

    pub fn with_gate(self, gate: WindowErrorGate) -> Self {
        Self {
            sink: self.sink.with_gate(gate),
        }
    }

    /// No path or recording-consent state is returned to the renderer.
    pub fn record(&self, report: WindowErrorReport) {
        self.sink.record(report);
    }
}

/// Resolve native caller policy before returning the recording service.
pub(crate) fn crash_service(
    webview: &Webview<Wry>,
    action: &str,
) -> Result<Arc<CrashHostService>, HostError> {
    let host = managed(webview.app_handle())?;
    let caller = caller_gate::gather(webview)?;
    host.service_for(ServiceId::CRASH, action, &caller.facts())
}
