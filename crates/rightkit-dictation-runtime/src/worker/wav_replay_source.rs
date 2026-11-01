//! Deterministic capture input for event-driven worker tests.
//!
//! This source models capture arrival order only. It never sleeps, reads a
//! wall clock, or decides when downstream work should run.

pub const REPLAY_BLOCK_SAMPLES: usize = 320;

#[derive(Clone, Debug, PartialEq)]
pub struct CaptureBlock {
    /// Position of the first sample in this block.
    pub at_sample: u64,
    pub samples: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScriptedCaptureEventKind {
    Control(String),
    CaptureError(String),
    Disconnect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptedCaptureEvent {
    pub at_sample: u64,
    pub kind: ScriptedCaptureEventKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CaptureEvent {
    Audio(CaptureBlock),
    Control { at_sample: u64, name: String },
    CaptureError { at_sample: u64, message: String },
    Disconnect { at_sample: u64 },
    Eof { at_sample: u64 },
}

impl CaptureEvent {
    pub fn at_sample(&self) -> u64 {
        match self {
            Self::Audio(block) => block.at_sample,
            Self::Control { at_sample, .. }
            | Self::CaptureError { at_sample, .. }
            | Self::Disconnect { at_sample }
            | Self::Eof { at_sample } => *at_sample,
        }
    }
}

/// Minimal seam consumed by a deterministic scheduler or a live adapter.
pub trait CaptureSource {
    fn sample_rate(&self) -> u32;
    fn next_capture_event(&mut self) -> Option<CaptureEvent>;
}

pub struct WavReplaySource {
    pub(crate) sample_rate: u32,
    pub(crate) samples: Vec<f32>,
    pub(crate) cursor: usize,
    pub(crate) scripted: Vec<ScriptedCaptureEvent>,
    pub(crate) scripted_cursor: usize,
    pub(crate) eof_emitted: bool,
}

impl WavReplaySource {
    pub fn from_samples(
        sample_rate: u32,
        samples: Vec<f32>,
        scripted: Vec<ScriptedCaptureEvent>,
    ) -> Result<Self, String> {
        if sample_rate == 0 {
            return Err("replay sample rate must be non-zero".into());
        }
        let sample_count = samples.len() as u64;
        let mut previous = 0;
        for (index, event) in scripted.iter().enumerate() {
            if event.at_sample > sample_count {
                return Err(format!(
                    "script event {index} is beyond EOF: {} > {sample_count}",
                    event.at_sample
                ));
            }
            if event.at_sample % REPLAY_BLOCK_SAMPLES as u64 != 0 && event.at_sample != sample_count
            {
                return Err(format!(
                    "script event {index} is not on a capture-block boundary: {}",
                    event.at_sample
                ));
            }
            if index > 0 && event.at_sample < previous {
                return Err(format!("script event {index} is out of order"));
            }
            previous = event.at_sample;
        }
        Ok(Self {
            sample_rate,
            samples,
            cursor: 0,
            scripted,
            scripted_cursor: 0,
            eof_emitted: false,
        })
    }

    pub(crate) fn scripted_event(&mut self) -> CaptureEvent {
        let event = self.scripted[self.scripted_cursor].clone();
        self.scripted_cursor += 1;
        match event.kind {
            ScriptedCaptureEventKind::Control(name) => CaptureEvent::Control {
                at_sample: event.at_sample,
                name,
            },
            ScriptedCaptureEventKind::CaptureError(message) => CaptureEvent::CaptureError {
                at_sample: event.at_sample,
                message,
            },
            ScriptedCaptureEventKind::Disconnect => CaptureEvent::Disconnect {
                at_sample: event.at_sample,
            },
        }
    }
}

impl CaptureSource for WavReplaySource {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn next_capture_event(&mut self) -> Option<CaptureEvent> {
        let cursor = self.cursor as u64;
        if self
            .scripted
            .get(self.scripted_cursor)
            .is_some_and(|event| event.at_sample == cursor)
        {
            return Some(self.scripted_event());
        }
        if self.cursor < self.samples.len() {
            let start = self.cursor;
            let end = (start + REPLAY_BLOCK_SAMPLES).min(self.samples.len());
            self.cursor = end;
            return Some(CaptureEvent::Audio(CaptureBlock {
                at_sample: start as u64,
                samples: self.samples[start..end].to_vec(),
            }));
        }
        if !self.eof_emitted {
            self.eof_emitted = true;
            return Some(CaptureEvent::Eof { at_sample: cursor });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_ordered_fixed_blocks_then_eof_without_wall_clock() {
        let samples = (0..650).map(|sample| sample as f32).collect();
        let mut source = WavReplaySource::from_samples(16_000, samples, Vec::new()).unwrap();

        let events: Vec<_> = std::iter::from_fn(|| source.next_capture_event()).collect();
        assert_eq!(source.sample_rate(), 16_000);
        assert_eq!(events.len(), 4);
        assert!(
            matches!(&events[0], CaptureEvent::Audio(block) if block.at_sample == 0 && block.samples.len() == 320)
        );
        assert!(
            matches!(&events[1], CaptureEvent::Audio(block) if block.at_sample == 320 && block.samples.len() == 320)
        );
        assert!(
            matches!(&events[2], CaptureEvent::Audio(block) if block.at_sample == 640 && block.samples.len() == 10)
        );
        assert_eq!(events[3], CaptureEvent::Eof { at_sample: 650 });
    }

    #[test]
    fn interleaves_scripted_events_stably_at_sample_boundaries() {
        let scripted = vec![
            ScriptedCaptureEvent {
                at_sample: 320,
                kind: ScriptedCaptureEventKind::Control("pause".into()),
            },
            ScriptedCaptureEvent {
                at_sample: 320,
                kind: ScriptedCaptureEventKind::CaptureError("overrun".into()),
            },
            ScriptedCaptureEvent {
                at_sample: 640,
                kind: ScriptedCaptureEventKind::Disconnect,
            },
        ];
        let mut source = WavReplaySource::from_samples(16_000, vec![0.0; 640], scripted).unwrap();
        let events: Vec<_> = std::iter::from_fn(|| source.next_capture_event()).collect();

        assert!(matches!(&events[0], CaptureEvent::Audio(block) if block.at_sample == 0));
        assert_eq!(events[1].at_sample(), 320);
        assert!(matches!(&events[1], CaptureEvent::Control { name, .. } if name == "pause"));
        assert!(
            matches!(&events[2], CaptureEvent::CaptureError { message, .. } if message == "overrun")
        );
        assert!(matches!(&events[3], CaptureEvent::Audio(block) if block.at_sample == 320));
        assert_eq!(events[4], CaptureEvent::Disconnect { at_sample: 640 });
        assert_eq!(events[5], CaptureEvent::Eof { at_sample: 640 });
    }

    #[test]
    fn rejects_events_that_break_capture_order_or_block_geometry() {
        let mid_block = vec![ScriptedCaptureEvent {
            at_sample: 1,
            kind: ScriptedCaptureEventKind::Disconnect,
        }];
        assert!(WavReplaySource::from_samples(16_000, vec![0.0; 320], mid_block).is_err());

        let reversed = vec![
            ScriptedCaptureEvent {
                at_sample: 320,
                kind: ScriptedCaptureEventKind::Disconnect,
            },
            ScriptedCaptureEvent {
                at_sample: 0,
                kind: ScriptedCaptureEventKind::Disconnect,
            },
        ];
        assert!(WavReplaySource::from_samples(16_000, vec![0.0; 320], reversed).is_err());
    }
}
