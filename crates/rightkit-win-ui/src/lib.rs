//! Win32 UI toolkit: RAII wrappers for windows and GDI objects ([`raii`]), a software canvas
//! ([`canvas`]), a GDI surface and text painter ([`surface`], [`text_layout`]), vector marks
//! ([`glyphs`]) and shell file icons ([`fileicon`]). Everything is Windows-only; on other systems
//! the crate is empty.

#[cfg(windows)]
pub mod canvas;
#[cfg(windows)]
pub mod diag;
#[cfg(windows)]
pub mod fileicon;
#[cfg(windows)]
pub mod glyphs;
#[cfg(windows)]
pub mod process;
#[cfg(windows)]
pub mod raii;
#[cfg(windows)]
pub mod surface;
#[cfg(windows)]
pub mod text_layout;
