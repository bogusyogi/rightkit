//! Audio output (feature `playback`): push interleaved `f32` samples into a bounded lock-free
//! ring that a cpal output callback drains, with a latency-adjusted playback clock for A/V
//! sync.
//!
//! Real-time rule: the output callback only reads and writes atomics and the lock-free ring,
//! into buffers allocated before the stream started. It never allocates, locks, blocks, logs
//! or panics. Everything that can fail or wait (device selection, stream build, device-loss
//! recovery, validation) runs on the caller's thread.
//!
//! ```no_run
//! use rightkit_audio::playback::OutputSession;
//!
//! let mut out = OutputSession::open(None)?;
//! let format = out.format(); // device rate and channels
//! let decoded: Vec<f32> = vec![0.0; format.sample_rate as usize * format.channels as usize / 10];
//! let accepted = out.push_samples(&decoded); // never blocks; may accept fewer
//! let clock = out.position(); // A/V master clock
//! # let _ = (accepted, clock);
//! # Ok::<(), rightkit_audio::AudioError>(())
//! ```

mod render;
mod ring;
mod session;
mod state;

pub use session::{
    list_output_devices, OutputControl, OutputDeviceInfo, OutputFormat, OutputSession,
    DEFAULT_RING_DURATION,
};
pub use state::OutputStats;
