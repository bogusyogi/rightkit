// Donor-internal helpers take explicit per-parameter contexts; keep the
// original signatures rather than reshaping the donated code.
#![allow(clippy::too_many_arguments)]

mod blend_mode;
mod compositor;
pub mod effects;
mod frame;
pub mod gpu;
pub mod masks;
mod texture_pool;
mod texture_store;

pub use blend_mode::BlendMode;
pub use compositor::{Compositor, CompositorError, RenderFrameOptions};
pub use frame::{
    CanvasClearDescriptor, CanvasTextureDescriptor, EffectPassDescriptor,
    EffectUniformValueDescriptor, FrameDescriptor, FrameItemDescriptor, LayerDescriptor,
    LayerMaskDescriptor, QuadTransformDescriptor,
};
