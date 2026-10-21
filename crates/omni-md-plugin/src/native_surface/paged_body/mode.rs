//! Mode and Find layouts retain finite original Source anchors; restoration uses
//! the sole Producer and its original Source target continuation.

use super::NativeMarkdownPagedBodyError as Error;
use super::*;
use crate::native_surface::editor::viewport::ModeSourceAnchor;
use crate::native_surface::paged_input::NativeMarkdownPagedInputState;
use crate::native_surface::{
    NativeMarkdownController, find_bar::NativeMarkdownFindBar,
    native_markdown_controller_has_foreground_authority,
    workspace_menu_intent::NativeMarkdownWorkspacePagedEditIntent,
};
use objc2::{ClassType, DefinedClass};
use objc2_foundation::{NSPoint, NSRange, NSRect, NSSize};
use omni_markdown_core::MarkdownPresentationMode;

/// Retains no Source, reader, Scene, or Native strong reference; target tickets contain weak identities
/// and finite scalar facts.
pub(super) struct PendingModeViewport {
    anchor: ModeSourceAnchor,
    state: NativeMarkdownPagedInputState,
    target: Option<Rc<target::SourceTargetRequest>>,
    controller: Option<Rc<ModeController>>,
    // Present only after the same Source and wrapping width produced a real glyph measurement; it does
    // not certify scroll completion.
    // Keep it until a new Scene certifies the original on-screen position; whole-row navigation must
    // not replace this glyph anchor.
    measured_glyph: Option<MeasuredModeGlyph>,
    // Hiding Find returns only bar height; retain exact document coordinates without a round trip
    // through local glyph coordinates.
    // The same target certifies Source, Input, and wrapping width; the original mode separately
    // constrains coordinate reuse.
    find_origin: Option<FindLayoutOrigin>,
}

/// Pure facts for the original Find-height change; these retain no Scene, Native view, or additional
/// Source owner.
#[derive(Clone, Copy)]
struct FindLayoutOrigin {
    position: NSPoint,
    presentation_mode: MarkdownPresentationMode,
}

/// Measured document top and complete glyph height from one Scene; retains geometry without fonts,
/// lines, or Source.
#[derive(Clone, Copy)]
struct MeasuredModeGlyph {
    document_top: f64,
    height: f64,
}

impl MeasuredModeGlyph {
    /// After font or mode changes, make only the nearest adjustment needed for full visibility; retain
    /// a valid original position exactly.
    /// A glyph taller than the viewport exposes its top; never shrink the glyph or invent a shorter
    /// measurement.
    fn requested_origin(self, screen_y: f64, viewport_height: f64) -> f64 {
        let last_top = (viewport_height - self.height).max(0.0);
        let visible_y = screen_y.clamp(0.0, last_top);
        (self.document_top - visible_y).max(0.0)
    }
}

/// Mode position belongs to the original window input; retain only weak Native identity and existing
/// controller revisions.
struct ModeController {
    controller: objc2::rc::Weak<NativeMarkdownController>,
    find_input: u64,
    header: u64,
    workspace_input: u64,
}
impl ModeController {
    /// Check pure epochs of the controller already held by the caller; do not acquire a new weak ticket
    /// between Native operations.
    fn permits(&self, controller: &NativeMarkdownController) -> bool {
        native_markdown_controller_has_foreground_authority(controller)
            && controller.find_input_intent() == self.find_input
            && controller.ivars().header_layout_revision.get() == self.header
            && controller.workspace_query_intent() == self.workspace_input
    }

    /// Load the original controller only at a Native boundary; pure Core checks neither load it nor
    /// capture successor input.
    fn current_native(&self) -> bool {
        self.controller
            .load()
            .is_some_and(|controller| self.permits(&controller))
    }
}

/// The sole mode-button action waiting for its original layout to become Ready; it retains no Scene,
/// Source, reader, or Native strong edge.
/// Publication records an actual weak Scene key and queued records one submission; neither means the
/// mode switch finished.
pub(super) struct PendingModeAction {
    original: Rc<NativeMarkdownWorkspacePagedEditIntent>,
    controller: Rc<ModeController>,
    revision: u64,
    request: NativeMarkdownPagedSceneRequest,
    previous: MarkdownPresentationMode,
    destination: MarkdownPresentationMode,
    publication: RefCell<Option<NativeMarkdownPagedSceneKey>>,
    queued: Cell<bool>,
    cancelled: Cell<bool>,
}

impl PendingModeAction {
    /// After revocation, compensate only the original foreground Doc/Body button; a successor note on
    /// the same controller is a different target.
    fn owns_surface(
        &self,
        owner: &NativeMarkdownPagedBody,
        controller: &NativeMarkdownController,
    ) -> bool {
        crate::native_surface::ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            active.as_ref().is_some_and(|surface| {
                std::ptr::eq(&*surface.controller, controller)
                    && native_markdown_controller_has_foreground_authority(controller)
                    && surface.editor.as_ref().is_some_and(|editor| {
                        Weak::ptr_eq(&self.request.document, &Rc::downgrade(editor.document()))
                            && std::ptr::eq(&*editor.body, owner)
                    })
            })
        })
    }

    /// Queued work requires the original document, complete parent/child Input, mode, and geometry;
    /// current Ready cannot renew old intent.
    fn current(
        &self,
        owner: &NativeMarkdownPagedBody,
        controller: &NativeMarkdownController,
    ) -> bool {
        owner.current_body()
            && owner.mode_action_revision.get() == self.revision
            && owner.presentation_mode() == self.previous
            && self.original.current()
            && self.controller.permits(controller)
            && owner.view().is_ok_and(|view| {
                view.capture_scene_request()
                    .is_ok_and(|actual| actual.same(&self.request))
            })
            && self.original.current()
            && self.controller.permits(controller)
            && owner.mode_action_revision.get() == self.revision
            && owner.presentation_mode() == self.previous
    }
}

