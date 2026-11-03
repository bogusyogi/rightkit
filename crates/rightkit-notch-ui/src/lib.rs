//! The notch drawing core: geometry ([`layout`]), motion ([`motion`]), the hover card model
//! (`card`), the software renderer (`render`) and the card swell (`swell`). The renderer and
//! the card model are Windows-only; the application supplies switches, thumbnails and
//! translation through [`host`].

#[cfg(windows)]
pub mod blur;
#[cfg(windows)]
pub mod card;
pub mod host;
pub mod layout;
pub mod motion;
#[cfg(windows)]
pub mod render;
#[cfg(windows)]
pub mod swell;
