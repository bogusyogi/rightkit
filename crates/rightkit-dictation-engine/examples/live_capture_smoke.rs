use anyhow::{bail, Result};
use rightkit_audio::capture::{CaptureFirstBuffer, CaptureSession, OutDType, SessionConfig};

fn main() -> Result<()> {
    let mut session = CaptureSession::start(SessionConfig {
        device_id: None,
        device_platform_id: None,
        target_rate: 16_000,
        target_channels: 1,
        target_dtype: OutDType::Float32,
        block_ms: 20,
    })?;

    let samples = match session.wait_first_buffer(1_333, 1_500)? {
        CaptureFirstBuffer::Data(samples) => samples,
        CaptureFirstBuffer::NoCallbacks => bail!("default input produced no callbacks"),
        CaptureFirstBuffer::NoSamples => bail!("default input callbacks produced no samples"),
        CaptureFirstBuffer::StreamError(kind) => {
            bail!("default input reported {}", kind.as_str())
        }
    };

    if samples.is_empty() || !samples.iter().all(|sample| sample.is_finite()) {
        bail!("default input returned invalid PCM");
    }

    let metrics = session.stop();
    if metrics.callback_invocations == 0 || metrics.input_samples_received == 0 {
        bail!("capture metrics did not record live input");
    }

    println!(
        "capture_live samples={} callbacks={} input_samples={} first_callback_ms={} async_errors={} dropped_blocks={}",
        samples.len(),
        metrics.callback_invocations,
        metrics.input_samples_received,
        metrics
            .first_callback_latency_us
            .map(|us| us / 1_000)
            .unwrap_or_default(),
        metrics.async_error_count,
        metrics.dropped_blocks,
    );
    Ok(())
}
