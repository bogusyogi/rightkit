//! Stateful streaming resampler wrapping rubato's fixed-input sinc resampler.
//!
//! The resampler is owned by the consumer side (the `read*` functions). The
//! audio callback never touches it — callbacks just push raw native-rate
//! samples into the ring buffer. When Python pulls audio out, we resample
//! whatever is buffered in chunks of the resampler's required input size.

use super::{anyhow, Result};
use rubato::{
    audioadapter_buffers::direct::SequentialSliceOfVecs, Async, FixedAsync, Resampler,
    SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

pub struct StreamResampler {
    inner: Option<Async<f32>>,
    input_rate: u32,
    output_rate: u32,
    /// Required input frames per resampler call.
    chunk_in: usize,
    /// Pending mono input samples that haven't filled a full chunk yet.
    leftover: Vec<f32>,
    /// Pre-allocated input scratch (1 channel × chunk_in).
    scratch_in: Vec<Vec<f32>>,
    /// Pre-allocated output scratch (1 channel × max_out).
    scratch_out: Vec<Vec<f32>>,
}

impl StreamResampler {
    pub fn new(input_rate: u32, output_rate: u32) -> Result<Self> {
        if input_rate == 0 || output_rate == 0 {
            return Err(anyhow!("resampler rates must be non-zero"));
        }

        // Bypass: same rate → just pass through, no rubato instance.
        if input_rate == output_rate {
            return Ok(Self {
                inner: None,
                input_rate,
                output_rate,
                chunk_in: 0,
                leftover: Vec::new(),
                scratch_in: Vec::new(),
                scratch_out: Vec::new(),
            });
        }

        let params = SincInterpolationParameters {
            sinc_len: 128,
            f_cutoff: Some(0.95),
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 128,
            window: WindowFunction::BlackmanHarris2,
        };

        // 20ms of input frames as the chunk granularity. Picked to balance
        // latency vs filter quality.
        let chunk_in = ((input_rate as usize) / 50).max(64);
        let resample_ratio = output_rate as f64 / input_rate as f64;
        let resampler =
            Async::<f32>::new_sinc(resample_ratio, 2.0, &params, chunk_in, 1, FixedAsync::Input)
                .map_err(|e| anyhow!("rubato init failed: {e}"))?;

        // Pre-allocate scratch buffers sized to the resampler's needs.
        let max_out = resampler.output_frames_max();
        let scratch_in = vec![vec![0.0f32; chunk_in]];
        let scratch_out = vec![vec![0.0f32; max_out]];

        Ok(Self {
            inner: Some(resampler),
            input_rate,
            output_rate,
            chunk_in,
            leftover: Vec::with_capacity(chunk_in * 2),
            scratch_in,
            scratch_out,
        })
    }

    pub fn input_rate(&self) -> u32 {
        self.input_rate
    }
    pub fn output_rate(&self) -> u32 {
        self.output_rate
    }

    /// Drop all pending state so the next `process` starts from silence: clears
    /// the partial-chunk `leftover` and resets rubato's internal filter history.
    /// Called at the start of a new recording so a prior clip's tail can't bleed
    /// into the next one. No-op in the same-rate bypass path.
    pub fn reset(&mut self) {
        self.leftover.clear();
        if let Some(r) = self.inner.as_mut() {
            r.reset();
        }
    }

    /// Drain any leftover samples by zero-padding to a full chunk and running
    /// the resampler one final time. Returns the resampled tail samples.
    /// After calling this, the resampler should not be used again.
    pub fn finish(&mut self, out: &mut Vec<f32>) -> Result<()> {
        let resampler = match self.inner.as_mut() {
            None => return Ok(()),
            Some(r) => r,
        };
        if self.leftover.is_empty() {
            return Ok(());
        }
        // Zero-pad leftover to a full chunk so we can run one last process.
        self.scratch_in[0].clear();
        self.scratch_in[0].extend_from_slice(&self.leftover);
        self.scratch_in[0].resize(self.chunk_in, 0.0);
        let input = SequentialSliceOfVecs::new(&self.scratch_in, 1, self.chunk_in)
            .map_err(|e| anyhow!("rubato input adapter failed: {e}"))?;
        let output_len = self.scratch_out[0].len();
        let mut output = SequentialSliceOfVecs::new_mut(&mut self.scratch_out, 1, output_len)
            .map_err(|e| anyhow!("rubato output adapter failed: {e}"))?;
        let (_frames_in, frames_out) = resampler
            .process_into_buffer(&input, &mut output, None)
            .map_err(|e| anyhow!("rubato finish failed: {e}"))?;
        // Trim the output by the ratio of real-vs-padded input samples.
        let real_ratio = self.leftover.len() as f64 / self.chunk_in as f64;
        let real_out = ((frames_out as f64) * real_ratio).round() as usize;
        let real_out = real_out.min(frames_out);
        out.extend_from_slice(&self.scratch_out[0][..real_out]);
        self.leftover.clear();
        Ok(())
    }

    /// Offline one-shot: feed an entire mono f32 buffer through the resampler
    /// using the same chunked code path as live capture, then flush the tail.
    /// Returns the full resampled output. Consumes the resampler state.
    pub fn process_offline(&mut self, input: &[f32]) -> Result<Vec<f32>> {
        // Pre-size: output_rate / input_rate * len + small slack for tail.
        let est = ((input.len() as u64) * (self.output_rate as u64)
            / (self.input_rate.max(1) as u64)) as usize
            + 128;
        let mut out = Vec::with_capacity(est);
        self.process(input, &mut out)?;
        self.finish(&mut out)?;
        Ok(out)
    }

    /// Feed mono `f32` samples and append resampled output to `out`.
    /// If input_rate == output_rate, this is a direct copy.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<()> {
        if self.inner.is_none() {
            out.extend_from_slice(input);
            return Ok(());
        }

        // Do not append a whole offline recording then drain its front once per
        // fixed-size rubato block: that makes a multi-minute conversion O(n²).
        // Keep only an incomplete block in `leftover`; complete input blocks are
        // consumed directly from `input`.
        let mut offset = 0;
        if !self.leftover.is_empty() {
            let needed = self.chunk_in - self.leftover.len();
            let take = needed.min(input.len());
            self.leftover.extend_from_slice(&input[..take]);
            offset = take;
            if self.leftover.len() < self.chunk_in {
                return Ok(());
            }
            // Detach this one full block so `process_full_chunk` can mutate
            // rubato scratch state without borrowing `leftover` at same time.
            let mut full_block = std::mem::take(&mut self.leftover);
            self.process_full_chunk(&full_block, out)?;
            full_block.clear();
            self.leftover = full_block;
        }

        while input.len().saturating_sub(offset) >= self.chunk_in {
            let end = offset + self.chunk_in;
            self.process_full_chunk(&input[offset..end], out)?;
            offset = end;
        }

        self.leftover.extend_from_slice(&input[offset..]);
        Ok(())
    }

    /// Run one exact rubato input block. FixedAsync::Input must consume every
    /// frame; fail closed rather than silently dropping any unusual remainder.
    fn process_full_chunk(&mut self, samples: &[f32], out: &mut Vec<f32>) -> Result<()> {
        debug_assert_eq!(samples.len(), self.chunk_in);
        let resampler = match self.inner.as_mut() {
            None => {
                out.extend_from_slice(samples);
                return Ok(());
            }
            Some(r) => r,
        };
        self.scratch_in[0].clear();
        self.scratch_in[0].extend_from_slice(samples);
        let input = SequentialSliceOfVecs::new(&self.scratch_in, 1, self.chunk_in)
            .map_err(|e| anyhow!("rubato input adapter failed: {e}"))?;
        let output_len = self.scratch_out[0].len();
        let mut output = SequentialSliceOfVecs::new_mut(&mut self.scratch_out, 1, output_len)
            .map_err(|e| anyhow!("rubato output adapter failed: {e}"))?;
        let (frames_in, frames_out) = resampler
            .process_into_buffer(&input, &mut output, None)
            .map_err(|e| anyhow!("rubato process failed: {e}"))?;
        if frames_in != self.chunk_in {
            return Err(anyhow!(
                "rubato fixed-input resampler consumed {frames_in} of {} frames",
                self.chunk_in
            ));
        }
        out.extend_from_slice(&self.scratch_out[0][..frames_out]);
        Ok(())
    }
}

