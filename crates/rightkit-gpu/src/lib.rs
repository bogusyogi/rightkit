//! Shared wgpu device context for Right Suite apps (#1159): per-OS backend policy, a crash
//! sentinel around device creation, adapter pick and limits, an exit gate for in-flight GPU work,
//! and a device-memory budget that adapts to out-of-memory. Native only; the apps' kernels stay in
//! the apps.
//!
//! * [`backend`]: which backends wgpu may load (Windows DX12 alone, Metal on Apple, Vulkan
//!   elsewhere, overridable by the app's own environment variable) and the crash sentinel
//!   ([`backend::set_init_marker`], [`backend::take_init_marker`]) that lets the next launch know a
//!   driver killed the last one during device creation.
//! * [`GpuContext::create`]: adapter by name or default, software adapters refused, limits, error
//!   and device-loss callbacks.
//! * [`exit`]: every entry into the device goes through [`exit::enter`]; the host ends the process
//!   with [`exit::begin_shutdown`] and [`exit::wait_idle`].
//! * [`MemoryBudget`]: renders ask for room, wait fairly, halve the budget on out-of-memory and
//!   grow it back with each success.
//!
//! wgpu is pinned at 30 here (the version Ember builds on, and `rightkit-compositor`'s). CutRight
//! moves from 29.0.1 to 30 to use this crate; the crate does not carry both.
//!
//! Adapted from Ember `crates/gpu` (`backend.rs`, `exit.rs`, `budget.rs`, the generic part of
//! `ctx.rs`); see NOTICE.

#![cfg(not(target_arch = "wasm32"))]

pub mod backend;
mod budget;
mod context;
pub mod exit;

pub use budget::{MemoryBudget, Permit, footprint, initial};
pub use context::{
    ContextOptions, GpuContext, LimitsCap, adapter_names, parse_limits_cap, pick_adapter,
};
pub use wgpu;
