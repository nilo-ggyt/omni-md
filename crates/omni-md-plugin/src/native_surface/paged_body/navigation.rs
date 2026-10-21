//! Committed document-end selections retain one original request; the sole Producer and SourceTarget
//! still own Source and actual Y.

use super::super::{
    paged_input::NativeMarkdownVerticalGoal,
    paged_text_view::{
        NativeMarkdownPagedNavigation as Nav, NativeMarkdownPagedNavigationContext,
        NativeMarkdownPagedNavigationGeometry,
    },
};
use super::*;
use objc2_foundation::NSRange;

#[derive(Clone, Copy, PartialEq)]
/// Actual document coordinates of the original caret line, without retaining a Scene/reader or
/// estimating an average line height.
pub(super) struct VerticalSeed {
    pub ordinal: usize,
    pub top: f64,
    pub bottom: f64,
    pub caret_y: f64,
    pub x: f64,
    pub goal: Option<NativeMarkdownVerticalGoal>,
}

/// An uncommitted sequence of actual vertical actions retains direction runs and finite selection only;
/// revocation cannot authorize a successor Scene.
pub(super) struct PendingVertical {
    pub request: NativeMarkdownPagedSceneRequest,
    pub geometry: NativeMarkdownPagedNavigationGeometry,
    pub serial: u64,
    pub seed: Cell<VerticalSeed>,
    steps: RefCell<VecDeque<VerticalRun>>,
    selection: Cell<omni_markdown_core::MarkdownTextSelection>,
    processing: Cell<bool>,
    pub resolved: Cell<Option<(usize, f64, f64)>>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct VerticalMotion {
    pub action: Nav,
    pub extend: bool,
}

struct VerticalRun {
    motion: VerticalMotion,
    remaining: usize,
}

impl PendingVertical {
    /// Read one current queue run; repeated motion never expands into Native events or cached document
    /// text.
    pub(super) fn motion(&self) -> Option<VerticalMotion> {
        self.steps.borrow().front().map(|step| step.motion)
    }

    /// Append one actual motion, coalescing only identical direction/extension runs.
    fn append(&self, motion: VerticalMotion) -> std::result::Result<(), ViewError> {
        if self.processing.get() {
            return Err(ViewError::Busy); // The current step owns Native calculation or final commit; reentrant actions cannot enter before their preceding position is known.
        }
        let mut steps = self.steps.borrow_mut();
        if let Some(last) = steps.back_mut().filter(|last| last.motion == motion) {
            let remaining = last
                .remaining
                .checked_add(1)
                .ok_or(ViewError::CounterExhausted)?;
            last.remaining = remaining; // Reject overflow without changing admitted counts; mixed Shift or reverse motion must not merge incorrectly.
        } else {
            if steps.len() == 32 {
                return Err(ViewError::Busy); // Bound unconsumed direction runs only; holding one direction does not truncate at 32 repeats.
            }
            steps.push_back(VerticalRun {
                motion,
                remaining: 1,
            });
        }
        Ok(())
    }

    /// Inspect the next motion before deriving its seed from the actual current caret, including
    /// whether it preserves the target column.
    fn next_motion(&self) -> Option<VerticalMotion> {
        let steps = self.steps.borrow();
        let first = steps.front()?;
        if first.remaining > 1 {
            Some(first.motion)
        } else {
            steps.get(1).map(|next| next.motion)
        }
    }

    /// Consume a step only after an actual Scene hit and original authority validation; BOF/EOF still
    /// apply real clamping per step.
    fn consume_motion(&self) -> std::result::Result<(), ViewError> {
        let mut steps = self.steps.borrow_mut();
        let first = steps.front_mut().ok_or(ViewError::Revoked)?;
        first.remaining -= 1;
        if first.remaining == 0 {
            steps.pop_front();
        }
        Ok(())
    }

