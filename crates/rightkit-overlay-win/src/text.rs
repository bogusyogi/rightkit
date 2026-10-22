//! Text rasterisation behind `PaintCtx::text`: a grey coverage mask per string. On Windows the
//! mask comes from GDI (`TextOutW` into a scratch DIB, grey anti-aliasing, no ClearType, so the
//! result composites onto a per-pixel-alpha surface); other hosts have no text backend and draw
//! nothing, which keeps the crate compiling in a cross-platform workspace.

/// Grey coverage of rendered text, one byte per pixel, top row first.
pub(crate) struct Mask {
    pub width: usize,
    pub height: usize,
    pub coverage: Vec<u8>,
}

pub(crate) struct TextEngine {
    #[cfg(windows)]
    gdi: crate::win::gdi::GdiText,
}

impl TextEngine {
    pub(crate) fn new() -> Self {
        Self {
            #[cfg(windows)]
            gdi: crate::win::gdi::GdiText::new(),
        }
    }

    /// Renders `text` at `px` device pixels of font height. `None` when the platform has no text
    /// backend or GDI failed.
    #[cfg(windows)]
    pub(crate) fn render(
        &mut self,
        text: &str,
        family: &str,
        px: f32,
        weight: u16,
        italic: bool,
    ) -> Option<Mask> {
        self.gdi.render(text, family, px, weight, italic)
    }

    #[cfg(not(windows))]
    pub(crate) fn render(
        &mut self,
        _text: &str,
        _family: &str,
        _px: f32,
        _weight: u16,
        _italic: bool,
    ) -> Option<Mask> {
        None
    }

    /// Width and height of `text` in device pixels.
    #[cfg(windows)]
    pub(crate) fn measure(
        &mut self,
        text: &str,
        family: &str,
        px: f32,
        weight: u16,
        italic: bool,
    ) -> (f32, f32) {
        self.gdi.measure(text, family, px, weight, italic)
    }

    /// Without a text backend, a rough monospace-ish estimate so layout code still gets numbers.
    #[cfg(not(windows))]
    pub(crate) fn measure(
        &mut self,
        text: &str,
        _family: &str,
        px: f32,
        _weight: u16,
        _italic: bool,
    ) -> (f32, f32) {
        (text.chars().count() as f32 * px * 0.55, px * 1.2)
    }
}
