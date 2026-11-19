//! Interaction regions: the contract between a painter and the crate's hit-testing and dispatch.
//!
//! A painter returns, with every frame, the list of regions the surface wants to react to. The
//! crate does not know what they are: it gives each rectangle to the overlay as a hit region,
//! tracks which ones the pointer is over, fades their hover amount, and on a click does what the
//! region's [`Action`] says. [`RegionId`] and [`ActionTag`] are opaque numbers the host defines
//! and maps back to its own meanings; the crate only compares and echoes them.
//!
//! Where regions overlap, the one listed last wins (the overlay's rule), so a painter lists the
//! body of a surface first and its controls after it.

use crate::draw::VRect;

/// The three surfaces the crate owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Surface {
    Pill = 0,
    Hint = 1,
    Clip = 2,
}

/// Opaque region identifier, chosen by the painter. Unique within one surface's frame. Ids at or
/// above [`RegionId::RESERVED_FROM`] belong to the crate (the capsule body and the visual extent
/// of a morphing capsule); a painter must not use them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RegionId(pub u32);

impl RegionId {
    pub const RESERVED_FROM: u32 = 0xFFFF_FF00;
    /// The capsule body, added by the crate from `PillFrame::capsule`.
    pub(crate) const CAPSULE: RegionId = RegionId(0xFFFF_FFFE);
    /// The area a morphing capsule paints in; visual only, never hover, never clickable.
    pub(crate) const VISUAL: RegionId = RegionId(0xFFFF_FFFF);

    pub fn is_reserved(self) -> bool {
        self.0 >= Self::RESERVED_FROM
    }
}

/// Opaque action number the host maps to one of its callbacks (see `Callbacks::activate`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ActionTag(pub u32);

/// The area a region covers, in the surface's window coordinates (logical px). Only rectangles
/// exist today because the overlay hit-tests by rectangle; a round button is its bounding box.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum Shape {
    Rect(VRect),
}

impl Shape {
    pub fn bounds(&self) -> VRect {
        match self {
            Shape::Rect(rect) => *rect,
        }
    }
}

/// The pointer shape over a region.
///
/// **Inert on Windows: nothing applies it.** `rightkit-overlay-win` 0.1.6 has no per-region
/// cursor: its window class carries the arrow, `HitRegion` is `{ id, rect }`, and it exposes no
/// message hook or subclass point, so this crate cannot answer `WM_SETCURSOR` for it. The field
/// is carried and compared (a change re-lays-out the region) and the driver reports one
/// `cursor_inert` event per generation the first time a frame carries a non-`Default` cursor, so
/// the gap shows in the host's log. Applying it requires a region-cursor overlay API;
/// there is no feature flag or implementation here until that API exists.
/// [`cursor_at`] is the reference hit-test (later regions win,
/// edges inclusive) that API must match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cursor {
    Default,
    Pointer,
}

/// The cursor the regions ask for at window point `(x, y)` (logical px): the region listed last
/// that contains the point wins, edges inclusive, exactly the overlay's hit rule. `Default` where
/// no region covers the point.
pub fn cursor_at(regions: &[Region], x: f32, y: f32) -> Cursor {
    regions
        .iter()
        .rev()
        .find(|region| region.rect().contains(x, y))
        .map_or(Cursor::Default, |region| region.cursor)
}

/// How the pointer's presence on a region counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverRole {
    /// Part of the surface and highlightable: the pointer over it counts as over the surface
    /// and its hover amount fades in (`HotSet`), which the painter reads.
    Control,
    /// Part of the surface, not highlightable (a capsule or card body). The pointer over it
    /// counts as over the surface; clicks on it are swallowed.
    Body,
}

/// What a click on a region does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing; the click is swallowed.
    None,
    /// Close the card that owns this region (it plays its exit animation).
    Dismiss,
    /// Clip card: copy the single card's shown payload (`Callbacks::copy_text`). The card shows
    /// "copied" feedback, and (for the single card) closes, only after the host reports success
    /// (`true`); on failure or no answer within `Timings::copy_deadline_ms` it shows the failure
    /// feedback and stays open.
    CopyCurrent,
    /// Clip card: copy `ClipState::items[index]`'s primary payload, with the same feedback rules.
    CopyItem(usize),
    /// Clip card: switch the single card between its primary and alternate payloads. The crate
    /// attaches no meaning to the pair (the host decides what they are, for example raw and
    /// polished).
    ToggleAlternate,
    /// Run `Callbacks::activate` on a worker thread. With `close_card_after` (a card's own
    /// region) or `close_clip_on_success` (a capsule region, clip card open) the card closes only
    /// when the callback returns `ActionOutcome::Done`; `Empty`, `Failed`, a panic or no answer
    /// within `Timings::host_action_deadline_ms` keep it open. Without either flag the outcome
    /// is not read (nothing on screen depends on it).
    Host(ActionTag),
    /// As `Host`, and the returned `ActionOutcome` drives the pill's copy feedback (a check mark
    /// for `Done`, a dimmed button for `Empty`, `Failed`, a panic or no answer within the
    /// deadline). While the action is in flight or the dimmed state shows, a click on this
    /// region is ignored.
    HostFlash(ActionTag),
}