impl NativeMarkdownPagedBody {
    /// The click binds its original Input before Native work; capture that geometry and button state
    /// without resigning or committing while Pending.
    pub(in crate::native_surface) fn accept_presentation_mode_click(
        &self,
        controller: &NativeMarkdownController,
        original: NativeMarkdownWorkspacePagedEditIntent,
        sender: &objc2::runtime::AnyObject,
    ) {
        // A mode click is independent input; a reentrant click from a getter must revoke the enclosing
        // action even on the same button.
        // Advance the epoch before the first Native call to identify the latest click; it does not
        // certify mode or layout completion.
        let revision = self
            .mode_action_revision
            .get()
            .checked_add(1)
            .expect("mode action revision overflow");
        self.mode_action_revision.set(revision);
        let find_input = controller.find_input_intent();
        let header = controller.ivars().header_layout_revision.get();
        let workspace_input = controller.workspace_query_intent();
        let old = self.pending_mode_action.borrow_mut().take();
        let original = Rc::new(original);
        let scope = Rc::new(ModeController {
            find_input,
            header,
            workspace_input,
            controller: objc2::rc::Weak::from(controller),
        });
        // Freeze all epochs before constructing the first Native weak reference; releasing an old weak
        // reference may reenter and cannot renew input.
        // Every new click revokes its predecessor, including a return to the current mode; do not
        // overwrite the newly received button state.
        drop(old);
        let admitted = self.prepare_presentation_mode_click(
            controller,
            Rc::clone(&original),
            Rc::clone(&scope),
            revision,
            sender,
        );
        if let Err(cause) = admitted {
            // Synchronous rejection also belongs to the original Input/Doc; an old error cannot roll
            // back the button after newer or reentrant input.
            let current = || {
                self.mode_action_revision.get() == revision
                    && original.current()
                    && scope.permits(controller)
                    && self.pending_mode_action.borrow().is_none()
            };
            if current() {
                controller.select_presentation_mode(self.presentation_mode());
                if current() {
                    controller.present_error(&cause);
                }
            }
        }
    }

    /// Install one action only after capturing its actual target and geometry; before Ready, wait
    /// without running the mode transaction.
    fn prepare_presentation_mode_click(
        &self,
        controller: &NativeMarkdownController,
        original: Rc<NativeMarkdownWorkspacePagedEditIntent>,
        scope: Rc<ModeController>,
        revision: u64,
        sender: &objc2::runtime::AnyObject,
    ) -> std::result::Result<(), String> {
        let previous = self.presentation_mode();
        let current = || {
            self.current_body()
                && self.mode_action_revision.get() == revision
                && self.presentation_mode() == previous
                && original.current()
                && scope.permits(controller)
        };
        if !current() {
            return Err("展示模式动作已失效".into());
        }
        let view = self.view().map_err(|cause| cause.to_string())?;
        // Capture through the original View API without requiring a Scene; invalid geometry remains an
        // error rather than an invented old anchor.
        let request = crate::native_surface::native_markdown_table_intent_step(&current, || {
            view.capture_scene_request()
        })
        .map_err(|_| "展示模式捕获期间原输入已改变")?
        .map_err(|cause| cause.to_string())?;
        let button = crate::native_surface::native_markdown_table_intent_step(&current, || {
            sender.downcast_ref::<objc2_app_kit::NSButton>()
        })
        .map_err(|_| "展示模式选择期间原输入已改变")?
        .ok_or("展示模式动作缺少原源码按钮")?;
        let owned = controller
            .ivars()
            .presentation_mode_button
            .get()
            .expect("原窗口已装配源码按钮");
        if !std::ptr::eq(button, &**owned) {
            return Err("展示模式动作不属于原源码按钮".into());
        }
        let state =
            crate::native_surface::native_markdown_table_intent_step(&current, || button.state())
                .map_err(|_| "展示模式选择期间原输入已改变")?;
        let destination = if state == objc2_app_kit::NSControlStateValueOn {
            MarkdownPresentationMode::Source
        } else if state == objc2_app_kit::NSControlStateValueOff {
            MarkdownPresentationMode::InlinePreview
        } else {
            return Err("展示模式选择无效".into());
        };
        if destination == previous {
            controller.select_presentation_mode(previous);
            return Ok(());
        }
        let pending = Rc::new(PendingModeAction {
            original,
            controller: scope,
            revision,
            request,
            previous,
            destination,
            publication: RefCell::new(None),
            queued: Cell::new(false),
            cancelled: Cell::new(false),
        });
        if !pending.current(self, controller) {
            return Err("展示模式捕获的原几何或输入已改变".into());
        }
        *self.pending_mode_action.borrow_mut() = Some(Rc::clone(&pending));
        match view.capture_ready_scene_key() {
            Ok(key) => *pending.publication.borrow_mut() = Some(key),
            Err(ViewError::Pending) => {} // The original ready_scene already woke the sole Producer; wait for actual publication without retry polling.
            Err(cause) => {
                drop(self.take_mode_action(&pending));
                return Err(cause.to_string());
            }
        }
        self.deliver_pending_mode_action();
        Ok(())
    }