    /// Check the original owner, complete Input, epochs, and exact pending-slot identity.
    pub(super) fn current(&self, owner: &NativeMarkdownPagedBody) -> bool {
        owner.current_body()
            && owner.serial.get() == self.serial
            && owner.layout_epoch.get() == self.geometry.layout_epoch
            && Weak::ptr_eq(&self.request.document, &owner.input.document_identity())
            && owner
                .input
                .state()
                .is_ok_and(|state| state == self.request.input)
            && owner
                .pending_vertical
                .borrow()
                .as_ref()
                .is_some_and(|pending| std::ptr::eq(&**pending, self))
    }
    /// Permit the target's own scrolling while preserving original wrapping and viewport height.
    pub(super) fn matches(
        &self,
        owner: &NativeMarkdownPagedBody,
        actual: &NativeMarkdownPagedSceneRequest,
    ) -> bool {
        self.current(owner)
            && actual.target.is_body(&actual.document)
            && Weak::ptr_eq(&actual.document, &self.request.document)
            && actual.input == self.request.input
            && actual.virtual_viewport.layout_width == self.request.virtual_viewport.layout_width
            && actual.virtual_viewport.layout_origin_x
                == self.request.virtual_viewport.layout_origin_x
            && actual.viewport.size.height == self.request.viewport.size.height
    }
    /// Recheck actual Native geometry between original pure-ticket validations.
    pub(super) fn current_native(&self, owner: &NativeMarkdownPagedBody) -> bool {
        self.current(owner)
            && owner.view().is_ok_and(|view| {
                view.capture_scene_request()
                    .is_ok_and(|actual| self.matches(owner, &actual))
            })
            && self.current(owner)
    }
}

impl NativeMarkdownPagedBody {
    /// Complete visible motion in the same Input; only an actually off-screen target creates a
    /// continuation, without changing selection early.
    pub(super) fn accept_vertical_navigation(
        &self,
        actual: &NativeMarkdownPagedSceneRequest,
        action: Nav,
        extend: bool,
    ) -> std::result::Result<(), ViewError> {
        if !matches!(action, Nav::Up | Nav::Down | Nav::PageUp | Nav::PageDown) {
            return Err(ViewError::UnsupportedCommand); // This entry accepts the four vertical actions only; other semantics retain their existing commands.
        }
        actual
            .input
            .directed_selection()
            .ok_or(ViewError::Pending)?;
        if !self.current_body()
            || !actual.target.is_body(&actual.document)
            || !Weak::ptr_eq(&actual.document, &self.input.document_identity())
            || self.input.state()? != actual.input
        {
            return Err(ViewError::Revoked);
        }
        let scene = self.scene.borrow().clone();
        let previous = self.pending_vertical.borrow().clone();
        if let Some(previous) = previous
            .as_ref()
            .filter(|pending| pending.matches(self, actual))
        {
            previous.append(VerticalMotion { action, extend })?;
            return self.schedule().map_err(|_| ViewError::Revoked); // Preserve the original Rc/seed; the next motion must continue from the preceding actual result.
        }
        let (geometry, seed, immediate) = if let Some(scene) = scene
            .filter(|scene| scene.key().request.same(actual) && self.scene_current(scene.key()))
        {
            let geometry = scene.navigation_geometry()?;
            let current = |epoch| {
                self.current(epoch)
                    && self.scene_current(scene.key())
                    && self.input.state().is_ok_and(|state| state == actual.input)
            };
            let permit = self.input.capture(self.configuration.epoch, &current)?;
            let (upstream, goal) = self.input.navigation_selection_facts(&permit, &current)?;
            let context = NativeMarkdownPagedNavigationContext {
                geometry: geometry.clone(),
                head_affinity: upstream.map(
                    super::super::paged_text_view::NativeMarkdownPagedHeadAffinity::from_upstream,
                ),
                vertical_goal: goal.filter(|goal| {
                    action.preserves_vertical_goal() && geometry.permits_goal(*goal)
                }),
                bidi_side: self.input.navigation_bidi_side(&permit, &current)?,
            };
            let seed = scene.vertical_seed(
                actual.input.selection.head_utf16,
                &context,
                action.preserves_vertical_goal(),
            )?;
            let immediate = match scene.navigate(action, extend, &context) {
                Ok(target) => Some(target),
                Err(ViewError::Pending | ViewError::InvalidRange) => None, // The caret is actual but its neighbor line/page is absent; a window edge cannot substitute for that target.
                Err(cause) => return Err(cause),
            };
            if !current(self.configuration.epoch) {
                return Err(ViewError::Revoked);
            }
            (geometry, seed, immediate)
        } else {
            return Err(ViewError::Pending);
        };
        drop(self.pending_vertical.borrow_mut().take()); // Return only the stale old domain; same-domain actions were already appended in order above.
        drop(self.pending_page_scroll.borrow_mut().take()); // The new relative selection owns the viewport; an old scroll-only task cannot reclaim display later.
        self.revoke_source_target();
        drop(self.selection_reveal.borrow_mut().take());
        let pending = Rc::new(PendingVertical {
            request: actual.clone(),
            geometry,
            serial: self.serial.get(),
            seed: Cell::new(seed),
            steps: RefCell::new(VecDeque::from([VerticalRun {
                motion: VerticalMotion { action, extend },
                remaining: 1,
            }])),
            selection: Cell::new(actual.input.selection),
            processing: Cell::new(false),
            resolved: Cell::new(None),
        });
        self.pending_vertical.replace(Some(pending.clone()));
        self.attach_native_vertical(&pending).map_err(|cause| {
            self.fail(cause);
            ViewError::Revoked
        })?; // A vertical action in the original event must hand off its actual receipt before the next key can claim the old caret.
        if let Some(target) = immediate {
            pending.processing.set(true);
            let result = self.commit_vertical_navigation(&pending, target);
            pending.processing.set(false);
            return result;
        }
        self.schedule().map_err(|_| ViewError::Revoked)
    }

