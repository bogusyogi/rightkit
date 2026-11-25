//! The sequencing of a hover card on its one window: it comes out of the notch, moves to another
//! ring as a [`Glide`], is replaced in place, and goes back in, with a glide in flight handed from
//! the card on show to the card closing and back. The host owns the window, the drawing and the
//! card it shows; this owns what the card is doing and when it is done.
//!
//! `P` is the host's card content; it is cloned, never read.

use crate::glide::Glide;
use crate::render::Body;
use crate::swell::SwellPair;

/// A card going back into the notch: what it showed, and where.
pub struct Closing<P> {
    pub panel: P,
    pub dpi: u32,
    /// The card window's key, as the host names it.
    pub window: isize,
    /// The glide the card was on when it was put away: it goes on while the card goes in.
    pub glide: Option<Glide<P>>,
    /// The notch it hung from, and where its window was (where it was going, on a glide), for a
    /// card wanted again before it is in: that one glides from here.
    pub key: isize,
    pub at: (i32, i32),
}

/// What one step of the card on show did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShownStep {
    /// The slide or the open still moves or waits to.
    pub swelling: bool,
    /// A glide is still going.
    pub gliding: bool,
    /// A glide that has just arrived, with the place its window ends up: the host draws the plain
    /// card there.
    pub arrived: Option<(i32, i32)>,
}

impl ShownStep {
    /// Nothing moved and nothing arrived: the host draws no frame and stops its timer, unless the
    /// card's own contents animate.
    pub fn settled(&self, animated: bool) -> bool {
        !animated && !self.gliding && self.arrived.is_none() && !self.swelling
    }
}

/// One step of the card going back in.
pub struct ClosingStep<P> {
    pub panel: P,
    pub dpi: u32,
    pub window: isize,
    pub slide: f32,
    pub open: f32,
    /// The slide or the open still moves or waits to.
    pub moving: bool,
    /// The glide it is still on, to draw this frame with.
    pub glide: Option<Glide<P>>,
    /// A glide that has just arrived, with the place its window ends up.
    pub arrived: Option<(i32, i32)>,
}

impl<P> ClosingStep<P> {
    /// The strip has slid in and settled: the host hides the card window and calls
    /// [`CardFlow::finish_closing`].
    pub fn done(&self) -> bool {
        !self.moving && self.slide <= 0.0
    }
}

/// What the card window is doing.
pub struct CardFlow<P> {
    /// How far the card has slid out of the notch and how far it has opened.
    pub swell: SwellPair,
    /// Where the card window is (its top-left; where it is going while it glides).
    pub at: Option<(i32, i32)>,
    /// The glide it is on, if any.
    pub glide: Option<Glide<P>>,
    /// A card on its way back into the notch, after it was put away.
    pub closing: Option<Closing<P>>,
}

impl<P: Clone> CardFlow<P> {
    pub const fn new() -> Self {
        Self {
            swell: SwellPair::new(),
            at: None,
            glide: None,
            closing: None,
        }
    }

    /// The card is put away. `leaving` is the card on show with its DPI and window, and `place`
    /// the notch it hangs from with where its window is. When it is out it goes back into the
    /// notch (true) and the host hides its window at the end of [`CardFlow::step_closing`];
    /// otherwise everything is cleared and the host hides the window now (false). A card already
    /// closing keeps closing.
    pub fn put_away(
        &mut self,
        leaving: Option<(P, u32, isize)>,
        place: Option<(isize, (i32, i32))>,
    ) -> bool {
        let glide = self.glide.take();
        let closing = match (leaving, place) {
            (Some((panel, dpi, window)), Some((key, at)))
                if self.swell.slide.spring.value > 0.0 =>
            {
                self.closing = Some(Closing {
                    panel,
                    dpi,
                    window,
                    glide,
                    key,
                    at,
                });
                self.swell.fall();
                true
            }
            _ => self.closing.is_some(),
        };
        if !closing {
            self.swell.reset();
        }
        self.at = None;
        closing
    }