/// One interaction region of a surface.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub id: RegionId,
    pub shape: Shape,
    pub cursor: Cursor,
    pub hover: HoverRole,
    pub action: Action,
    /// Close the clip card (if one is open when the region is clicked) once the action reports
    /// `ActionOutcome::Done`, never before. Only an `Action::Host` reads it; on any other action
    /// it does nothing.
    pub close_clip_on_success: bool,
    /// Close the card that owns this region after the action: at once for the actions that
    /// finish synchronously (`None`, `Dismiss`, `ToggleAlternate`), on the host's `Done` for
    /// `Action::Host`, on a confirmed copy for `CopyCurrent` / `CopyItem`.
    pub close_card_after: bool,
    /// Action for a right-button press while the pointer rests on this region (run through
    /// `Callbacks::activate_secondary`, on the driver thread, so it must return quickly).
    pub secondary: Option<ActionTag>,
}

impl Region {
    pub fn new(id: RegionId, rect: VRect, hover: HoverRole) -> Region {
        Region {
            id,
            shape: Shape::Rect(rect),
            cursor: Cursor::Default,
            hover,
            action: Action::None,
            close_clip_on_success: false,
            close_card_after: false,
            secondary: None,
        }
    }

    pub fn cursor(mut self, cursor: Cursor) -> Region {
        self.cursor = cursor;
        self
    }

    pub fn action(mut self, action: Action) -> Region {
        self.action = action;
        self
    }

    pub fn closing_clip_on_success(mut self) -> Region {
        self.close_clip_on_success = true;
        self
    }

    pub fn closing_card_after(mut self) -> Region {
        self.close_card_after = true;
        self
    }

    pub fn secondary(mut self, tag: ActionTag) -> Region {
        self.secondary = Some(tag);
        self
    }

    pub fn rect(&self) -> VRect {
        self.shape.bounds()
    }
}

/// What the crate hands the host when a region's action fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Activation {
    pub surface: Surface,
    pub region: RegionId,
    pub tag: ActionTag,
}

/// Per-region hover amount `0.0..=1.0` (a fade), as the painter reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HotSet(pub(crate) Vec<(RegionId, f32)>);

impl HotSet {
    pub fn empty() -> HotSet {
        HotSet(Vec::new())
    }

    /// The hover amount of `id`; `0.0` for a region that was never hovered.
    pub fn get(&self, id: RegionId) -> f32 {
        self.0
            .iter()
            .find(|(region, _)| *region == id)
            .map_or(0.0, |(_, amount)| *amount)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> VRect {
        VRect::new(x, y, w, h)
    }

    #[test]
    fn the_last_listed_region_under_the_point_decides_the_cursor() {
        let regions = vec![
            Region::new(RegionId(1), rect(0.0, 0.0, 100.0, 40.0), HoverRole::Body),
            Region::new(
                RegionId(2),
                rect(10.0, 10.0, 20.0, 20.0),
                HoverRole::Control,
            )
            .cursor(Cursor::Pointer),
        ];
        assert_eq!(cursor_at(&regions, 15.0, 15.0), Cursor::Pointer);
        assert_eq!(cursor_at(&regions, 50.0, 15.0), Cursor::Default);
    }

    #[test]
    fn edges_are_inclusive_and_outside_is_default() {
        let regions = vec![Region::new(
            RegionId(1),
            rect(10.0, 10.0, 20.0, 20.0),
            HoverRole::Control,
        )
        .cursor(Cursor::Pointer)];
        assert_eq!(cursor_at(&regions, 10.0, 10.0), Cursor::Pointer);
        assert_eq!(cursor_at(&regions, 30.0, 30.0), Cursor::Pointer);
        assert_eq!(cursor_at(&regions, 30.5, 30.0), Cursor::Default);
        assert_eq!(cursor_at(&[], 1.0, 1.0), Cursor::Default);
    }

    #[test]
    fn a_body_listed_after_a_control_hides_its_cursor() {
        let regions = vec![
            Region::new(RegionId(1), rect(0.0, 0.0, 50.0, 50.0), HoverRole::Control)
                .cursor(Cursor::Pointer),
            Region::new(RegionId(2), rect(0.0, 0.0, 50.0, 50.0), HoverRole::Body),
        ];
        assert_eq!(cursor_at(&regions, 5.0, 5.0), Cursor::Default);
    }

    #[test]
    fn reserved_ids_belong_to_the_crate() {
        assert!(RegionId::CAPSULE.is_reserved());
        assert!(RegionId::VISUAL.is_reserved());
        assert!(!RegionId(7).is_reserved());
    }

    #[test]
    fn hot_set_reads_zero_for_a_region_never_hovered() {
        let hot = HotSet(vec![(RegionId(3), 0.5)]);
        assert_eq!(hot.get(RegionId(3)), 0.5);
        assert_eq!(hot.get(RegionId(4)), 0.0);
        assert_eq!(HotSet::empty().get(RegionId(3)), 0.0);
    }
}
