//! Deterministic capture event pump used by production-worker replay tests.
//!
//! Audio & scripted controls share sample-positioned virtual time. Decoder
//! lanes remain independent events in `ReplayEventSchedule`.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use super::wav_replay_source::{
    CaptureEvent, CaptureSource, ScriptedCaptureEvent, WavReplaySource,
};
use super::worker_clock::{
    ClockError, ExecutorLane, ReplayEventKind, ReplayEventSchedule, VirtualWorkerClock,
    WorkerClock, WorkerInstant,
};

pub(crate) struct ReplayDriver {
    pub(crate) source: WavReplaySource,
    pub(crate) clock: Arc<VirtualWorkerClock>,
    pub(crate) base: WorkerInstant,
    pub(crate) schedule: ReplayEventSchedule,
    pub(crate) kws_flight: LaneFlight,
    pub(crate) main_asr_flight: LaneFlight,
    pub(crate) pending_capture: Option<CaptureEvent>,
    pub(crate) controls: VecDeque<CaptureEvent>,
    pub(crate) completed_lanes: VecDeque<(ExecutorLane, WorkerInstant)>,
    pub(crate) audio_remainder: VecDeque<f32>,
    pub(crate) eof: bool,
}

impl ReplayDriver {
    pub(crate) fn from_samples(
        sample_rate: u32,
        samples: Vec<f32>,
        scripted: Vec<ScriptedCaptureEvent>,
        clock: Arc<VirtualWorkerClock>,
    ) -> Result<Self, String> {
        let source = WavReplaySource::from_samples(sample_rate, samples, scripted)?;
        let base = clock.now();
        Ok(Self {
            source,
            clock,
            base,
            schedule: ReplayEventSchedule::default(),
            kws_flight: LaneFlight::Idle,
            main_asr_flight: LaneFlight::Idle,
            pending_capture: None,
            controls: VecDeque::new(),
            completed_lanes: VecDeque::new(),
            audio_remainder: VecDeque::new(),
            eof: false,
        })
    }

    pub(crate) fn is_eof(&self) -> bool {
        self.eof
    }

    pub(crate) fn mark_lane_submitted(
        &mut self,
        lane: ExecutorLane,
        submitted_at: WorkerInstant,
    ) -> Result<(), ReplayLaneError> {
        let started_at = self.clock.now().max(submitted_at);
        let flight = self.lane_flight_mut(lane);
        match flight {
            LaneFlight::Idle => {
                *flight = LaneFlight::AwaitingDuration {
                    request_at: submitted_at,
                    started_at,
                    pending_at: None,
                };
            }
            LaneFlight::AwaitingDuration {
                request_at,
                pending_at,
                ..
            }
            | LaneFlight::Scheduled {
                request_at,
                pending_at,
            }
            | LaneFlight::CompletionReady {
                request_at,
                pending_at,
            } => {
                if *request_at != submitted_at {
                    *pending_at = Some(submitted_at);
                }
            }
        }
        Ok(())
    }

    /// Bind the sole executor's actual dequeue before newer submissions can
    /// replace the predicted pending request. The preceding result has already
    /// consumed its completion before this executor takes another request.
    pub(crate) fn mark_lane_started(
        &mut self,
        lane: ExecutorLane,
        actual_request_at: WorkerInstant,
    ) -> Result<(), ReplayLaneError> {
        let (started_at, pending_at) = match self.lane_flight(lane) {
            LaneFlight::AwaitingDuration {
                started_at,
                pending_at,
                ..
            } => (started_at.max(actual_request_at), pending_at),
            // Distinct requests can share a virtual timestamp. Submission of
            // the second then appears identical to the first until dequeue.
            LaneFlight::Idle => (self.clock.now().max(actual_request_at), None),
            _ => return Err(ReplayLaneError::UnexpectedStart(lane, actual_request_at)),
        };
        *self.lane_flight_mut(lane) = LaneFlight::AwaitingDuration {
            request_at: actual_request_at,
            started_at,
            pending_at: pending_at.filter(|pending| *pending != actual_request_at),
        };
        Ok(())
    }

