//! Offscreen execution: `GpuContext` + `Compositor` + transport + cache.
//!
//! `NativeCompositor` composes one frame end to end offscreen and can read it
//! back to RGBA. This is the proof path — it is *not* claimed to be efficient
//! embedded Tauri playback (no surface presentation, no decode pipeline, no
//! playback clock); it establishes descriptor correctness and pixel semantics.

use rightkit_compositor::gpu::{wgpu, GpuContext, GpuError};
use rightkit_compositor::{Compositor, CompositorError};
use thiserror::Error;

use crate::native::cache::FrameResource;
use crate::native::identity::{FrameKey, PreviewQuality};
use crate::native::mapper::{compose_frame, ComposeError, ComposedFrame};
use crate::native::snapshot::ComposeSnapshot;
use crate::native::transport::TextureProvider;

#[derive(Debug, Error)]
pub enum ExecuteError {
    #[error(transparent)]
    Compose(#[from] ComposeError),
    #[error(transparent)]
    Compositor(#[from] CompositorError),
    #[error(transparent)]
    Gpu(#[from] GpuError),
}

/// GPU texture-backed frame payload for [`crate::native::cache::FrameCache`].
pub type GpuFrame = FrameResource<wgpu::Texture>;

/// Wraps the donor compositor behind the snapshot contract.
pub struct NativeCompositor {
    context: GpuContext,
    compositor: Compositor,
}

impl NativeCompositor {
    /// Acquires a native WebGPU device (high-performance preference).
    /// Returns `Err` when no usable adapter exists — callers must treat that
    /// as "native compositing unavailable on this host", not as a crash.
    pub async fn new() -> Result<Self, GpuError> {
        let context = GpuContext::new().await?;
        let compositor = Compositor::new(&context);
        Ok(Self {
            context,
            compositor,
        })
    }

    pub fn context(&self) -> &GpuContext {
        &self.context
    }

    /// Builds the frame key for `frame_index` under `quality`.
    /// `quality` is applied to the snapshot's output spec via
    /// [`ComposeSnapshot::at_quality`]-style rebasing done by the caller; the
    /// key records the effective dimensions.
    pub fn frame_key(
        snapshot: &ComposeSnapshot,
        frame_index: i64,
        quality: PreviewQuality,
    ) -> FrameKey {
        let spec = crate::native::snapshot::OutputSpec {
            scale: snapshot.output.scale * quality.scale_factor(),
            ..snapshot.output
        };
        let (width, height) = spec.effective_size();
        FrameKey {
            identity: snapshot.identity.clone(),
            frame: frame_index,
            width,
            height,
            quality_scale: spec.scale,
        }
    }

    /// Maps, resolves textures and renders one frame offscreen.
    /// Returns the composited texture plus the composed frame description.
    pub fn render_frame(
        &mut self,
        snapshot: &ComposeSnapshot,
        frame_index: i64,
        provider: &mut dyn TextureProvider,
    ) -> Result<(wgpu::Texture, ComposedFrame), ExecuteError> {
        let t0 = std::time::Instant::now();
        let composed = compose_frame(snapshot, frame_index)?;
        let size = (composed.descriptor.width, composed.descriptor.height);
        let mut uploaded = Vec::new();
        let mut provide_ms = 0u128;
        for req in &composed.requirements {
            let tp = std::time::Instant::now();
            let texture = provider.provide(&self.context, req, size)?;
            provide_ms += tp.elapsed().as_millis();
            let id = req.texture_id().to_string();
            self.compositor.upsert_texture(id.clone(), texture);
            uploaded.push(id);
        }
        let t1 = std::time::Instant::now();
        let texture = self
            .compositor
            .render_frame_to_texture(&self.context, &composed.descriptor)?;
        eprintln!(
            "render_frame {}: provide={provide_ms}ms compose+upload={:?} submit={:?}",
            frame_index,
            t1 - t0,
            t1.elapsed()
        );
        // Requirement textures are per-frame inputs; release them so ids stay
        // scoped to this snapshot/frame rather than leaking across revisions.
        for id in uploaded {
            self.compositor.release_texture(&id);
        }
        Ok((texture, composed))
    }

    /// Renders one frame and reads it back as RGBA8 bytes (row-major,
    /// tightly packed, BGRA converted to RGBA when the backend requires).
    pub async fn render_frame_rgba(
        &mut self,
        snapshot: &ComposeSnapshot,
        frame_index: i64,
        provider: &mut dyn TextureProvider,
    ) -> Result<(Vec<u8>, u32, u32, ComposedFrame), ExecuteError> {
        let (texture, composed) = self.render_frame(snapshot, frame_index, provider)?;
        let (w, h) = (composed.descriptor.width, composed.descriptor.height);
        let raw = self.context.read_texture_pixels(&texture, w, h).await?;
        Ok((raw_to_rgba(&raw, self.context.is_bgra()), w, h, composed))
    }
}

/// Swizzles BGRA readback to RGBA; pass-through for RGBA-format backends.
pub fn raw_to_rgba(raw: &[u8], bgra: bool) -> Vec<u8> {
    if !bgra {
        return raw.to_vec();
    }
    let mut out = raw.to_vec();
    for px in out.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    out
}

/// Helper for providers/tests: build a texture from RGBA pixels regardless
/// of the backend's native channel order.
pub fn texture_from_rgba(
    context: &GpuContext,
    rgba: &[u8],
    width: u32,
    height: u32,
    label: &'static str,
) -> wgpu::Texture {
    let pixels = if context.is_bgra() {
        let mut b = rgba.to_vec();
        for px in b.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        b
    } else {
        rgba.to_vec()
    };
    context.texture_from_pixels(&pixels, width, height, label)
}