    /// Called only from the two successful Scene publication exits; record a weak key without switching
    /// mode inside a Producer or Native borrow.
    pub(super) fn mode_scene_published(&self) {
        let pending = self.pending_mode_action.borrow().clone();
        let Some(pending) =
            pending.filter(|pending| !pending.queued.get() && !pending.cancelled.get())
        else {
            return;
        };
        let key = self
            .scene
            .borrow()
            .as_ref()
            .map(|scene| scene.key().clone());
        // A successor publication is only a notification; delivery still validates the original request
        // and cannot acquire successor authority.
        *pending.publication.borrow_mut() = key;
    }

    /// Submit one owned task after advance and synchronous borrows end; without actual publication
    /// there is no work and no Ready polling.
    pub(super) fn deliver_pending_mode_action(&self) {
        if self.preparing.get() {
            return;
        }
        let pending = self.pending_mode_action.borrow().clone();
        let Some(pending) = pending.filter(|pending| !pending.queued.get()) else {
            return;
        };
        let key = pending.publication.borrow_mut().take();
        if key.is_none() && !pending.cancelled.get() {
            return;
        }
        pending.queued.set(true);
        let Some(controller) = pending.controller.controller.load() else {
            drop(self.take_mode_action(&pending));
            return;
        };
        let body = MainThreadBound::new(self.weak.clone(), self.mtm);
        let pending = MainThreadBound::new(Rc::downgrade(&pending), self.mtm);
        let key = MainThreadBound::new(key, self.mtm);
        // The Body already admitted this action using its weak document identity, complete Input,
        // geometry, and revision.
        // An AppKit nested loop may temporarily borrow Active; generic enqueue_owned recognizes only
        // the slotted owner and could
        // discard valid work before this closure while leaving queued=true. Submit directly to the
        // original main queue and let
        // the pending identity and slot decide execution or cancellation. Busy remains a real
        // transaction failure without replay or renewed tickets.
        crate::native_surface::enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("原模式动作只在主队列交付");
            let (Some(owner), Some(pending)) =
                (body.get(mtm).upgrade(), pending.get(mtm).upgrade())
            else {
                return;
            };
            let Some(controller) = pending.controller.controller.load() else {
                drop(owner.take_mode_action(&pending));
                return;
            };
            let valid = !pending.cancelled.get()
                && pending.current(&owner, &controller)
                && key.get(mtm).as_ref().is_some_and(|key| {
                    key.presentation_mode == pending.previous
                        && key.request.same(&pending.request)
                        && owner.view().is_ok_and(|view| {
                            view.capture_ready_scene_key()
                                .is_ok_and(|actual| actual.same(key))
                        })
                })
                && pending.current(&owner, &controller);
            // Slot identity is the final admission check: a reentrant new click replaces it, and this
            // old closure must not remove that successor.
            let Some(accepted) = owner.take_mode_action(&pending) else {
                return;
            };
            drop(accepted);
            if !valid {
                if pending.owns_surface(&owner, &controller)
                    && owner.pending_mode_action.borrow().is_none()
                {
                    controller.select_presentation_mode(owner.presentation_mode());
                }
                return;
            }
            // Only now run the original resignation, Cell commit, and mode transaction once; report
            // failure through the original entry without replay.
            if let Err(cause) = crate::native_surface::set_active_native_markdown_presentation_mode(
                pending.destination,
            ) && pending.owns_surface(&owner, &controller)
                && owner.mode_action_revision.get() == pending.revision
                && owner.pending_mode_action.borrow().is_none()
            {
                // This transaction may legitimately advance Input or Find; failure feedback checks the
                // original Doc/Body and click, never a successor note.
                controller.present_error(&cause);
            }
        });
        // Keep the same temporary strong edge as owned enqueue; the queue carries weak identities and
        // controller destruction happens outside slot borrows.
        drop(controller);
    }

    /// Remove only this action identity; destroy the returned owner after the slot borrow ends, keeping
    /// Native weak release outside RefMut.
    fn take_mode_action(&self, original: &Rc<PendingModeAction>) -> Option<Rc<PendingModeAction>> {
        let mut slot = self.pending_mode_action.borrow_mut();
        if slot
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, original))
        {
            slot.take()
        } else {
            None
        }
    }

    /// Real input or external scroll revokes the old ticket but retains its slot until delivery can
    /// detect a successor click and restore the button safely.
    /// Queue cancellation feedback once; failure and close release the slot without another mode
    /// pipeline or layout retry.
    pub(super) fn cancel_pending_mode_action(&self) {
        let pending = self.pending_mode_action.borrow().clone();
        if let Some(pending) = pending {
            pending.cancelled.set(true);
            let publication = pending.publication.borrow_mut().take();
            drop(publication);
            self.deliver_pending_mode_action();
        }
    }

    /// Borrow one actual glyph from the current Ready Scene without advancing layout, estimating by
    /// line number, or changing selection.
    pub(in crate::native_surface) fn capture_mode_viewport(
        &self,
    ) -> Result<(ModeSourceAnchor, NativeMarkdownPagedSceneKey)> {
        let view = self.view()?;
        let key = view.capture_ready_scene_key()?;
        let scene = self.scene.borrow().clone().ok_or(ViewError::Pending)?;
        if !scene.key().same(&key) || !self.scene_current(&key) {
            return Err(ViewError::Revoked.into());
        }
        let document = self.document.borrow().clone().ok_or(ViewError::Revoked)?;
        let clip = key.request.viewport;
        for offset in scene.mode_source_candidates() {
            let offset = offset?; // Do not hide a real Cell-directory I/O failure by skipping to another candidate.
            // Read at most one scalar for a nonempty candidate; the same sealed Scene certifies
            // grapheme geometry without reading a whole line.
            let probe = {
                let document = document.borrow();
                if offset > key.request.input.utf16_len {
                    return Err(ViewError::InvalidRange.into());
                }
                if offset == key.request.input.utf16_len {
                    offset..offset
                } else {
                    let unit = document
                        .utf16_code_unit_at(offset)
                        .map_err(ViewError::Document)?;
                    let end = offset
                        + if (0xd800..=0xdbff).contains(&unit) {
                            2
                        } else {
                            1
                        };
                    document
                        .byte_offset_for_utf16(offset)
                        .map_err(ViewError::Document)?;
                    document
                        .byte_offset_for_utf16(end)
                        .map_err(ViewError::Document)?;
                    offset..end
                }
            };
            let first = match scene.first_rect(NSRange::new(probe.start, probe.len())) {
                Ok(first) => first,
                Err(_cause @ (ViewError::Pending | ViewError::InvalidRange)) => {
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    if std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some() {
                        eprintln!(
                            "native_source_mode_capture candidate={}..{} error={_cause:?} primary={:?} canonical={:?}",
                            probe.start,
                            probe.end,
                            key.request.input.primary_range,
                            key.request.input.selection
                        );
                    }
                    continue;
                }
                Err(cause) => return Err(cause.into()),
            };
            let frame = first.rect;
            let end = first
                .actual_range
                .location
                .checked_add(first.actual_range.length)
                .ok_or(ViewError::InvalidRange)?;
            // Use the complete glyph rectangle for top offset; skip clipped lines instead of treating
            // the clip intersection as a complete glyph.
            if first.actual_range.location > probe.start
                || end < probe.end
                || ![
                    frame.origin.x,
                    frame.origin.y,
                    frame.size.width,
                    frame.size.height,
                    frame.origin.x + frame.size.width,
                    frame.origin.y + frame.size.height,
                ]
                .into_iter()
                .all(f64::is_finite)
                || frame.size.height <= 0.0
                // The original minimal-scroll contract allows half a point only for floating-point
                // rounding at real clip and glyph edges.
                || frame.origin.y < clip.origin.y - 0.5
                || frame.origin.y + frame.size.height > clip.origin.y + clip.size.height + 0.5
                || frame.origin.x + frame.size.width < clip.origin.x
                || frame.origin.x > clip.origin.x + clip.size.width
            {
                #[cfg(feature = "native-markdown-runtime-probe")]
                if std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some() {
                    eprintln!(
                        "native_source_mode_capture candidate={}..{} accepted=false first={:?} actual_range={:?} clip={clip:?} top_clip={} bottom_clip={} primary={:?} canonical={:?}",
                        probe.start,
                        probe.end,
                        frame,
                        first.actual_range,
                        (frame.origin.y + frame.size.height) - (clip.origin.y + clip.size.height),
                        clip.origin.y - frame.origin.y,
                        key.request.input.primary_range,
                        key.request.input.selection
                    );
                }
                continue;
            }
            #[cfg(feature = "native-markdown-runtime-probe")]
            if std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some() {
                eprintln!(
                    "native_source_mode_capture candidate={}..{} accepted=true first={frame:?} actual_range={:?} clip={clip:?} primary={:?} canonical={:?}",
                    probe.start,
                    probe.end,
                    first.actual_range,
                    key.request.input.primary_range,
                    key.request.input.selection
                );
            }
            if !self.scene_current(&key) || !view.capture_ready_scene_key()?.same(&key) {
                return Err(ViewError::Revoked.into());
            }
            return Ok((
                ModeSourceAnchor {
                    document: Rc::downgrade(&document),
                    generation: key.request.input.generation,
                    length: key.request.input.utf16_len,
                    selection: key.request.input.selection,
                    probe,
                    // Source uses unflipped Core Graphics coordinates; retain the actual top offset
                    // consistently with downward text coordinates.
                    screen_y: clip.origin.y + clip.size.height - frame.origin.y - frame.size.height,
                },
                key,
            ));
        }
        Err(ViewError::Pending.into())
    }

    /// Preserve the same Source glyph when hiding Find increases viewport height; Native clipping must
    /// not mistake a measured prefix for EOF.
    /// Synchronous layout borrows existing preparation admission; prefix growth and restoration stay
    /// with the same pending viewport and Composer.
    pub(in crate::native_surface) fn hide_find_bar(
        &self,
        bar: &Rc<NativeMarkdownFindBar>,
        scroll: &NSScrollView,
        controller: &NativeMarkdownController,
    ) -> std::result::Result<bool, String> {
        if !bar.is_visible() {
            return Ok(false);
        }
        let view = self.view().map_err(|cause| cause.to_string())?;
        // Hiding may proceed while the Producer is borrowed, but this scope must not release another
        // scope's preparation admission.
        // Create a release guard only when this operation acquires admission; later user scroll still
        // revokes the anchor through existing notifications.
        let was_preparing = self.preparing.replace(true);
        let preparation =
            (!was_preparing).then(|| super::presentation::PresentationUse(&self.preparing));
        let mut installed = None;
        let result = (|| {
            let scope = Rc::new(ModeController {
                find_input: controller.find_input_intent(),
                header: controller.ivars().header_layout_revision.get(),
                workspace_input: controller.workspace_query_intent(),
                controller: objc2::rc::Weak::from(controller),
            });
            if !scope.permits(controller) {
                return Err(ViewError::Revoked.to_string());
            }
            self.cancel_pending_mode_action();
            // The restoring position belongs to the original Source request; Hide transfers its Find
            // scope without choosing another anchor or Source.
            let inherited = self.inherit_find_layout_anchor(&scope, controller);
            self.cancel_pending_find()
                .map_err(|cause| cause.to_string())?;
            if !inherited && !was_preparing && self.scene.borrow().is_some() {
                match self.begin_find_layout_anchor(&scope, controller) {
                    Ok(target) => installed = Some(target),
                    Err(Error::View(ViewError::Pending | ViewError::Busy | ViewError::Revoked)) => {
                        // Hide still executes without a certifiable glyph, but never claims to restore
                        // a position that was not captured.
                    }
                    Err(cause) => return Err(cause.to_string()),
                }
            }
            let hidden = bar.hide(scroll, view.as_super(), controller)?;
            if installed
                .as_ref()
                .is_some_and(|target| !target.current_native(self))
            {
                return Err(ViewError::Revoked.to_string());
            }
            Ok(hidden)
        })();
        drop(preparation); // Resume the sole original main-queue quantum only after synchronous Native calls and slot borrows end.
        if was_preparing {
            // The outer owner retains its worker completions and deferred work; this layout requests
            // the same successor without clearing another scope's slots.
            self.deferred.set(true);
            return result;
        }
        // This layout owns the admission; one original schedule consumes its layout request and
        // registered deferred work.
        // Worker notices remain on their original queue for normal completion handling, rather than
        // being consumed by this layout scope.
        self.deferred.set(false);
        // Native cancellation cannot hide the frozen Source error; actual failure retires resources
        // through the Body's existing failure path.
        let health = self
            .producer
            .borrow()
            .as_ref()
            .map_or(Ok(()), |producer| producer.target_source_health());
        if let Err(cause) = health {
            let message = cause.to_string();
            self.fail(cause);
            return Err(message);
        }
        if result.is_err()
            && let Some(target) = &installed
        {
            let owns = self.mode_viewport.borrow().as_ref().is_some_and(|pending| {
                pending
                    .target
                    .as_ref()
                    .is_some_and(|actual| Rc::ptr_eq(actual, target))
            });
            if owns {
                self.cancel_mode_viewport(); // Revoke only this operation's installed ticket; an old failure cannot clear reentrant successor work.
            }
        }
        if self.current_body() {
            self.schedule().map_err(|cause| cause.to_string())?;
        }
        result
    }

    /// Transfer only the same-window Hide scope of an existing anchor; Source, target, measurement, and
    /// glyph position remain original.
    fn inherit_find_layout_anchor(
        &self,
        scope: &Rc<ModeController>,
        controller: &NativeMarkdownController,
    ) -> bool {
        let captured = self.mode_viewport.borrow().as_ref().map(|pending| {
            (
                pending.anchor.clone(),
                pending.state,
                pending.target.clone(),
                pending.controller.clone(),
            )
        });
        let Some((anchor, state, target, Some(original))) = captured else {
            return false;
        };
        // Load the weak Native owner outside the slot borrow, then verify the original scope Rc and
        // target so replaced or revoked tickets stay revoked.
        if !original
            .controller
            .load()
            .is_some_and(|actual| std::ptr::eq(&*actual, controller))
            || !scope.permits(controller)
            || !anchor.current_document()
            || self.input.state().ok() != Some(state)
            || target.as_ref().is_some_and(|target| !target.current(self))
        {
            return false;
        }
        let previous = {
            let mut slot = self.mode_viewport.borrow_mut();
            let Some(pending) = slot.as_mut() else {
                return false;
            };
            let same_target = match (&pending.target, &target) {
                (Some(actual), Some(original)) => Rc::ptr_eq(actual, original),
                (None, None) => true,
                _ => false,
            };
            if !same_target
                || !pending
                    .controller
                    .as_ref()
                    .is_some_and(|actual| Rc::ptr_eq(actual, &original))
            {
                return false;
            }
            pending.controller.replace(Rc::clone(scope))
        };
        drop(previous); // Release the old Native weak control block outside the mode borrow; successor input may still revoke the fixed original slot.
        true
    }

    /// Create this Find anchor only from an actual Ready Scene; retain the existing Producer and Source
    /// without another document or parser.
    fn begin_find_layout_anchor(
        &self,
        scope: &Rc<ModeController>,
        controller: &NativeMarkdownController,
    ) -> Result<Rc<target::SourceTargetRequest>> {
        let (anchor, key) = self.capture_mode_viewport()?;
        let scene = self.scene.borrow().clone().ok_or(ViewError::Pending)?;
        if !scene.key().same(&key) {
            return Err(ViewError::Revoked.into());
        }
        // Measure from the same Scene nonce as the anchor; source ranges and line numbers cannot
        // determine font, marker visibility, or glyph height.
        // Retain the actual document top so temporary clipping to a short prefix during Hide cannot
        // replace the original position intent.
        let first = scene.first_rect(NSRange::new(anchor.probe.start, anchor.probe.len()))?;
        let measured = MeasuredModeGlyph {
            document_top: key.request.virtual_viewport.document_origin.y
                + key.request.viewport.origin.y
                + key.request.viewport.size.height
                - first.rect.origin.y
                - first.rect.size.height,
            height: first.rect.size.height,
        };
        drop(scene); // Keep only actual scalar measurements across layout; the anchor must not retain the old Scene or reader.
        if !scope.permits(controller)
            || !self
                .view()?
                .capture_ready_scene_key()
                .is_ok_and(|actual| actual.same(&key))
        {
            return Err(ViewError::Revoked.into());
        }
        // The original Producer API verifies Source health and the complete request before lending
        // identity; no mapping copy or parser request is created.
        // The original target computes the new viewport bottom from measurements; only actual Composer
        // EOF may stop prefix growth.
        let source = self
            .producer
            .borrow()
            .as_ref()
            .ok_or(ViewError::Pending)?
            .target_source_authority(&key.request, self.serial.get())?;
        let target = self.admit_source_target(
            &key.request,
            source,
            NSRange::new(anchor.probe.start, anchor.probe.len()),
            &|| anchor.current_document() && scope.permits(controller),
        )?;
        self.cancel_mode_viewport();
        self.install_source_target(Rc::clone(&target))?;
        // Installation releases the old reader and queues original work; reentrant input revokes this
        // ticket instead of attaching successor state to an old anchor.
        if !target.current(self) || !scope.permits(controller) {
            target.revoke();
            return Err(ViewError::Revoked.into());
        }
        *self.mode_viewport.borrow_mut() = Some(PendingModeViewport {
            anchor,
            state: key.request.input,
            target: Some(Rc::clone(&target)),
            controller: Some(Rc::clone(scope)),
            measured_glyph: Some(measured),
            find_origin: Some(FindLayoutOrigin {
                position: key.request.virtual_viewport.document_origin,
                presentation_mode: key.presentation_mode,
            }),
        });
        Ok(target)
    }

    /// Register once before first Ready; creation failure keeps the previous frame without temporary
    /// selections or synthetic undo for positioning.
    pub(in crate::native_surface) fn restore_mode_viewport(
        &self,
        anchor: ModeSourceAnchor,
    ) -> Result<()> {
        if self.producer.borrow().is_some() || self.scene.borrow().is_some() {
            return Err(ViewError::Busy.into());
        }
        let state = self.input.state().map_err(ViewError::Input)?;
        if !anchor.current_document()
            || !Weak::ptr_eq(&anchor.document, &self.input.document_identity())
            || state.generation != anchor.generation
            || state.utf16_len != anchor.length
            || state.selection != anchor.selection
        {
            return Err(ViewError::Revoked.into());
        }
        *self.mode_viewport.borrow_mut() = Some(PendingModeViewport {
            anchor,
            state,
            target: None,
            controller: None,
            measured_glyph: None,
            find_origin: None, // Mode and font changes retain their real glyph anchor instead of borrowing Find's absolute origin.
        });
        self.schedule()
    }

    /// The sole new scroll root already completed layout and Find focus transfer; bind controller
    /// authority without initializing another body Scene.
    pub(in crate::native_surface) fn bind_mode_viewport(
        &self,
        controller: &Retained<NativeMarkdownController>,
    ) -> Result<()> {
        let scope = ModeController {
            controller: objc2::rc::Weak::from_retained(controller),
            find_input: controller.find_input_intent(),
            header: controller.ivars().header_layout_revision.get(),
            workspace_input: controller.workspace_query_intent(),
        };
        if !scope.current_native() {
            self.cancel_mode_viewport();
            if self.current_body() {
                // Outer focus or mount failure may have consumed initial preparation; after cancelling
                // the old anchor, resume the current mode explicitly.
                self.schedule()?;
            }
            return Err(ViewError::Revoked.into());
        }
        let scope = Rc::new(scope);
        let previous = self
            .mode_viewport
            .borrow_mut()
            .as_mut()
            .and_then(|pending| pending.controller.replace(Rc::clone(&scope)));
        drop(previous); // Destroy the previous Objective-C weak slot after the RefCell borrow ends; weak-copy and deallocation must not run inside the slot borrow.
        self.schedule()
    }

    /// Do not publish document start while Native mounting is active; stale controller input revokes
    /// only the old mode target, retaining successor input.
    pub(super) fn mode_viewport_waits_for_mount(&self) -> bool {
        let scope = {
            let slot = self.mode_viewport.borrow();
            let Some(pending) = slot.as_ref() else {
                return false;
            };
            pending.controller.clone()
        };
        let Some(scope) = scope else {
            return true;
        };
        if !scope.current_native() {
            self.cancel_mode_viewport();
        }
        false
    }

    /// This window scope constrains only its original mode target; Find and navigation retain their own
    /// original authority.
    pub(super) fn mode_target_current_native(&self, target: &target::SourceTargetRequest) -> bool {
        let scope = {
            let slot = self.mode_viewport.borrow();
            let Some(pending) = slot.as_ref() else {
                return true;
            };
            if !pending
                .target
                .as_ref()
                .is_some_and(|actual| std::ptr::eq(&**actual, target))
            {
                return true;
            }
            pending.controller.clone()
        };
        scope.is_some_and(|scope| scope.current_native())
    }

    /// User scroll or external resize revokes this old clip ticket; preparation-owned height and
    /// scrolling do not use this path.
    pub(super) fn cancel_mode_for_external_clip(&self) {
        if self.preparing.get() {
            return;
        }
        self.cancel_pending_mode_action();
        let admitted = self
            .mode_viewport
            .borrow()
            .as_ref()
            .is_some_and(|pending| pending.target.is_some());
        if admitted {
            self.cancel_mode_viewport();
        }
    }

    /// Detach the finite old ticket, then revoke only the identical target; never clear a later Find or
    /// another Source action.
    pub(super) fn cancel_mode_viewport(&self) {
        self.cancel_pending_mode_action();
        let pending = self.mode_viewport.borrow_mut().take();
        let Some(target) = pending.and_then(|pending| pending.target) else {
            return;
        };
        target.revoke();
        let previous = {
            let mut slot = self.source_target.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, &target))
            {
                slot.take()
            } else {
                None
            }
        };
        drop(previous);
    }

    /// The first real Producer issues the same-Source target without another capture or an intermediate
    /// Ready frame at document start.
    pub(super) fn prepare_mode_viewport(
        &self,
        producer: &mut Producer,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<()> {
        let (state, anchor, target) = {
            let slot = self.mode_viewport.borrow();
            let Some(pending) = slot.as_ref() else {
                return Ok(());
            };
            (
                pending.state,
                pending.anchor.clone(),
                pending.target.clone(),
            )
        }; // End the initialization-slot borrow before Native scheduling or revocation may reenter.

        if request.input != state || !anchor.current_document() {
            drop(self.mode_viewport.borrow_mut().take());
            return Ok(()); // Successor input revokes the old position; current selection cannot renew the original mode action.
        }
        if let Some(target) = target {
            if !target.matches(self, request) {
                drop(self.mode_viewport.borrow_mut().take());
            }
            return Ok(());
        }
        let source = producer.target_source_authority(request, self.serial.get())?;
        let target = self.admit_source_target(
            request,
            source,
            NSRange::new(anchor.probe.start, anchor.probe.len()),
            &|| anchor.current_document() && self.input.state().is_ok_and(|actual| actual == state),
        )?;
        self.install_source_target(Rc::clone(&target))?;
        if let Some(pending) = self.mode_viewport.borrow_mut().as_mut() {
            pending.target = Some(target);
        }
        Ok(())
    }

    /// The measured target top determines the required lower body; continue the original prefix instead
    /// of declaring restoration after clamping to a short page.
    pub(super) fn mode_viewport_required_bottom(
        &self,
        target: &Rc<target::SourceTargetRequest>,
        line_top: f64,
        viewport_height: f64,
    ) -> Option<f64> {
        let slot = self.mode_viewport.borrow();
        let pending = slot.as_ref()?;
        pending
            .target
            .as_ref()
            .filter(|actual| Rc::ptr_eq(actual, target))?;
        // Before a Scene exists, extend from the real line top; once measured, use the Cell glyph's
        // actual document position.
        // A target may lie inside a tall Cell; the row top cannot prove content below the original
        // on-screen anchor is prepared.
        let origin = if let Some(original) = pending.find_origin
            && original.presentation_mode == self.presentation_mode()
        {
            // For the same Source, Input, and width, extend only the new visible height below the exact
            // original coordinate; avoid coordinate round trips.
            original.position.y
        } else {
            pending.measured_glyph.map_or_else(
                || (line_top - pending.anchor.screen_y).max(0.0),
                |glyph| glyph.requested_origin(pending.anchor.screen_y, viewport_height),
            )
        };
        Some(origin + viewport_height)
    }

    /// Take over subsequent scroll only after the original mode target has a measured glyph; initial
    /// remote positioning remains with target preparation.
    /// This checks identical tickets and finite measurements only; it certifies neither Ready,
    /// visibility, nor completion of external operations.
    pub(super) fn mode_retains_measured_glyph(
        &self,
        target: &Rc<target::SourceTargetRequest>,
    ) -> bool {
        self.mode_viewport.borrow().as_ref().is_some_and(|pending| {
            pending.measured_glyph.is_some()
                && pending
                    .target
                    .as_ref()
                    .is_some_and(|actual| Rc::ptr_eq(actual, target))
        })
    }

    /// Restore on-screen Y from the measured glyph after actual target layout; a pre-scroll Scene can
    /// never issue final visibility proof.
    pub(super) fn finish_mode_viewport(&self) -> Result<bool> {
        let (anchor, state, target, find_origin, measured_glyph) = {
            let slot = self.mode_viewport.borrow();
            let Some(pending) = slot.as_ref() else {
                return Ok(false);
            };
            (
                pending.anchor.clone(),
                pending.state,
                pending.target.clone(),
                pending.find_origin,
                pending.measured_glyph,
            )
        }; // Query glyph and clip geometry outside the initialization slot; failure cleanup must not recursively borrow that RefCell.

        let Some(target) = target else {
            return Ok(false);
        };
        let scene = self.scene.borrow().clone();
        let Some(scene) = scene else {
            // Preparation and the original Producer ticket already own controller, Input, and Source
            // revocation for this turn.
            // Without a Scene, there is no measured glyph and no Native scroll; do not capture all
            // Native geometry for each Core quantum.
            // Once a Scene is published, the full original Input and target checks still guard both
            // sides of each real side effect.
            return Ok(false);
        };
        if self.input.state().map_err(ViewError::Input)? != state || !target.current_native(self) {
            drop(self.mode_viewport.borrow_mut().take());
            return Ok(false);
        }
        if !target.matches(self, &scene.key().request) || target.source != scene.key().source {
            return Ok(false);
        }
        let first = match scene.first_rect(NSRange::new(anchor.probe.start, anchor.probe.len())) {
            Ok(first) => first,
            Err(ViewError::Pending) => return Ok(false),
            Err(cause) => {
                // Label the existing failure exit only; avoid additional Scene/current queries and do
                // not turn rejected old coordinates into a retry.
                #[cfg(feature = "native-close-stage-diagnostic")]
                eprintln!(
                    "native_mode_phase_failure phase=first_rect cause={:?}",
                    std::mem::discriminant(&cause)
                );
                return Err(cause.into());
            }
        };
        let request = &scene.key().request;
        let viewport = self.viewport.borrow().clone().ok_or(ViewError::Closed)?;
        let geometry = viewport.geometry().inspect_err(|cause| {
            #[cfg(feature = "native-close-stage-diagnostic")]
            eprintln!(
                "native_mode_phase_failure phase=geometry cause={:?}",
                std::mem::discriminant(cause)
            );
            let _ = cause;
        })?;
        // The original target certifies Source, complete Input, and width; only the same actual mode
        // may reuse the exact Find origin.
        // Font and mode changes measure a new glyph; old coordinates must not prevent layout around the
        // original on-screen anchor.
        let find_origin = find_origin
            .filter(|original| original.presentation_mode == scene.key().presentation_mode);
        let measured = if find_origin.is_some() {
            // Find already measured this glyph in the same Scene; a pure height change must not derive
            // document top again from a new local Y.
            measured_glyph.expect("Find 布局原点始终伴随原 Scene 的真实字形测量")
        } else {
            MeasuredModeGlyph {
                document_top: request.virtual_viewport.document_origin.y
                    + request.viewport.origin.y
                    + request.viewport.size.height
                    - first.rect.origin.y
                    - first.rect.size.height,
                height: first.rect.size.height,
            }
        };
        let glyph_top = measured.document_top;
        let maximum = (geometry.content_height - geometry.body_frame.size.height).max(0.0);
        let requested = find_origin.map_or_else(
            || measured.requested_origin(anchor.screen_y, geometry.body_frame.size.height),
            |original| original.position.y,
        );
        let desired = requested.min(maximum);
        if !glyph_top.is_finite() || !desired.is_finite() {
            return Err(ViewError::InvalidGeometry.into());
        }
        // Transfer subsequent scrolling only after the current same-ticket Scene certifies first_rect.
        // Native reveal may change geometry; retain measurements without consuming the mode, and
        // revalidate the next Scene.
        if let Some(pending) = self.mode_viewport.borrow_mut().as_mut()
            && pending
                .target
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, &target))
        {
            pending.measured_glyph = Some(measured);
        }
        // A Scene may end midway through a tall Cell; its temporary page bottom must not replace the
        // original on-screen position.
        // Let the sole Producer prepare the visible area below the actual target; clamping before
        // actual EOF never means completion.
        let required_bottom = requested + geometry.body_frame.size.height;
        let needs_prefix = self
            .producer
            .borrow()
            .as_ref()
            .expect("模式 Scene 仍归唯一实际 Producer")
            .needs_document_prefix(required_bottom);
        if needs_prefix {
            // Publication dirtied this intermediate Scene before restoration completed. Revoke its
            // ticket before the CPU observer can
            // mistake it for a completed mode frame ahead of prefix work; retain the measurements and
            // sole Producer.
            self.clear_scene()?;
            self.schedule()?;
            return Ok(true);
        }
        let changed = viewport.reveal_document_rect(
            NSRect::new(
                NSPoint::new(
                    find_origin
                        .map_or(geometry.body_frame.origin.x, |original| original.position.x),
                    desired,
                ),
                NSSize::new(
                    1.0,
                    geometry.body_frame.size.height.min(geometry.content_height),
                ),
            ),
            &|| target.current_native(self),
        );
        // Ordinary foreground cancellation cannot hide the original Source I/O error; the Native
        // operation was consumed and must never replay.
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?;
        }
        #[cfg(feature = "native-close-stage-diagnostic")]
        if let Err(cause) = &changed {
            // The original Source error still takes precedence above; this diagnostic neither consumes
            // nor replays the Native reveal.
            eprintln!(
                "native_mode_phase_failure phase=reveal cause={:?}",
                std::mem::discriminant(cause)
            );
        }
        if changed? {
            self.schedule()?;
            return Ok(true);
        }
        let actual_y = request.viewport.origin.y + request.viewport.size.height
            - first.rect.origin.y
            - first.rect.size.height;
        let expected_y = glyph_top - desired;
        #[cfg(feature = "native-markdown-runtime-probe")]
        eprintln!(
            "native_source_mode_position scalar={}..{} old_screen_y={} glyph_document_top={glyph_top} requested_origin={} constrained_origin={desired} actual_origin={} actual_screen_y={actual_y} prefix_height={} glyph={:?} viewport={:?}",
            anchor.probe.start,
            anchor.probe.end,
            anchor.screen_y,
            requested,
            request.virtual_viewport.document_origin.y,
            geometry.content_height,
            first.rect,
            request.viewport
        );
        if !target.current_native(self) || (actual_y - expected_y).abs() > 0.5 {
            return Err(ViewError::Revoked.into());
        }
        self.view()?
            .verify_find_target_visible(scene.key(), target.range)
            .inspect_err(|cause| {
                #[cfg(feature = "native-close-stage-diagnostic")]
                eprintln!(
                    "native_mode_phase_failure phase=verify cause={:?}",
                    std::mem::discriminant(cause)
                );
                let _ = cause;
            })?;
        drop(self.mode_viewport.borrow_mut().take());
        Ok(false)
    }
}