    /// Revoke only stale pending motion when actual foreground facts change.
    pub(super) fn refresh_vertical_navigation(&self, actual: &NativeMarkdownPagedSceneRequest) {
        let pending = self.pending_vertical.borrow().clone();
        if pending
            .as_ref()
            .is_some_and(|pending| !pending.matches(self, actual))
        {
            drop(self.pending_vertical.borrow_mut().take()); // Edits, mouse selection, width, or owner changes revoke old work instead of leaving it permanently Pending.
        }
    }

    /// After the Producer locates and scrolls to the actual target line, only its matching new Scene
    /// may hit the original column and commit selection.
    pub(super) fn finish_vertical_navigation(&self) -> Result<()> {
        let pending = self.pending_vertical.borrow().clone();
        let scene = self.scene.borrow().clone();
        let (Some(pending), Some(scene)) = (pending, scene) else {
            return Ok(());
        };
        if !pending.matches(self, &scene.key().request)
            || pending.geometry.source != scene.key().source
        {
            return Ok(());
        }
        let Some((ordinal, _, _)) = pending.resolved.get() else {
            return Ok(());
        };
        if pending.processing.replace(true) {
            return Err(ViewError::Busy.into());
        }
        let result = (|| -> Result<()> {
            let motion = pending.motion().ok_or(ViewError::Revoked)?;
            let seed = pending.seed.get();
            let target = scene.vertical_line_target(
                ordinal,
                seed.x,
                pending.selection.get(),
                motion.extend,
                seed.goal,
            )?;
            let next = pending.next_motion();
            let next_seed = if let Some(next) = next {
                let context = NativeMarkdownPagedNavigationContext {
                    geometry: pending.geometry.clone(),
                    head_affinity: target.head_affinity,
                    vertical_goal: target
                        .vertical_goal
                        .filter(|_| next.action.preserves_vertical_goal()),
                    bidi_side: target.bidi_side,
                };
                Some(scene.vertical_seed(
                    target.selection.head_utf16,
                    &context,
                    next.action.preserves_vertical_goal(),
                )?)
            } else {
                None
            }; // Derive the next step only from this actual caret/line rectangle, never the old before-state or a net line count.
            if !pending.current_native(self) {
                return Err(ViewError::Revoked.into());
            }
            pending.consume_motion()?;
            if let Some(seed) = next_seed {
                pending.selection.set(target.selection); // Store pending scalar facts only, without another Input or mutation of canonical before-state.
                pending.seed.set(seed);
                pending.resolved.set(None);
                let old = self.scene.borrow_mut().take();
                drop(old); // An intermediate hit is not a completed sequence frame; revoke the old CPU candidate and let the same Producer prepare the next actual line.
                return self.schedule();
            }
            self.commit_vertical_navigation(&pending, target)?;
            Ok(())
        })();
        pending.processing.set(false);
        result
    }

