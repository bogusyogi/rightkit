#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::{engine::*, engine_impl::*, engine_support::*, finalize_transcript::*};

impl EngineRuntime {
    /// Compatibility response for callers that still address the legacy engine route.
    /// The shell owns destination proof and recovery for a repaste.
    pub fn repaste_last(&mut self) -> Result<DeliveryRecord, String> {
        Err("repaste requires the shell delivery host".to_string())
    }

    pub fn copy_last(&mut self) -> Result<String, String> {
        let last = self
            .last_delivery
            .as_ref()
            .ok_or_else(|| "no last delivery to copy".to_string())?;
        copy_text(&last.transcript).map_err(|e| e.to_string())?;
        Ok(last.transcript.clone())
    }

    pub fn last_delivery(&self) -> Option<&DeliveryRecord> {
        self.last_delivery.as_ref()
    }

    pub(crate) fn push_recent(&mut self, record: DeliveryRecord) {
        if self.recent.len() == RECENT_HISTORY_CAP {
            self.recent.pop_back();
        }
        self.recent.push_front(record);
    }
}
