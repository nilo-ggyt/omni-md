//! Find consumes the first position under the same Ready authority; selection and actual viewport visibility are separate facts.

use super::*;

/// Only the actual View constructs this certified proof; weak Key retains no Scene, Source, document, or Native View.
pub struct NativeMarkdownPagedFindVisibleProof {
    key: NativeMarkdownPagedSceneKey,
    range: NSRange,
    first_rect: NSRect,
}
impl NativeMarkdownPagedFindVisibleProof {
    #[must_use]
    pub fn key(&self) -> &NativeMarkdownPagedSceneKey {
        &self.key
    }
    #[must_use]
    pub fn range(&self) -> NSRange {
        self.range
    }
    #[must_use]
    pub fn first_rect(&self) -> NSRect {
        self.first_rect
    }
}

impl NativeMarkdownPagedTextView {
    /// Capture the existing Ready Scene key without layout, scrolling, or choosing a successor nonce for an old request.
    ///
    /// # Errors
    /// Return the actual missing-Ready, original Source, or Input/geometry revocation error.
    pub fn capture_ready_scene_key(&self) -> Result<NativeMarkdownPagedSceneKey> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let scene = self.scene()?;
        if !scene.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(scene.key.clone())
    }

    /// The scan visitor may hold a document read borrow; call no Native, delegate, or reentrant current here.
    /// None allows authorized offscreen Reveal; complete Key/private proof must still establish final visibility.
    #[must_use]
    pub fn find_input_is_current(
        &self,
        input: &Rc<NativeMarkdownPagedInput>,
        epoch: u64,
        expected: &NativeMarkdownPagedInputState,
        geometry_epoch: Option<u64>,
    ) -> bool {
        if self.ivars().closed.get()
            || self.ivars().epoch != epoch
            || self.ivars().geometry_failed.get()
            || geometry_epoch.is_some_and(|epoch| self.ivars().geometry_epoch.get() != epoch)
        {
            return false;
        }
        let owns_input = self.ivars().input.try_borrow().is_ok_and(|owner| {
            owner
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, input))
        });
        owns_input && input.state().is_ok_and(|actual| actual == *expected)
    }

    /// Return scalar geometry generation only, without frame/bounds reads or Native layout.
    #[must_use]
    pub fn find_geometry_epoch(&self) -> Option<u64> {
        (!self.ivars().closed.get() && !self.ivars().geometry_failed.get())
            .then(|| self.ivars().geometry_epoch.get())
    }

    /// A committed Core Find action notifies existing display scheduling through original Input without renewing/replaying text authority.
    ///
    /// # Errors
    /// Closure, mismatched original owner/state, and revoked display effects remain CommittedEffect, never a claim that text was unchanged.
    pub fn apply_find_commit(
        &self,
        input: &Rc<NativeMarkdownPagedInput>,
        commit: &NativeMarkdownPagedInputCommit,
    ) -> Result<()> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let origin = (|| -> Result<Origin> {
            let origin = self.origin()?;
            if !Rc::ptr_eq(&origin.input, input) || origin.state != commit.state {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            commit.check_current(&|epoch| origin.session_current(self, epoch))?;
            Ok(origin)
        })()
        .map_err(|cause| NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause)))?;
        self.finish_commit_ref(&origin, commit)
    }

    /// Certify Find visibility only when the same actual Ready Key's first scalar/caret enters the real clip.
    ///
    /// Perform no layout, Reveal, scroll, or retry. Remain Pending for offscreen/missing Scene;
    /// a later visible match line cannot replace the first position. Clipping limits visibility without changing the user's source range.
    ///
    /// # Errors
    /// Reject revoked authority, invalid range, or hidden first position; current selection/nonempty cache is not successful positioning.
    pub fn verify_find_target_visible(
        &self,
        expected: &NativeMarkdownPagedSceneKey,
        range: NSRange,
    ) -> Result<NativeMarkdownPagedFindVisibleProof> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        let scene = self.scene()?;
        if !expected.same(&scene.key) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        self.range_in_source(&scene, range)?;
        let first_end = if range.length == 0 {
            range.location
        } else {
            let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
            let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
            let mut units = [0u16; 1];
            scene.origin.input.copy_utf16_code_units(
                &permit,
                range.location,
                &mut units,
                &current,
            )?;
            let unit = units[0];
            range
                .location
                .checked_add(if (0xd800..=0xdbff).contains(&unit) {
                    2
                } else {
                    1
                })
                .ok_or(NativeMarkdownPagedTextViewError::InvalidRange)?
        }; // Read one actual UTF-16 unit only; return the borrow before subsequent Scene/Native methods can reenter.
        if !scene.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let first = scene.scene.first_rect(range)?;
        self.range_in_source(&scene, first.actual_range)?;
        let actual_end = first
            .actual_range
            .location
            .checked_add(first.actual_range.length)
            .ok_or(NativeMarkdownPagedTextViewError::InvalidRange)?;
        if first.actual_range.location > range.location || actual_end < first_end {
            return Err(NativeMarkdownPagedTextViewError::Pending);
        } // A later visible match line, text following a table, or virtual EOF cannot substitute for the first glyph.
        if !finite_rect(first.rect, false)
            || first.rect.size.height <= 0.0
            || !finite_endpoints(first.rect)
        {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        let visible = scene.step(self, || self.visibleRect())?;
        let bounds = scene.step(self, || self.bounds())?;
        let visible = bounded_viewport(visible, bounds)
            .filter(|visible| *visible == expected.request.viewport)
            .ok_or(NativeMarkdownPagedTextViewError::Revoked)?;
        let first_rect = visible_first_rect(first.rect, visible)
            .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
        if !scene.current(self) || !expected.same(&scene.key) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(NativeMarkdownPagedFindVisibleProof {
            key: scene.key.clone(),
            range,
            first_rect,
        })
    }
}