    /// Publish one actual selection, then hand its receipt to display and queued input owners.
    fn commit_vertical_navigation(
        &self,
        pending: &Rc<PendingVertical>,
        target: super::super::paged_text_view::NativeMarkdownPagedNavigationTarget,
    ) -> std::result::Result<(), ViewError> {
        let before = self
            .view()
            .map_err(|_| ViewError::Closed)?
            .capture_scene_request()?;
        if !pending.current_native(self) {
            return Err(ViewError::Revoked);
        }
        let current = |epoch| self.current(epoch) && pending.current_native(self);
        let permit = self.input.capture(self.configuration.epoch, &current)?;
        let outcome = self.input.set_selection_intent(
            &permit,
            target.selection,
            target
                .head_affinity
                .map(super::super::paged_text_view::NativeMarkdownPagedHeadAffinity::is_upstream),
            target.vertical_goal,
            &current,
        )?;
        let removed = {
            let mut slot = self.pending_vertical.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|current| Rc::ptr_eq(current, pending))
            {
                slot.take()
            } else {
                None
            }
        };
        drop(removed); // After selection commits, stop requiring its old before-state; only the actual commit may notify display, without replay.
        match outcome {
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Committed(commit) => {
                let commit = Rc::new(commit);
                let keys = self
                    .bind_vertical_native_receipt(pending, &before, Some(Rc::clone(&commit)))
                    .map_err(|cause| {
                        self.fail(cause);
                        ViewError::Revoked
                    })?;
                self.view()
                    .map_err(|_| ViewError::Closed)?
                    .finish_navigation_commit(&commit)?;
                if keys {
                    self.finish_vertical_native_receipt(Some(&commit))
                        .map_err(|cause| {
                            self.fail(cause);
                            ViewError::Revoked
                        })?;
                }
                Ok(())
            }
            super::super::paged_input::NativeMarkdownPagedInputOutcome::Unchanged(_) => {
                let keys = self
                    .bind_vertical_native_receipt(pending, &before, None)
                    .map_err(|cause| {
                        self.fail(cause);
                        ViewError::Revoked
                    })?;
                if keys {
                    self.finish_vertical_native_receipt(None).map_err(|cause| {
                        self.fail(cause);
                        ViewError::Revoked
                    })?;
                }
                Ok(())
            }
        }
    }

    /// Retain the actual weak request for this committed selection; absent Ready, neither acquire
    /// another Source nor read off-screen text.
    pub(super) fn accept_selection_reveal(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
    ) -> std::result::Result<(), ViewError> {
        commit.check_current(&|epoch| self.current(epoch))?;
        let request = self
            .view()
            .map_err(|_| ViewError::Closed)?
            .capture_scene_request()?;
        commit.check_current(&|epoch| self.current(epoch))?;
        if request.input != commit.state
            || !self.input.target_domain()?.same(&request.target)
            || !Weak::ptr_eq(&request.document, &self.input.document_identity())
            || request.input.directed_selection().is_none()
        {
            return Err(ViewError::Revoked);
        } // Accept the same complete Body/Cell post-state only; the original domain constrains Cell-local coordinates without borrowing canonical identity.
        let request = self.synchronize_committed_gutter(commit, &request)?;
        let old = self.selection_reveal.replace(Some(request));
        drop(old); // Return only the original weak request, retaining no old Native event, Source, or history copy.
        self.schedule().map_err(|_| ViewError::Revoked)
    }

