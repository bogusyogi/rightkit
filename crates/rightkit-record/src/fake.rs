//! A scripted backend for tests. It replays helper events from a queue, writes
//! placeholder media, and records how the core used it.

use std::collections::VecDeque;
use std::fs;

use rightkit_contracts::types::record_session::{RecorderOs, VisibilityState};
use rightkit_contracts::types::Rect;

use crate::backend::{
    track_file, Backend, BackendPlan, Finished, FrameSample, HelperEvent, HelperIdentity,
    Permission, Source, StartInfo, TrackOutput,
};
use crate::bundle::MEDIA_FILE;
use crate::error::{Code, RecordError};
use crate::hash::blake3_text;

pub struct FakeBackend {
    os: RecorderOs,
    os_build: String,
    permission: Permission,
    /// Whether an `Undetermined` permission is granted by the start-time prompt.
    pub prompt_grants: bool,
    /// The visibility the fake reports at start. `None` reports the declared one.
    pub observed_visibility: Option<VisibilityState>,
    /// The scale the fake reports as applied. `None` reports the requested one.
    pub applied_scale: Option<f64>,
    pub sources: Vec<Source>,
    /// Batches returned by successive `poll` calls, oldest first.
    script: VecDeque<Vec<HelperEvent>>,
    pub start_error: Option<RecordError>,
    pub stop_error: Option<RecordError>,
    pub clean_finish: bool,
    pub starts: u32,
    pub stops: u32,
    pub aborts: u32,
    pub last_plan: Option<BackendPlan>,
}

impl FakeBackend {
    pub fn macos() -> Self {
        Self::new(RecorderOs::Macos, "27.0.1")
    }

    pub fn windows() -> Self {
        Self::new(RecorderOs::Windows, "10.0.26300.9550")
    }

    fn new(os: RecorderOs, os_build: &str) -> Self {
        FakeBackend {
            os,
            os_build: os_build.into(),
            permission: Permission::Granted,
            prompt_grants: true,
            observed_visibility: None,
            applied_scale: None,
            sources: Vec::new(),
            script: VecDeque::new(),
            start_error: None,
            stop_error: None,
            clean_finish: true,
            starts: 0,
            stops: 0,
            aborts: 0,
            last_plan: None,
        }
    }

    pub fn set_permission(&mut self, permission: Permission) {
        self.permission = permission;
    }

    /// Queues one poll's worth of helper events.
    pub fn push(&mut self, batch: Vec<HelperEvent>) {
        self.script.push_back(batch);
    }
}

impl Backend for FakeBackend {
    fn os(&self) -> RecorderOs {
        self.os
    }

    fn os_build(&self) -> String {
        self.os_build.clone()
    }

    fn helper(&self) -> HelperIdentity {
        HelperIdentity {
            identity: "rightkit-record-fake".into(),
            signed: false,
        }
    }

    fn permission(&self) -> Permission {
        self.permission
    }

    fn sources(&self) -> Result<Vec<Source>, RecordError> {
        Ok(self.sources.clone())
    }

    fn start(&mut self, plan: &BackendPlan) -> Result<StartInfo, RecordError> {
        self.starts += 1;
        self.last_plan = Some(plan.clone());
        if let Some(error) = self.start_error.take() {
            return Err(error);
        }
        match self.permission {
            Permission::Granted => {}
            Permission::Denied => {
                return Err(RecordError::new(Code::PermissionDenied, "not granted"));
            }
            // The start-time prompt decides an undetermined permission.
            Permission::Undetermined if self.prompt_grants => {
                self.permission = Permission::Granted;
            }
            Permission::Undetermined => {
                return Err(RecordError::new(Code::PermissionDenied, "prompt declined"));
            }
        }
        // One second of structurally valid fragmented MP4, so a quarantined take
        // has media the recovery check accepts.
        fs::write(
            plan.out_dir.join(MEDIA_FILE),
            crate::media::synthetic_fragmented_mp4(30, 33, 30),
        )
        .map_err(|e| RecordError::new(Code::Io, e.to_string()))?;
        for kind in &plan.audio {
            let path = plan.out_dir.join(track_file(*kind));
            fs::create_dir_all(path.parent().expect("track paths have a parent"))
                .and_then(|_| fs::write(&path, b"fake-track-bytes"))
                .map_err(|e| RecordError::new(Code::Io, e.to_string()))?;
        }
        Ok(StartInfo {
            host_ns_at_start: 1_000_000_000,
            width: 1600,
            height: 900,
            scale: self.applied_scale.unwrap_or(plan.scale),
            bounds_pt: Rect {
                x: -10_000.0,
                y: -10_000.0,
                width: 800.0,
                height: 520.0,
            },
            dpi: 144,
            codec: plan.codec.as_str().into(),
            os_build: self.os_build.clone(),
            visibility: self.observed_visibility.unwrap_or(plan.visibility),
        })
    }

    fn poll(&mut self) -> Vec<HelperEvent> {
        self.script.pop_front().unwrap_or_default()
    }

    fn stop(&mut self) -> Result<Finished, RecordError> {
        self.stops += 1;
        if let Some(error) = self.stop_error.take() {
            return Err(error);
        }
        let tracks = self
            .last_plan
            .as_ref()
            .map(|plan| {
                plan.audio
                    .iter()
                    .map(|kind| TrackOutput {
                        kind: *kind,
                        offset_ms: 0,
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Finished {
            clean: self.clean_finish,
            tracks,
        })
    }

    fn abort(&mut self) {
        self.aborts += 1;
    }
}

/// A frame whose raw hash is derived from its index.
pub fn frame(index: u64, host_ns: u64, sample_pts_ns: u64) -> FrameSample {
    FrameSample {
        index,
        host_ns,
        sample_pts_ns,
        raw_hash: blake3_text(&index.to_le_bytes()),
    }
}

/// A visibility state that the default platform supports.
pub fn supported_state() -> VisibilityState {
    VisibilityState::OffScreenVisible
}