    pub(crate) fn supply_lane_duration(
        &mut self,
        lane: ExecutorLane,
        request_at: WorkerInstant,
        measured_decode_duration: Duration,
    ) -> Result<(), ReplayLaneError> {
        let LaneFlight::AwaitingDuration {
            request_at: mut active_request_at,
            started_at,
            mut pending_at,
        } = self.lane_flight(lane)
        else {
            return Err(ReplayLaneError::UnexpectedCompletion(lane, request_at));
        };
        if active_request_at != request_at {
            if pending_at == Some(request_at) {
                // Latest-only replacement can swap the predicted next request
                // after the prior completion released its lane but before the
                // executor dequeues it. The replacement starts at the same
                // virtual release instant; the superseded request never ran.
                active_request_at = request_at;
                pending_at = None;
            } else {
                return Err(ReplayLaneError::UnexpectedCompletion(lane, request_at));
            }
        }
        self.schedule
            .schedule_lane_completion(lane, started_at, measured_decode_duration);
        *self.lane_flight_mut(lane) = LaneFlight::Scheduled {
            request_at: active_request_at,
            pending_at,
        };
        Ok(())
    }

    pub(crate) fn schedule_lane_completion(
        &mut self,
        lane: ExecutorLane,
        submitted_at: WorkerInstant,
        measured_decode_duration: Duration,
    ) -> Result<(), ReplayLaneError> {
        self.mark_lane_submitted(lane, submitted_at)?;
        self.supply_lane_duration(lane, submitted_at, measured_decode_duration)
    }

    pub(crate) fn is_waiting_for_lane_duration(&self) -> bool {
        self.awaiting_duration_lane().is_some()
    }

    pub(crate) fn pop_control(&mut self) -> Option<CaptureEvent> {
        self.controls.pop_front()
    }

    pub(crate) fn lane_completion_ready(
        &self,
        lane: ExecutorLane,
        request_at: WorkerInstant,
    ) -> bool {
        self.completed_lanes.contains(&(lane, request_at))
    }

    pub(crate) fn take_lane_completion(
        &mut self,
        lane: ExecutorLane,
        request_at: WorkerInstant,
    ) -> bool {
        let Some(index) = self
            .completed_lanes
            .iter()
            .position(|&ready| ready == (lane, request_at))
        else {
            return false;
        };
        self.completed_lanes.remove(index);
        let LaneFlight::CompletionReady {
            request_at: active_request_at,
            pending_at,
        } = self.lane_flight(lane)
        else {
            return false;
        };
        if active_request_at != request_at {
            return false;
        }
        *self.lane_flight_mut(lane) = match pending_at {
            Some(next_request_at) => LaneFlight::AwaitingDuration {
                request_at: next_request_at,
                started_at: self.clock.now(),
                pending_at: None,
            },
            None => LaneFlight::Idle,
        };
        true
    }

    /// Advance virtual events until the requested decoder completion becomes
    /// visible. Audio that arrives first is queued for the capture consumer,
    /// preserving event order even when a trigger has already stopped capture.
    pub(crate) fn advance_to_lane_completion(
        &mut self,
        lane: ExecutorLane,
        request_at: WorkerInstant,
    ) -> Result<bool, ClockError> {
        if self.take_lane_completion(lane, request_at) {
            return Ok(true);
        }
        loop {
            let Some(event) = self.next_event()? else {
                return Ok(false);
            };
            match event {
                ReplayDriverEvent::AwaitingLaneDuration(_) => return Ok(false),
                ReplayDriverEvent::LaneCompletion(_, _) => {
                    if self.take_lane_completion(lane, request_at) {
                        return Ok(true);
                    }
                }
                ReplayDriverEvent::Capture(CaptureEvent::Audio(block)) => {
                    self.audio_remainder.extend(block.samples);
                }
                ReplayDriverEvent::Capture(CaptureEvent::Eof { .. }) => {
                    self.eof = true;
                }
                ReplayDriverEvent::Capture(control) => {
                    self.controls.push_back(control);
                }
            }
        }
    }

    pub(crate) fn next_event(&mut self) -> Result<Option<ReplayDriverEvent>, ClockError> {
        self.prime_capture_event();
        if let Some(lane) = self.awaiting_duration_lane() {
            return Ok(Some(ReplayDriverEvent::AwaitingLaneDuration(lane)));
        }
        let Some(event) = self.schedule.pop_next_event(&self.clock)? else {
            return Ok(None);
        };
        Ok(Some(match event.kind {
            ReplayEventKind::LaneCompletion(lane) => {
                let LaneFlight::Scheduled {
                    request_at,
                    pending_at,
                } = self.lane_flight(lane)
                else {
                    unreachable!("scheduled completion must have an active lane")
                };
                *self.lane_flight_mut(lane) = LaneFlight::CompletionReady {
                    request_at,
                    pending_at,
                };
                self.completed_lanes.push_back((lane, request_at));
                ReplayDriverEvent::LaneCompletion(lane, request_at)
            }
            ReplayEventKind::AudioBlock | ReplayEventKind::ScriptedControl => {
                ReplayDriverEvent::Capture(
                    self.pending_capture
                        .take()
                        .expect("capture schedule must hold capture event"),
                )
            }
        }))
    }

