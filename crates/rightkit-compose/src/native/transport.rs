//! Frame transport boundary: who turns a [`TextureRequirement`] into a
//! `wgpu::Texture`.
//!
//! This trait is the seam between decoder/media and the compositor. The
//! adapter never decodes: integrators plug in the qualified FFmpeg-side
//! decode path (or a hardware decoder later), tests plug in synthetic frames.
//! Keeping this a trait is what separates decoder/frame transport from
//! control, and prevents this package from growing a second media runtime.

use rightkit_compositor::gpu::{wgpu, GpuContext};

use crate::native::mapper::{ComposeError, TextureRequirement};

/// Supplies a texture for one [`TextureRequirement`].
///
/// `output_size` is the effective render size; a `SourceFrame` provider may
/// decode at `source size * quality_scale`, a `RasterizedTitle` provider must
/// return exactly `output_size`.
pub trait TextureProvider {
    fn provide(
        &mut self,
        context: &GpuContext,
        requirement: &TextureRequirement,
        output_size: (u32, u32),
    ) -> Result<wgpu::Texture, ComposeError>;
}

/// Determines the texture dimensions a `SourceFrame` should decode to.
pub fn decode_size(source_width: u32, source_height: u32, quality_scale: f32) -> (u32, u32) {
    (
        ((source_width as f32 * quality_scale).ceil() as u32).max(1),
        ((source_height as f32 * quality_scale).ceil() as u32).max(1),
    )
}
