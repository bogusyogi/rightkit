//! The notch drawing core: geometry ([`layout`]), motion ([`motion`]), the hover card model
//! (`card`), the software renderer (`render`), the card swell (`swell`), the card glide (`glide`),
//! the card sequencing (`card_flow`) and the bound on how much of a string is ever shaped
//! ([`text`]). The renderer and the card model are Windows-only; the application supplies switches, thumbnails and
//! translation through [`host`].

#[cfg(windows)]
pub mod blur;
#[cfg(windows)]
pub mod card;
#[cfg(windows)]
pub mod card_flow;
#[cfg(windows)]
pub mod glide;
pub mod host;
pub mod layout;
pub mod motion;
#[cfg(windows)]
pub mod render;
#[cfg(windows)]
pub mod swell;
pub mod text;