    pub(crate) fn read_audio(
        &mut self,
        max_samples: usize,
    ) -> Result<Option<Vec<f32>>, ClockError> {
        if !self.audio_remainder.is_empty() {
            return Ok(Some(self.take_remainder(max_samples)));
        }
        loop {
            let Some(event) = self.next_event()? else {
                return Ok(None);
            };
            match event {
                ReplayDriverEvent::AwaitingLaneDuration(_) => return Ok(None),
                ReplayDriverEvent::LaneCompletion(_, _) => {}
                ReplayDriverEvent::Capture(CaptureEvent::Audio(block)) => {
                    self.audio_remainder.extend(block.samples);
                    return Ok(Some(self.take_remainder(max_samples)));
                }
                ReplayDriverEvent::Capture(CaptureEvent::Eof { .. }) => {
                    self.eof = true;
                    return Ok(None);
                }
                ReplayDriverEvent::Capture(control) => {
                    self.controls.push_back(control);
                }
            }
        }
    }

    pub(crate) fn take_remainder(&mut self, max_samples: usize) -> Vec<f32> {
        let take = max_samples.min(self.audio_remainder.len());
        self.audio_remainder.drain(..take).collect()
    }

    pub(crate) fn awaiting_duration_lane(&self) -> Option<ExecutorLane> {
        [ExecutorLane::Kws, ExecutorLane::MainAsr]
            .into_iter()
            .find(|&lane| {
                matches!(
                    self.lane_flight(lane),
                    LaneFlight::AwaitingDuration { .. } | LaneFlight::CompletionReady { .. }
                )
            })
    }

    pub(crate) fn lane_flight(&self, lane: ExecutorLane) -> LaneFlight {
        match lane {
            ExecutorLane::Kws => self.kws_flight,
            ExecutorLane::MainAsr => self.main_asr_flight,
        }
    }

    pub(crate) fn lane_flight_mut(&mut self, lane: ExecutorLane) -> &mut LaneFlight {
        match lane {
            ExecutorLane::Kws => &mut self.kws_flight,
            ExecutorLane::MainAsr => &mut self.main_asr_flight,
        }
    }

    pub(crate) fn prime_capture_event(&mut self) {
        if self.pending_capture.is_some() || self.eof {
            return;
        }
        let Some(event) = self.source.next_capture_event() else {
            self.eof = true;
            return;
        };
        let at = self.base.saturating_add(sample_duration(
            event.at_sample(),
            self.source.sample_rate(),
        ));
        match &event {
            CaptureEvent::Audio(_) | CaptureEvent::Eof { .. } => {
                self.schedule.set_next_audio(Some(at));
            }
            CaptureEvent::Control { .. }
            | CaptureEvent::CaptureError { .. }
            | CaptureEvent::Disconnect { .. } => {
                self.schedule.set_next_control(Some(at));
            }
        }
        self.pending_capture = Some(event);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LaneFlight {
    Idle,
    AwaitingDuration {
        request_at: WorkerInstant,
        started_at: WorkerInstant,
        pending_at: Option<WorkerInstant>,
    },
    Scheduled {
        request_at: WorkerInstant,
        pending_at: Option<WorkerInstant>,
    },
    CompletionReady {
        request_at: WorkerInstant,
        pending_at: Option<WorkerInstant>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReplayLaneError {
    UnexpectedStart(ExecutorLane, WorkerInstant),
    UnexpectedCompletion(ExecutorLane, WorkerInstant),
}

#[derive(Debug, PartialEq)]
pub(crate) enum ReplayDriverEvent {
    Capture(CaptureEvent),
    LaneCompletion(ExecutorLane, WorkerInstant),
    AwaitingLaneDuration(ExecutorLane),
}

pub(crate) fn sample_instant(sample: u64, sample_rate: u32) -> WorkerInstant {
    WorkerInstant::from_duration_since_start(sample_duration(sample, sample_rate))
}

pub(crate) fn sample_duration(sample: u64, sample_rate: u32) -> Duration {
    Duration::from_secs_f64(sample as f64 / sample_rate as f64)
}

#[cfg(test)]
#[path = "replay_driver_tests.rs"]
mod tests;