fn finite_endpoints(rect: NSRect) -> bool {
    (rect.origin.x + rect.size.width).is_finite() && (rect.origin.y + rect.size.height).is_finite()
}

/// A zero-width caret is a valid vertical line; nonempty glyphs need positive-width intersection, not mere edge contact.
fn visible_first_rect(first: NSRect, clip: NSRect) -> Option<NSRect> {
    if !finite_rect(clip, true) || !finite_endpoints(clip) {
        return None;
    }
    let x = first.origin.x.max(clip.origin.x);
    let y = first.origin.y.max(clip.origin.y);
    let right = (first.origin.x + first.size.width).min(clip.origin.x + clip.size.width);
    let top = (first.origin.y + first.size.height).min(clip.origin.y + clip.size.height);
    if top <= y
        || (first.size.width == 0.0 && (first.origin.x < clip.origin.x || first.origin.x > right))
        || (first.size.width != 0.0 && right <= x)
    {
        return None;
    }
    Some(NSRect::new(
        NSPoint::new(x, y),
        objc2_foundation::NSSize::new(right - x, top - y),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_foundation::NSSize;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
    }

    #[test]
    fn first_glyph_requires_real_intersection_while_edge_caret_remains_visible() {
        let clip = rect(10.0, 10.0, 100.0, 100.0);
        assert_eq!(
            visible_first_rect(rect(0.0, 50.0, 30.0, 20.0), clip),
            Some(rect(10.0, 50.0, 20.0, 20.0))
        );
        assert!(visible_first_rect(rect(0.0, 50.0, 10.0, 20.0), clip).is_none());
        for x in [10.0, 110.0] {
            assert_eq!(
                visible_first_rect(rect(x, 50.0, 0.0, 20.0), clip),
                Some(rect(x, 50.0, 0.0, 20.0))
            );
        } // Carets may touch the clip boundary; zero width does not mean a missing text position.
        for x in [9.0, 111.0] {
            assert!(visible_first_rect(rect(x, 50.0, 0.0, 20.0), clip).is_none());
        }
        assert!(visible_first_rect(rect(20.0, 0.0, 20.0, 10.0), clip).is_none());
        assert!(visible_first_rect(rect(20.0, 110.0, 20.0, 10.0), clip).is_none());
    }

    #[test]
    fn invalid_or_overflowing_clip_cannot_certify_a_position() {
        let first = rect(20.0, 20.0, 1.0, 18.0);
        for clip in [
            rect(0.0, 0.0, 0.0, 20.0),
            rect(0.0, 0.0, 20.0, -1.0),
            rect(f64::NAN, 0.0, 20.0, 20.0),
            rect(f64::MAX, 0.0, f64::MAX, 20.0),
        ] {
            assert!(visible_first_rect(first, clip).is_none());
        }
    }
}