    /// The card a wanted hover card replaces, with where its window was: the card on show for
    /// `key` (`shown_here`, which the host passes only for a plain hover card on that notch) or,
    /// when it was wanted again before it was in, the card closing on that notch. `plain` is false
    /// for a popup or a notice, which never glide.
    pub fn prior(
        &self,
        key: isize,
        plain: bool,
        shown_here: Option<&P>,
    ) -> Option<(P, (i32, i32))> {
        if !plain {
            return None;
        }
        match (shown_here, self.at) {
            (Some(panel), Some(now)) => Some((panel.clone(), now)),
            (None, _) => self
                .closing
                .as_ref()
                .filter(|closing| closing.key == key)
                .map(|closing| (closing.panel.clone(), closing.at)),
            _ => None,
        }
    }

    /// The card is wanted at `at`. `prior` is [`CardFlow::prior`]; `bodies` the old card's body at
    /// its place and the new card's body at `at`, when they differ. A glide the closing card was
    /// on carries on toward the new place; one already going to `at` carries on; one going
    /// elsewhere turns round with the speed it has; a card with no prior has none.
    pub fn move_to(
        &mut self,
        prior: Option<(P, (i32, i32))>,
        bodies: Option<(Body, Body)>,
        at: (i32, i32),
    ) {
        if prior.is_some() && self.glide.is_none() {
            self.glide = self
                .closing
                .as_mut()
                .and_then(|closing| closing.glide.take());
        }
        let going = self.glide.as_ref().map(|glide| glide.target);
        match (prior, bodies, going) {
            (Some(_), _, Some(target)) if target == at => {}
            (Some((old, _)), Some((_, to)), Some(_)) => {
                if let Some(glide) = self.glide.as_mut() {
                    glide.retarget(to, old);
                }
            }
            (Some((old, _)), Some((from, to)), None) => {
                self.glide = Some(Glide::new(from, to, old));
            }
            _ => self.glide = None,
        }
        self.at = Some(at);
    }

    /// The card is drawn. A card that comes up from nothing (`nothing_shown`) comes out of the
    /// notch and any card closing is dropped; one already up (moving to another ring, or
    /// changing) does not come out again.
    pub fn emerge(&mut self, nothing_shown: bool) {
        if nothing_shown {
            self.closing = None;
            self.swell.rise();
        }
    }

    /// Anything the host's frame timer must keep running for: a swell, a glide, a closing card.
    pub fn animating(&self) -> bool {
        self.swell.moving() || self.glide.is_some() || self.closing.is_some()
    }

    /// Nothing takes a click until the contents are past half opacity: smoothstep
    /// `((open - 0.55) / 0.4)` at 0.5, that is open at 0.75 (the Mac's `allowsHitTesting`).
    pub fn accepts_hits(&self) -> bool {
        self.swell.open.spring.value.min(1.0) > 0.75
    }

    /// One frame of the card on show: the swell and the glide step to now. A glide that has
    /// arrived is dropped and reported in `arrived`, so the host hands the card back to the
    /// plain picture at its place.
    pub fn step_shown(&mut self) -> ShownStep {
        let swelling = self.swell.step();
        let (gliding, arrived) = match self
            .glide
            .as_mut()
            .map(|glide| (glide.step(), glide.target))
        {
            Some((true, target)) => {
                self.glide = None;
                (false, Some(target))
            }
            Some((false, _)) => (true, None),
            None => (false, None),
        };
        ShownStep {
            swelling,
            gliding,
            arrived,
        }
    }

    /// One frame of the card going back in; `None` when none is. A glide it was on carries on
    /// while it goes in, and is reported in `arrived` when it arrives.
    pub fn step_closing(&mut self) -> Option<ClosingStep<P>> {
        let (panel, dpi, window) = self
            .closing
            .as_ref()
            .map(|closing| (closing.panel.clone(), closing.dpi, closing.window))?;
        let moving = self.swell.step();
        let (slide, open) = self.swell.values();
        let mut glide = None;
        let mut arrived = None;
        if let Some(held) = self
            .closing
            .as_mut()
            .and_then(|closing| closing.glide.as_mut())
        {
            if held.step() {
                arrived = Some(held.target);
            } else {
                glide = Some(held.clone());
            }
        }
        if arrived.is_some()
            && let Some(closing) = self.closing.as_mut()
        {
            closing.glide = None;
        }
        Some(ClosingStep {
            panel,
            dpi,
            window,
            slide,
            open,
            moving,
            glide,
            arrived,
        })
    }

    /// The card is in: drop it.
    pub fn finish_closing(&mut self) {
        self.closing = None;
    }
}

impl<P: Clone> Default for CardFlow<P> {
    fn default() -> Self {
        Self::new()
    }
}
