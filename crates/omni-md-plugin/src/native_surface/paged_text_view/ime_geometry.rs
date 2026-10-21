//! Insertion-point geometry for a system composition session retains only coordinates,
//! without extending the old Scene, Source, font, or pixel lifetime.
//!
//! The IME queries candidate geometry immediately after setMarkedText, before asynchronous
//! layout can publish the new body. It needs the actual insertion point from this
//! composition start, not a Ready claim for new glyphs. Hit testing and drawing still use the current
//! Scene.

use super::*;

#[derive(Clone, Copy)]
pub(super) struct CompositionAnchor {
    state: NativeMarkdownPagedInputState,
    geometry_epoch: u64,
    location: usize,
    rect: NSRect,
}

impl NativeMarkdownPagedTextView {
    /// Capture the original composition insertion point before Core mutation; continuation requires the
    /// same client, geometry, and complete Input.
    /// Release the Scene after capture; a Core commit must not consult old layout or old text.
    pub(super) fn capture_composition_anchor(&self, origin: &Origin) -> Option<CompositionAnchor> {
        let location = origin
            .state
            .marked
            .unwrap_or(origin.state.primary_range)
            .location;
        let previous = self.ivars().composition_anchor.get().filter(|anchor| {
            anchor.state == origin.state
                && anchor.geometry_epoch == origin.geometry_epoch
                && anchor.location == location
                && origin.current(self)
        });
        // Prefer the actually published layout; only an existing composition's valid in-flight update
        // may inherit its original insertion point.
        // Geometry is not an input prerequisite: the original client may accept text, but no measured
        // anchor means no successful coordinate result.
        let scene = match self.scene_for_origin(origin.clone()) {
            Ok(scene) => scene,
            Err(NativeMarkdownPagedTextViewError::Pending) => return previous,
            Err(_) => return None,
        };
        self.composition_anchor_from_scene(&scene, location)
    }

    /// Accept only a complete, finite caret rectangle certified by this Scene.
    fn composition_anchor_from_scene(
        &self,
        scene: &SceneUse,
        location: usize,
    ) -> Option<CompositionAnchor> {
        let range = NSRange::new(location, 0);
        self.range_in_source(scene, range).ok()?;
        let value = scene.scene.first_rect(range).ok()?;
        if value.actual_range != range
            || !finite_rect(value.rect, false)
            || value.rect.size.height <= 0.0
            || !scene.current(self)
        {
            return None;
        }
        Some(CompositionAnchor {
            state: scene.origin.state,
            geometry_epoch: scene.origin.geometry_epoch,
            location,
            rect: value.rect,
        })
    }

    /// Transfer the insertion point to committed marked state only when this Core transaction actually
    /// replaces the original selection or composition range.
    /// Replacement elsewhere, fold changes, a different client, or scrolling ends the transfer; the
    /// coordinate cannot authorize another edit.
    pub(super) fn bind_composition_anchor(
        &self,
        origin: &Origin,
        commit: &NativeMarkdownPagedInputCommit,
        anchor: Option<CompositionAnchor>,
    ) {
        self.ivars().composition_anchor.set(None);
        let (Some(mut anchor), Some(marked)) = (anchor, commit.state.marked) else {
            return;
        };
        let replaced = origin.state.marked.unwrap_or(origin.state.primary_range);
        let Some(end) = replaced.location.checked_add(replaced.length) else {
            return;
        };
        // An unmark-only provisional update may preserve its range; all character
        // changes must replace exactly the original composition/selection range.
        let same_range = commit
            .invalidation
            .replaced_utf16_range
            .as_ref()
            .is_some_and(|range| *range == (replaced.location..end));
        let same_provisional =
            !commit.invalidation.characters_changed && origin.state.marked == commit.state.marked;
        if (!same_range && !same_provisional)
            || commit.invalidation.folds_changed
            || anchor.location != replaced.location
            || marked.location != anchor.location
            || self.ivars().geometry_epoch.get() != anchor.geometry_epoch
            || self.ivars().geometry_failed.get()
            || self.ivars().closed.get()
            || self
                .ivars()
                .input
                .borrow()
                .as_ref()
                .is_none_or(|input| input.state().ok() != Some(commit.state))
        {
            return;
        }
        // Store only scalar facts of this committed Input; a later command revokes the anchor when the
        // complete state differs.
        anchor.state = commit.state;
        self.ivars().composition_anchor.set(Some(anchor));
    }

    /// Revoke the anchor for commits outside its original composition transfer, preventing it from
    /// surviving undo, Find, or client changes.
    pub(super) fn retire_other_composition_anchor(&self, state: NativeMarkdownPagedInputState) {
        if self
            .ivars()
            .composition_anchor
            .get()
            .is_some_and(|anchor| anchor.state != state || state.marked.is_none())
        {
            self.ivars().composition_anchor.set(None);
        }
    }

    /// Refresh after actual layout publication without forcing a draw or treating this coordinate as
    /// CPU completion proof.
    pub(super) fn refresh_composition_anchor(&self, scene: &SceneUse) {
        let anchor = scene
            .origin
            .state
            .marked
            .and_then(|marked| self.composition_anchor_from_scene(scene, marked.location));
        if scene.current(self) {
            self.ivars().composition_anchor.set(anchor);
        }
    }

    /// Return a measured insertion point only for system composition candidates; an empty actualRange
    /// does not claim newly shaped glyphs.
    /// Ranges outside current marked text, real scroll/resize, and other input cannot use it to bypass
    /// Pending.
    pub(super) fn composition_screen_rect(
        &self,
        range: NSRange,
    ) -> Result<NativeMarkdownPagedCharacterRect> {
        let origin = self.origin()?;
        let marked = origin
            .state
            .marked
            .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
        let anchor = self
            .ivars()
            .composition_anchor
            .get()
            .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
        let end = range
            .location
            .checked_add(range.length)
            .ok_or(NativeMarkdownPagedTextViewError::InvalidRange)?;
        let marked_end = marked
            .location
            .checked_add(marked.length)
            .ok_or(NativeMarkdownPagedTextViewError::InvalidRange)?;
        if anchor.state != origin.state
            || anchor.geometry_epoch != origin.geometry_epoch
            || anchor.location != marked.location
            || range.location < marked.location
            || end > marked_end
        {
            return Err(NativeMarkdownPagedTextViewError::Pending);
        }
        // Convert through the original window one Native boundary at a time.
        // Origin::step rejects reentrant client or geometry changes after each call.
        let window = origin
            .step(self, || self.window())?
            .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
        let base = origin.step(self, || self.convertRect_toView(anchor.rect, None))?;
        let rect = origin.step(self, || window.convertRectToScreen(base))?;
        if !finite_rect(rect, false) || rect.size.height <= 0.0 {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        Ok(NativeMarkdownPagedCharacterRect {
            rect,
            actual_range: NSRange::new(anchor.location, 0),
        })
    }
}