    /// Gutter-digit changes authorize this receipt's own horizontal layout only; original Input and the
    /// external scroll container must remain unchanged.
    pub(super) fn synchronize_committed_gutter(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
        before: &NativeMarkdownPagedSceneRequest,
    ) -> std::result::Result<NativeMarkdownPagedSceneRequest, ViewError> {
        let view = self.view().map_err(|_| ViewError::Closed)?;
        commit.check_current(&|epoch| self.current(epoch))?;
        let actual = view.capture_scene_request()?;
        commit.check_current(&|epoch| self.current(epoch))?;
        if !actual.same(before) || actual.input != commit.state {
            return Err(ViewError::Revoked); // Before owned layout begins, a ticket change is external takeover and cannot renew this operation.
        }
        let gutter = self.gutter.borrow().clone();
        let Some(gutter) = gutter else {
            return Ok(actual); // A Cell or gutterless client has no such horizontal change and needs no replacement request or additional layout.
        };
        let scroll = gutter.scrollView();
        commit.check_current(&|epoch| self.current(epoch))?;
        let scroll = scroll.ok_or(ViewError::Closed)?;
        let frame = scroll.frame();
        commit.check_current(&|epoch| self.current(epoch))?;
        let bounds = scroll.bounds();
        commit.check_current(&|epoch| self.current(epoch))?;
        if !view.capture_scene_request()?.same(before) {
            return Err(ViewError::Revoked);
        } // Native reads cannot admit unauthorized geometry changes; retain the actual original container without estimating its new width.
        let lines = self
            .document
            .borrow()
            .as_ref()
            .expect("Source行号属于原Body文档")
            .borrow()
            .line_count();
        let changed = gutter.update_width(lines);
        commit.check_current(&|epoch| self.current(epoch))?;
        let changed = changed?;
        if changed {
            let viewport = self.viewport.borrow().clone().ok_or(ViewError::Closed)?;
            let synchronized = viewport.synchronize();
            commit.check_current(&|epoch| self.current(epoch))?;
            synchronized.map_err(|cause| ViewError::Raster(cause.to_string()))?;
        } // Only this actual gutter-width change may synchronize the body frame; ordinary external resize cannot rebind the input chain.
        let after_frame = scroll.frame();
        commit.check_current(&|epoch| self.current(epoch))?;
        let after_bounds = scroll.bounds();
        commit.check_current(&|epoch| self.current(epoch))?;
        if after_frame != frame || after_bounds != bounds {
            return Err(ViewError::Revoked); // External container size/position change immediately stops original committed effects; it never restores a closed owner.
        }
        let after = view.capture_scene_request()?;
        commit.check_current(&|epoch| self.current(epoch))?;
        if !changed {
            if !after.same(before) {
                return Err(ViewError::Revoked);
            }
        } else if !Weak::ptr_eq(&after.document, &before.document)
            || !after.target.same(&before.target)
            || after.input != commit.state
            || after.viewport.size.height != before.viewport.size.height
            || after.frame_in_parent.size.height != before.frame_in_parent.size.height
            || after.virtual_viewport.document_origin.y != before.virtual_viewport.document_origin.y
        {
            return Err(ViewError::Revoked);
        } // The owned ruler changes horizontal space only; it cannot authorize changed body identity, Input, vertical viewport, or outer container.
        Ok(after)
    }

    /// Bind the original Source once after selecting the sole Producer; revoke stale requests rather
    /// than authorizing old navigation with successor state.
    pub(super) fn bind_selection_reveal(
        &self,
        producer: &Producer,
        actual: &NativeMarkdownPagedSceneRequest,
    ) -> Result<()> {
        let pending = self.selection_reveal.borrow().clone();
        let Some(pending) = pending else {
            return Ok(());
        };
        producer.target_source_health()?;
        if !pending.same(actual) {
            drop(self.selection_reveal.borrow_mut().take());
            return Ok(()); // Input, mouse, or external geometry took over; a successor request cannot continue old intent.
        }
        let serial = self.serial.get();
        let source = producer.target_source_authority(actual, serial)?;
        let scope = || {
            let owns = self
                .selection_reveal
                .borrow()
                .as_ref()
                .is_some_and(|request| request.same(&pending));
            owns && self.current_body()
                && self.serial.get() == serial
                && self.view().is_ok_and(|view| {
                    view.capture_scene_request()
                        .is_ok_and(|request| request.same(&pending))
                })
                && self
                    .selection_reveal
                    .borrow()
                    .as_ref()
                    .is_some_and(|request| request.same(&pending))
        }; // End all slot borrows before Native getters; post-checks detect replacement of the original intent during reentry.
        let result = self.admit_source_target(
            actual,
            source,
            NSRange::new(pending.input.selection.head_utf16, 0),
            &scope,
        );
        producer.target_source_health()?;
        let current = scope();
        producer.target_source_health()?; // The actual Source failure takes precedence; ordinary Input/width revocation cannot hide EIO.
        let target = match result {
            Ok(target) if current => target,
            Ok(target) => {
                target.revoke();
                return Ok(());
            }
            Err(cause) if cause.is_foreground_revocation() => return Ok(()),
            Err(cause) => return Err(cause),
        };
        let removed = {
            let mut slot = self.selection_reveal.borrow_mut();
            if slot.as_ref().is_some_and(|request| request.same(&pending)) {
                slot.take()
            } else {
                None
            }
        };
        if removed.is_none() {
            target.revoke();
            return Ok(()); // An old callback cannot take a successor slot or install its own target into successor state.
        }
        drop(removed);
        // EOF remains an actual zero-length endpoint; the original ActualTarget must reach complete
        // before looking up the final line.
        self.install_source_target(target)
    }
}
