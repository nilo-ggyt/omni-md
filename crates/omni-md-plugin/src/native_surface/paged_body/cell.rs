//! Original Cell actions admit bounded intent, return display reads, then consume the same canonical commit authority once.

use super::*;
use crate::native_surface::paged_input::{
    NativeMarkdownPagedCellCommit, NativeMarkdownPagedInputOutcome, NativeMarkdownPagedInputPermit,
    NativeMarkdownPagedInputState,
};
use crate::native_surface::paged_text_view::{
    NativeMarkdownPagedCellAction, NativeMarkdownPagedCellPointer,
    NativeMarkdownPagedPointerLocation,
};
use objc2_app_kit::NSScrollView;
use objc2_foundation::{NSPoint, NSRange, NSRect, NSSize};
use omni_markdown_core::markdown_document_buffer::VerifiedMarkdownTableCellSource;
use omni_markdown_core::{
    MarkdownCellDraftWordAnchor, MarkdownPresentationMode, MarkdownTextSelection,
};

/// Parent Body owns one actual active editing graph. Child recognizes its parent weakly; CellSession alone owns the original draft.
/// Row/raw tickets copy no text and cannot renew successor canonical or SourcePublication authority.
pub(super) struct ActiveCell {
    pub(super) body: Rc<NativeMarkdownPagedBody>,
    pub(super) scroll: Retained<NSScrollView>,
    row: omni_markdown_core::VerifiedMarkdownLayoutTableRow,
    source: VerifiedMarkdownTableCellSource,
    parent_request: NativeMarkdownPagedSceneRequest,
    content_frame: NSRect,
}

/// Retain original click intent across commit only. Old hit row/raw/frame authority ends when canonical state changes;
/// remount obtains actual facts from a new Scene, without retaining old publication or large Source tickets while waiting.
pub(super) struct ReopenPointer {
    head: usize,
    affinity: crate::native_surface::paged_text_view::NativeMarkdownPagedHeadAffinity,
    backward_bias: bool,
    clicks: usize,
    extending: bool,
    anchor: Option<usize>,
    word_anchor: Option<crate::native_surface::paged_text_view::NativeMarkdownPagedWordAnchor>,
    dragging: bool,
    intent: Weak<()>,
    /// Reveal retains initial Down scalars; fast Drag/Up has one latest endpoint, without extending old Hit/Source authority.
    endpoint: RefCell<Option<NativeMarkdownPagedPointerLocation>>,
    cancelled: Cell<bool>,
    /// While original RequiredRow owns Source, a newer Down retains only its request/bounded point, not another Source or queue.
    replacement: RefCell<Option<Box<ReopenDown>>>,
}

struct ReopenDown {
    request: NativeMarkdownPagedSceneRequest,
    location: NativeMarkdownPagedPointerLocation,
}

impl From<NativeMarkdownPagedCellPointer> for ReopenPointer {
    fn from(pointer: NativeMarkdownPagedCellPointer) -> Self {
        Self {
            head: pointer.hit.head_utf16,
            affinity: pointer.hit.head_affinity,
            backward_bias: pointer.hit.backward_bias,
            clicks: pointer.clicks,
            extending: pointer.extending,
            anchor: pointer.anchor,
            word_anchor: pointer.word_anchor,
            dragging: pointer.dragging,
            intent: pointer.intent,
            endpoint: RefCell::new(None),
            cancelled: Cell::new(false),
            replacement: RefCell::new(None),
        }
    }
}

impl ReopenPointer {
    /// Geometry and Source come from the actual current hit; position/affinity remain the same admitted mouse intent.
    fn pointer(
        &self,
        mut hit: crate::native_surface::paged_text_view::NativeMarkdownPagedCellHit,
    ) -> NativeMarkdownPagedCellPointer {
        hit.head_utf16 = self.head;
        hit.head_affinity = self.affinity;
        hit.backward_bias = self.backward_bias;
        NativeMarkdownPagedCellPointer {
            hit,
            clicks: self.clicks,
            extending: self.extending,
            anchor: self.anchor,
            word_anchor: self.word_anchor,
            dragging: self.dragging,
            intent: self.intent.clone(),
        }
    }
}

/// One actual action occupies at most one slot; Pointer waits for its Cell Source while navigation waits for the parent's sole Source request.
/// These are admitted use-case continuations, not per-cell jobs, history caches, or another writable document.
pub(super) enum PendingCell {
    /// Latest actual window endpoint of the original gesture: numeric facts/weak identity only, without events or old Scene/Source.
    ParentPointer {
        request: RefCell<NativeMarkdownPagedSceneRequest>,
        pointer: RefCell<NativeMarkdownPagedPointerLocation>,
        /// Keep initial Down's window point until its first hit; latest Drag cannot replace that initial query input.
        down: RefCell<Option<NativeMarkdownPagedPointerLocation>>,
        waiting_group: Cell<bool>,
        /// Present only when the child's own actual Down crosses bounds; its request/anchor remain child-local.
        child: Option<Weak<ActiveCell>>,
    },
    Pointer {
        owner: Weak<ActiveCell>,
        pointer: NativeMarkdownPagedCellPointer,
        applied: Cell<bool>,
        endpoint: RefCell<Option<NativeMarkdownPagedPointerLocation>>,
    },
    Navigation {
        owner: Weak<ActiveCell>,
        permit: RefCell<Option<NativeMarkdownPagedInputPermit>>,
        action: NativeMarkdownPagedCellAction,
        result: RefCell<Option<semantics::CellNavigationStep>>,
    },
    Reopen {
        state: NativeMarkdownPagedInputState,
        position: usize,
        column: usize,
        next_row: bool,
        destination: RefCell<Option<semantics::CellDestination>>,
        target: RefCell<Option<Rc<target::SourceTargetRequest>>>,
        pointer: Option<ReopenPointer>,
    },
    Closing {
        owner: Weak<ActiveCell>,
        /// Detach child only after actual parent navigation returns; local cancellation does not complete Source work.
        drained: Cell<bool>,
    },
}

impl NativeMarkdownPagedBody {
    /// Preview table insertion enters the first cell through its sole actual receipt; existing Reopen owns Source preparation, remote reveal, and mounting.
    /// Source mode keeps the literal template and its own caret, without automatic client switching or new controls.
    pub(super) fn accept_inserted_table(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
    ) -> Result<()> {
        commit
            .check_current(&|epoch| self.current(epoch))
            .map_err(ViewError::Input)?;
        if self.presentation_mode() != MarkdownPresentationMode::InlinePreview {
            return Ok(());
        }
        if self.pending_cell.borrow().is_some() || self.active_cell.borrow().is_some() {
            // Commit notification may reenter Native; an admitted newer Down/cell action takes precedence over automatic focus.
            // The template is already committed once; neither repeat insertion nor cancel later admitted user intent.
            return Ok(());
        }
        drop(self.selection_reveal.borrow_mut().take());
        self.revoke_source_target(); // Revoke ordinary caret reveal first; this first-cell operation owns one actual positioning intent.
        *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::Reopen {
            state: commit.state,
            position: commit.state.selection.head_utf16,
            column: 0,
            next_row: false,
            destination: RefCell::new(None),
            target: RefCell::new(None),
            pointer: None,
        }));
        // The next original quantum runs RequiredRow/CellInline, then consumes SourceTarget's measured content frame.
        // Keep no Scene/Source strong claim here; the button callback neither parses the table synchronously nor creates a placeholder child.
        self.schedule()
    }

    /// Menus borrow the actual current child owner; weak identity plus InputState, not a view address or empty history, determines the input domain.
    pub(in crate::native_surface) fn active_cell_body(&self) -> Option<Rc<Self>> {
        self.active_cell
            .borrow()
            .as_ref()
            .map(|active| active.body.clone())
    }

    pub(super) fn notify_local_cell_input(&self) {
        // End subscription borrows before notification; local consumers may synchronously close themselves without replacing production history updates.
        let observers = self.input_observers.borrow().clone();
        for observer in observers
            .into_iter()
            .filter_map(|observer| observer.upgrade())
        {
            observer.local_input_changed();
        }
    }

    /// New cells reuse actual parent fonts, catalog, and frozen paragraph facts; final mapping release still depends on every actual claim.
    /// The scroll container spans actual content/viewport intersection only; a huge cell never becomes equally huge Native backing.
    fn mount_cell(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        hit: &crate::native_surface::paged_text_view::NativeMarkdownPagedCellHit,
    ) -> Result<Rc<ActiveCell>> {
        if self.input.is_cell() || self.active_cell.borrow().is_some() {
            return Err(NativeMarkdownPagedBodyError::Busy);
        }
        if self.cell_close.borrow().is_some() {
            return Err(ViewError::Pending.into());
        }
        let left = hit.content_frame.origin.x.max(request.viewport.origin.x);
        let bottom = hit.content_frame.origin.y.max(request.viewport.origin.y);
        let right = (hit.content_frame.origin.x + hit.content_frame.size.width)
            .min(request.viewport.origin.x + request.viewport.size.width);
        let top = (hit.content_frame.origin.y + hit.content_frame.size.height)
            .min(request.viewport.origin.y + request.viewport.size.height);
        if right <= left || top <= bottom {
            return Err(ViewError::Pending.into()); // Top padding has no text intersection and cannot create a placeholder editor.
        }
        // The draft remains a literal Source client with the product's 14-point monospaced role. Parent Preview using
        // a 15-point proportional face cannot authorize blindly inheriting palette zero; generic Single still uses the caller's actual font.
        let scene = self.scene.borrow().clone().ok_or(ViewError::Pending)?;
        if !scene.key().request.same(request) {
            return Err(ViewError::Revoked.into());
        }
        let current = || self.scene_current(scene.key());
        scene.validate_ready(&current)?;
        let captured = self.capture_base_font(MarkdownPresentationMode::Source, &current);
        scene.validate_ready(&current)?; // Preserve original Source failure observed during font callbacks before accepting their result.
        let captured = captured?;
        let authority = captured.core_font().authority();
        let existing = self
            .fonts
            .borrow()
            .iter()
            .find(|font| font.native().core_font().permits(&authority))
            .cloned();
        let font = if let Some(existing) = existing {
            drop(captured);
            existing // Parent already owns this exact instance; reuse it without duplicate Core registration or platform advances.
        } else {
            let registered = NativeMarkdownLineFont::register(captured, &self.catalog, &current);
            scene.validate_ready(&current)?;
            Rc::new(registered?)
        };
        // Child owns this selected font role only, without inheriting both parent families or enabling local mode changes.
        let base_fonts = Rc::new(BaseFonts::Single(font.native().native_font().retain()));
        scene.validate_ready(&current)?;
        drop(scene); // Scene certifies font preparation only; release it before child mounting without retaining parent Source/Reader across that boundary.
        let session = NativeMarkdownPagedCellSession::begin(
            self.input.document_identity(),
            hit.source.clone(),
        )
        .map_err(ViewError::Input)?;
        let frame = NSRect::new(
            NSPoint::new(left, bottom),
            NSSize::new(right - left, top - bottom),
        );
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(self.mtm), frame);
        scroll.setHasVerticalScroller(true);
        scroll.setHasHorizontalScroller(false);
        scroll.setDrawsBackground(true);
        let body = Self::mount_target(
            Attachment::Cell(session),
            &scroll,
            vec![font],
            base_fonts,
            self.catalog.clone(),
            self.paragraph.clone(),
            None,
            NativeMarkdownPagedBodyConfiguration {
                presentation_mode: MarkdownPresentationMode::Source,
                epoch: self.configuration.epoch,
                maximum_font_bytes: self.configuration.maximum_font_bytes,
                padding: 0.0,
                colors: self.configuration.colors,
            },
        )?;
        *body.cell_parent.borrow_mut() = self.weak.clone();
        let active = Rc::new(ActiveCell {
            body,
            scroll,
            row: hit.row.clone(),
            source: hit.source.clone(),
            parent_request: request.clone(),
            content_frame: hit.content_frame,
        });
        // Install the sole owner before reentrant Native calls; failure detaches that exact owner without overriding a successor cell.
        *self.active_cell.borrow_mut() = Some(active.clone());
        let mounted = (|| {
            let parent = self.view()?;
            parent.addSubview(&active.scroll);
            let child = active.body.view()?;
            let window = parent.window().ok_or(ViewError::Pending)?;
            if !self.owns_cell(&active) || !window.makeFirstResponder(Some(&**child)) {
                return Err(ViewError::Revoked.into());
            }
            Ok(())
        })();
        if let Err(cause) = mounted {
            let cleanup = self.detach_cell(&active).map(|_| ());
            return producer::merge_preparation_cleanup(Err(cause), cleanup).map(|_| active);
        }
        self.notify_local_cell_input();
        Ok(active)
    }

    fn owns_cell(&self, active: &Rc<ActiveCell>) -> bool {
        self.active_cell
            .borrow()
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, active))
    }

    /// First hand the actual raw click position to the same Cell Input; original selection reveal then prepares remote glyphs.
    /// Page zero becoming Ready cannot certify visibility in the middle of a huge cell.
    fn seed_cell_pointer(
        &self,
        active: &Rc<ActiveCell>,
        pointer: &NativeMarkdownPagedCellPointer,
    ) -> Result<()> {
        let document = self
            .document
            .borrow()
            .as_ref()
            .expect("格只挂原正文")
            .clone();
        let range = active
            .source
            .canonical_range_for(&document.borrow())?
            .clone();
        let head = pointer
            .hit
            .head_utf16
            .checked_sub(range.start)
            .filter(|head| *head <= range.end - range.start)
            .ok_or(ViewError::InvalidRange)?;
        let child = &active.body;
        let current = |epoch| child.current(epoch) && self.owns_cell(active);
        let permit = child
            .input
            .capture(child.configuration.epoch, &current)
            .map_err(ViewError::Input)?;
        let outcome = child
            .input
            .set_selection_intent(
                &permit,
                MarkdownTextSelection::caret(head),
                Some(pointer.hit.head_affinity.is_upstream()),
                None,
                &current,
            )
            .map_err(ViewError::Input)?;
        if let NativeMarkdownPagedInputOutcome::Committed(commit) = outcome {
            child.view()?.finish_word_delete_commit(&commit)?;
        } else {
            child.reveal_current_selection()?;
        }
        Ok(())
    }

    pub(super) fn accept_cell_pointer(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        pointer: NativeMarkdownPagedCellPointer,
        permit: NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        // Only parent InlinePreview owns cell activation. Other clients retain
        // their existing pointer handling without acquiring canonical cell authority.
        if self.input.is_cell()
            || self.presentation_mode() != MarkdownPresentationMode::InlinePreview
        {
            return Ok(false);
        }
        self.input
            .check_permit(&permit, &|epoch| self.current(epoch))?;
        if self.input.state()? != request.input {
            return Err(ViewError::Revoked);
        }
        if self
            .pending_cell
            .borrow()
            .as_ref()
            .is_some_and(|pending| match &**pending {
                PendingCell::Pointer { .. } => false,
                PendingCell::ParentPointer {
                    request: original,
                    pointer: original_pointer,
                    ..
                } => {
                    !original.borrow().same(request)
                        || !Weak::ptr_eq(&original_pointer.borrow().intent, &pointer.intent)
                }
                _ => true,
            })
        {
            return Err(ViewError::Pending);
        }
        let document = self
            .document
            .borrow()
            .as_ref()
            .expect("格指针属于正文")
            .clone();
        let target_range = pointer
            .hit
            .source
            .canonical_range_for(&document.borrow())
            .map_err(|cause| cell_view_error(cause.into()))?
            .clone();
        let (_, _, column) = pointer
            .hit
            .source
            .coordinate_for(&document.borrow())
            .map_err(|cause| cell_view_error(cause.into()))?;
        let active = self.active_cell.borrow().clone();
        if let Some(active) = active {
            // Compare actual canonical range and table coordinates under the same
            // document. Equal text or a visible-column index cannot identify this cell.
            let same = active
                .source
                .canonical_range_for(&document.borrow())
                .map_err(|cause| cell_view_error(cause.into()))?
                == &target_range
                && active
                    .source
                    .coordinate_for(&document.borrow())
                    .map_err(|cause| cell_view_error(cause.into()))?
                    == pointer
                        .hit
                        .source
                        .coordinate_for(&document.borrow())
                        .map_err(|cause| cell_view_error(cause.into()))?;
            // An admitted parent Word endpoint remains owned by that parent gesture, even when target and first group share the same raw cell.
            // After first-group Ready, publish canonical extension; routing the same Up back to child would loop.
            // Child-owned Down uses local word selection outside this parent ParentPointer slot, preserving its editing/history role.
            let parent_word_endpoint = pointer.dragging
                && pointer.clicks == 2
                && matches!(
                    pointer.word_anchor,
                    Some(
                        crate::native_surface::paged_text_view::NativeMarkdownPagedWordAnchor::Group { .. }
                            | crate::native_surface::paged_text_view::NativeMarkdownPagedWordAnchor::Selection(_)
                    )
                )
                && self.pending_cell.borrow().as_ref().is_some_and(|pending| {
                    matches!(&**pending, PendingCell::ParentPointer {
                        request: original,
                        pointer: endpoint,
                        child: None,
                        ..
                    } if original.borrow().same(request)
                        && Weak::ptr_eq(&endpoint.borrow().intent, &pointer.intent)
                        && (endpoint.borrow().dragging || endpoint.borrow().mouse_up))
                });
            if !same || parent_word_endpoint {
                if pointer.extending {
                    use crate::native_surface::paged_text_view::NativeMarkdownPagedCellExtension as Extension;
                    // Cross-cell selection preserves initial click granularity; double-click word dragging cannot degrade to character-caret extension.
                    let extension = if pointer.clicks == 2 {
                        Extension::Word {
                            selection: crate::native_surface::paged_text_view::NativeMarkdownPagedWordSelection {
                                head_utf16: pointer.hit.head_utf16,
                                backward_bias: pointer.hit.backward_bias,
                                anchor: pointer.word_anchor,
                                cell: pointer.hit.word_target,
                            },
                            intent: pointer.intent.clone(),
                        }
                    } else {
                        Extension::Selection {
                            selection: MarkdownTextSelection {
                                anchor_utf16: pointer
                                    .anchor
                                    .unwrap_or(request.input.selection.anchor_utf16),
                                head_utf16: pointer.hit.head_utf16,
                            },
                            head_affinity: pointer.hit.head_affinity,
                            intent: pointer.intent.clone(),
                        }
                    };
                    return self.extend_cell_selection(request, extension, permit);
                }
                let position = pointer.hit.row.source_range().start;
                let committed = self
                    .commit_owned_cell(&active, None, None, None, None, None)
                    .map_err(cell_view_error)?;
                let pointer =
                    map_pointer(pointer.into(), &committed.outcome).map_err(cell_view_error)?;
                let position =
                    map_position(position, &committed.outcome).map_err(cell_view_error)?;
                let state = self.input.state()?;
                *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::Reopen {
                    state,
                    position,
                    column,
                    next_row: false,
                    destination: RefCell::new(None),
                    target: RefCell::new(None),
                    pointer: Some(pointer),
                }));
                self.schedule().map_err(cell_view_error)?;
                return Ok(true);
            }
            *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::Pointer {
                owner: Rc::downgrade(&active),
                pointer,
                applied: Cell::new(false),
                endpoint: RefCell::new(None),
            }));
        } else {
            // Cross-cell Shift keeps an actual Body anchor in Body selection, never forcing an external raw position into local Input.
            if pointer.extending
                && pointer
                    .anchor
                    .is_some_and(|anchor| anchor < target_range.start || anchor > target_range.end)
            {
                return Ok(false);
            }
            let active = match self.mount_cell(request, &pointer.hit) {
                Ok(active) => active,
                Err(NativeMarkdownPagedBodyError::View(ViewError::Pending)) => {
                    // Top padding, space below completed short text, or an unreleased prior pixel receipt cannot lose the original Down.
                    // Retain actual row/column query intent; the same Request/SourceTarget obtains a visible content frame before mounting.
                    *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::Reopen {
                        state: request.input,
                        position: pointer.hit.row.source_range().start,
                        column,
                        next_row: false,
                        destination: RefCell::new(None),
                        target: RefCell::new(None),
                        pointer: Some(pointer.into()),
                    }));
                    self.schedule().map_err(cell_view_error)?;
                    return Ok(true);
                }
                Err(cause) => return Err(cell_view_error(cause)),
            };
            self.seed_cell_pointer(&active, &pointer)
                .map_err(cell_view_error)?;
            *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::Pointer {
                owner: Rc::downgrade(&active),
                pointer,
                applied: Cell::new(false),
                endpoint: RefCell::new(None),
            }));
        }
        self.finish_cell_work().map_err(cell_view_error)?;
        Ok(true)
    }

    /// After actual parent/child Ready, resume admitted scalar intent only. Early Up may finish endpoint selection without reviving drag lifetime.
    pub(super) fn finish_cell_work(&self) -> Result<()> {
        if self.input.is_cell() {
            let pending = self.pending_cell.borrow().clone();
            if let Some(pending) = pending
                && matches!(&*pending, PendingCell::ParentPointer { .. })
            {
                self.finish_deferred_cell_pointer(&pending)?;
            } // Child Source may await its own Scene; cancellation stays in this domain rather than cancelling a gesture already handed to parent.
            if let Some(parent) = self.cell_parent.borrow().upgrade() {
                parent.finish_cell_work()?;
            }
            return Ok(());
        }
        let pending = self.pending_cell.borrow().clone();
        let Some(pending) = pending else {
            return self.reposition_cell();
        };
        match &*pending {
            PendingCell::ParentPointer { .. } => {
                self.finish_deferred_cell_pointer(&pending)?;
            }
            PendingCell::Pointer {
                owner,
                pointer,
                applied,
                endpoint,
            } => {
                let Some(active) = owner.upgrade().filter(|active| self.owns_cell(active)) else {
                    self.clear_cell_pending(&pending);
                    return Ok(());
                };
                if active.body.preparing.get() || applied.get() {
                    return Ok(()); // The first query was admitted; await its actual Group without resending initial Down.
                }
                let document = self.document.borrow().as_ref().expect("实际父Doc").clone();
                let range = active
                    .source
                    .canonical_range_for(&document.borrow())?
                    .clone();
                match active
                    .body
                    .view()?
                    .apply_cell_pointer(pointer, range.clone())
                {
                    Ok(true) => {
                        if pointer.clicks == 2 {
                            applied.set(true);
                        } else {
                            // End the endpoint mutable borrow before reentrant Native geometry reads; callbacks may update this same gesture.
                            let latest = endpoint.borrow_mut().take();
                            if let Some(pointer) = latest {
                                let request = self.view()?.capture_scene_request()?;
                                *self.pending_cell.borrow_mut() =
                                    Some(Rc::new(PendingCell::ParentPointer {
                                        request: RefCell::new(request),
                                        pointer: RefCell::new(pointer),
                                        down: RefCell::new(None),
                                        waiting_group: Cell::new(false),
                                        child: None,
                                    }));
                                self.schedule()?;
                            } else {
                                self.clear_cell_pending(&pending);
                            }
                        }
                    }
                    Ok(false) | Err(ViewError::Pending | ViewError::Busy) => {}
                    Err(cause) => return Err(cause.into()),
                }
            }
            PendingCell::Navigation {
                owner,
                action,
                result,
                ..
            } => {
                let Some(result) = result.borrow_mut().take() else {
                    return Ok(());
                };
                let Some(active) = owner.upgrade().filter(|active| self.owns_cell(active)) else {
                    self.clear_cell_pending(&pending);
                    return Ok(());
                };
                self.finish_cell_navigation(&pending, &active, *action, result)?;
            }
            PendingCell::Reopen {
                state,
                destination,
                target,
                pointer,
                ..
            } => {
                if pointer
                    .as_ref()
                    .is_some_and(|pointer| pointer.cancelled.get())
                {
                    return Ok(()); // Cancelled clicks retain Source-drain obligations only; old Ready cannot reopen their editor.
                }
                if self.cell_close.borrow().is_some() {
                    return Ok(());
                }
                if self.input.state().map_err(ViewError::Input)? != *state {
                    self.clear_cell_pending(&pending);
                    return Ok(());
                }
                let original_target = target.borrow().clone();
                let ready = original_target
                    .as_ref()
                    .is_some_and(|target| !target.pending());
                let empty = match &original_target {
                    Some(target) => self.reopening_empty_cell_target(target)?,
                    None => false,
                };
                if !ready && !empty {
                    return Ok(());
                }
                let scene = self.scene.borrow().clone();
                let Some(scene) = scene else {
                    return Ok(());
                };
                let hit = {
                    let destination = destination.borrow();
                    let Some(destination) = destination.as_ref() else {
                        return Ok(());
                    };
                    scene.cell_source_hit(&destination.source)?
                };
                let Some(hit) = hit else {
                    return Ok(());
                };
                let active = self.mount_cell(&scene.key().request, &hit)?;
                if empty && let Some(target) = original_target {
                    // An empty cell consumes only the actual visible content frame just checked against the same Doc, coordinates, and cell kind.
                    // After successful mounting retire this exact target, without inventing glyph/Find proof absent from the parent graph.
                    // Child Source must still prepare its actual empty line, caret, and input protocol.
                    target.revoke();
                    let mut slot = self.source_target.borrow_mut();
                    if slot
                        .as_ref()
                        .is_some_and(|actual| Rc::ptr_eq(actual, &target))
                    {
                        slot.take();
                    }
                }
                if let Some(pointer) = pointer {
                    let down = pointer.pointer(hit);
                    self.seed_cell_pointer(&active, &down)?;
                    // Seeding sends Native notifications; a reentrant newer Down may revoke this slot, so do not overwrite its successor or revive old dragging.
                    let original = self
                        .pending_cell
                        .borrow()
                        .as_ref()
                        .is_some_and(|actual| Rc::ptr_eq(actual, &pending));
                    if !original || pointer.cancelled.get() {
                        return Ok(());
                    }
                    let endpoint = pointer.endpoint.borrow_mut().take();
                    *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::Pointer {
                        owner: Rc::downgrade(&active),
                        pointer: down,
                        applied: Cell::new(false),
                        endpoint: RefCell::new(endpoint),
                    }));
                } else {
                    self.clear_cell_pending(&pending);
                    active.body.view()?.command(objc2::sel!(selectAll:))?; // Existing table keyboard navigation selects the target cell's raw content; no shortcut is added.
                }
            }
            PendingCell::Closing { owner, drained } => {
                if !drained.get() {
                    return Ok(()); // RequiredRow/CellInline still owns the request; the next original quantum only drains that work.
                }
                if let Some(active) = owner.upgrade().filter(|active| self.owns_cell(active)) {
                    if active.body.preflight_release().is_err() {
                        return Ok(());
                    }
                    self.detach_cell(&active)?;
                    self.focus_cell_parent()?;
                }
                self.clear_cell_pending(&pending);
            }
        }
        self.reposition_cell()
    }

    fn clear_cell_pending(&self, pending: &Rc<PendingCell>) {
        // A late completion may observe a newer slot. Remove only its own Rc so
        // original cleanup cannot cancel an independent successor Down/navigation.
        let mut slot = self.pending_cell.borrow_mut();
        if slot
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, pending))
        {
            slot.take();
        }
    }

    /// Decoded endpoints enter this gesture's sole slot; original Down and latest endpoint have separate duties and cannot replace each other.
    pub(super) fn defer_cell_pointer(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        pointer: NativeMarkdownPagedPointerLocation,
        permit: NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        self.input
            .check_permit(&permit, &|epoch| self.current(epoch))?;
        let view = self.view().map_err(cell_view_error)?;
        if !view.cell_pointer_is_current(&pointer.intent)
            || !view.capture_scene_request()?.same(request)
            || !view.cell_pointer_is_current(&pointer.intent)
        {
            return Err(ViewError::Revoked);
        }
        self.input
            .check_permit(&permit, &|epoch| self.current(epoch))?;
        // Same-point Up needs no new hit, but initial Select may still own the original Word producer.
        // Retain the endpoint until its actual group returns; premature no-op removal would prevent newer Down from revoking the old query.
        let waiting_word = self.has_pending_pointer_word(&pointer.intent);
        let occupied = self.pending_cell.borrow().clone();
        if let Some(pending) = occupied {
            match &*pending {
                PendingCell::Pointer {
                    pointer: down,
                    endpoint,
                    ..
                } if Weak::ptr_eq(&down.intent, &pointer.intent) => {
                    if endpoint.borrow().as_ref().is_some_and(|end| end.mouse_up) {
                        return Err(ViewError::Revoked);
                    }
                    *endpoint.borrow_mut() = Some(pointer);
                    self.schedule().map_err(cell_view_error)?;
                    return Ok(true); // While the child initial query runs, update endpoint only; raw Source/initial click remain with original Down.
                }
                PendingCell::ParentPointer {
                    request: original,
                    pointer: endpoint,
                    waiting_group,
                    child: None,
                    ..
                } if original.borrow().same(request)
                    && Weak::ptr_eq(&endpoint.borrow().intent, &pointer.intent)
                    && !endpoint.borrow().mouse_up =>
                {
                    *endpoint.borrow_mut() = pointer;
                    if waiting_word {
                        waiting_group.set(true);
                    }
                    self.schedule().map_err(cell_view_error)?;
                    return Ok(true);
                }
                PendingCell::Reopen {
                    pointer: Some(reopen),
                    state,
                    ..
                } => {
                    if reopen.cancelled.get() {
                        if pointer.dragging || pointer.mouse_up {
                            let original =
                                reopen.replacement.borrow().as_ref().is_some_and(|down| {
                                    down.request.same(request)
                                        && Weak::ptr_eq(&down.location.intent, &pointer.intent)
                                });
                            if !original
                                || reopen
                                    .endpoint
                                    .borrow()
                                    .as_ref()
                                    .is_some_and(|end| end.mouse_up)
                            {
                                return Err(ViewError::Revoked);
                            }
                            *reopen.endpoint.borrow_mut() = Some(pointer);
                        } else {
                            *reopen.replacement.borrow_mut() = Some(Box::new(ReopenDown {
                                request: request.clone(),
                                location: pointer,
                            }));
                            reopen.endpoint.borrow_mut().take();
                        }
                    } else {
                        if request.input != *state
                            || !Weak::ptr_eq(&reopen.intent, &pointer.intent)
                            || reopen
                                .endpoint
                                .borrow()
                                .as_ref()
                                .is_some_and(|end| end.mouse_up)
                        {
                            return Err(ViewError::Revoked);
                        }
                        *reopen.endpoint.borrow_mut() = Some(pointer);
                    }
                    self.schedule().map_err(cell_view_error)?;
                    return Ok(true); // Keep one actual first Down and latest endpoint in this slot while original Source work returns first.
                }
                _ => return Err(ViewError::Pending),
            }
        }
        let down = (!pointer.dragging && !pointer.mouse_up).then(|| pointer.clone());
        let pending = Rc::new(PendingCell::ParentPointer {
            request: RefCell::new(request.clone()),
            pointer: RefCell::new(pointer),
            down: RefCell::new(down),
            waiting_group: Cell::new(waiting_word),
            child: None,
        });
        *self.pending_cell.borrow_mut() = Some(pending.clone());
        if let Err(cause) = self.schedule() {
            self.clear_cell_pending(&pending);
            return Err(cell_view_error(cause));
        }
        Ok(true)
    }

    /// New Down/input revokes the old endpoint; cancel only pure selection of that weak identity, preserving admitted text/navigation obligations.
    pub(super) fn cancel_deferred_cell_pointer(&self) -> bool {
        let pending = self.pending_cell.borrow().clone();
        let Some(pending) = pending else {
            if self.input.is_cell()
                && let Some(parent) = self.cell_parent.borrow().upgrade()
            {
                parent.cancel_child_pointer(self);
            }
            return false;
        };
        let (intent, child) = match &*pending {
            PendingCell::Reopen {
                pointer: Some(pointer),
                ..
            } => {
                // Started Source work must actually return; revoke old pointer intent here without treating cancellation as release completion.
                pointer.cancelled.set(true);
                pointer.endpoint.borrow_mut().take();
                pointer.replacement.borrow_mut().take();
                drop(self.selection_reveal.borrow_mut().take());
                self.revoke_source_target(); // New input revokes old positioning; original quanta below still drain actual Source work.
                if let Err(cause) = self.schedule() {
                    self.fail(cause);
                }
                return true;
            }
            PendingCell::Pointer { pointer, owner, .. } => {
                (pointer.intent.clone(), owner.upgrade())
            }
            PendingCell::ParentPointer { pointer, child, .. } => (
                pointer.borrow().intent.clone(),
                child.as_ref().and_then(Weak::upgrade),
            ),
            _ => return false,
        };
        self.clear_cell_pending(&pending);
        let result = if let Some(child) = child {
            child.body.cancel_pointer_word(&intent)
        } else {
            self.cancel_pointer_word(&intent)
        };
        if let Err(cause) = result {
            self.fail(cause);
        }
        if self.input.is_cell()
            && let Some(parent) = self.cell_parent.borrow().upgrade()
        {
            parent.cancel_child_pointer(self);
        }
        false
    }

    fn cancel_child_pointer(&self, child: &Self) {
        let pending = self.pending_cell.borrow().clone();
        let Some(pending) = pending else {
            return;
        };
        let owner = match &*pending {
            PendingCell::Pointer { owner, .. } => owner.upgrade(),
            PendingCell::ParentPointer {
                child: Some(owner), ..
            } => owner.upgrade(),
            _ => None,
        };
        if owner.is_some_and(|owner| Weak::ptr_eq(&owner.body.weak, &child.weak)) {
            self.cancel_deferred_cell_pointer();
        }
    }

    /// When original Word querying ends after actual Input/geometry revocation, clear that gesture's waiting-group fact rather than awaiting impossible Ready.
    /// Clear only this admitted query's metadata; original owners still return active cells, Reopen, and in-flight RequiredRow/CellInline.
    pub(super) fn cancel_waiting_pointer_word(&self, intent: &Weak<()>) {
        let pending = self.pending_cell.borrow().clone();
        if let Some(pending) = pending
            && let PendingCell::ParentPointer {
                pointer,
                waiting_group,
                child: None,
                ..
            } = &*pending
            && waiting_group.get()
            && Weak::ptr_eq(&pointer.borrow().intent, intent)
        {
            self.clear_cell_pending(&pending);
        }
        if !self.input.is_cell() {
            return;
        }
        let Some(parent) = self.cell_parent.borrow().upgrade() else {
            return;
        };
        let pending = parent.pending_cell.borrow().clone();
        let Some(pending) = pending else {
            return;
        };
        let owner = match &*pending {
            PendingCell::Pointer {
                owner,
                pointer,
                applied,
                ..
            } if applied.get() && Weak::ptr_eq(&pointer.intent, intent) => owner.upgrade(),
            PendingCell::ParentPointer {
                pointer,
                waiting_group,
                child: Some(owner),
                ..
            } if waiting_group.get() && Weak::ptr_eq(&pointer.borrow().intent, intent) => {
                owner.upgrade()
            }
            _ => None,
        };
        if owner.is_some_and(|owner| {
            parent.owns_cell(&owner) && Weak::ptr_eq(&owner.body.weak, &self.weak)
        }) {
            parent.clear_cell_pending(&pending);
        } // Successor Down owns a distinct weak identity even with equal selection; old-query cleanup cannot remove it.
    }

    /// After actual frame publication consume admitted intent once; child-local endpoints must follow parent/child commit before affecting parent Input.
    fn finish_deferred_cell_pointer(&self, pending: &Rc<PendingCell>) -> Result<()> {
        let PendingCell::ParentPointer {
            request,
            pointer,
            down,
            waiting_group,
            child,
        } = &**pending
        else {
            unreachable!("只推进原指针续体");
        };
        let request = request.borrow().clone();
        let pointer = pointer.borrow().clone();
        if let Some(child) = child {
            let Some(active) = child.upgrade().filter(|active| self.owns_cell(active)) else {
                self.clear_cell_pending(pending);
                return Ok(());
            };
            if waiting_group.get() || active.body.preparing.get() || active.body.has_pending_input()
            {
                return Ok(()); // Actual first-group/text work still owns child; its completion notification resumes work without polling or synthetic Ready.
            }
            let view = active.body.view()?;
            if !view.capture_scene_request()?.same(&request) {
                self.clear_cell_pending(pending);
                return Ok(());
            }
            let parent_request = self.view()?.capture_scene_request()?;
            match self.ready_scene(&parent_request) {
                Ok(_) => {}
                Err(ViewError::Pending) => return Ok(()),
                Err(cause) => return Err(cause.into()),
            }
            let permit = active
                .body
                .input
                .capture(active.body.configuration.epoch, &|epoch| {
                    active.body.current(epoch) && self.owns_cell(&active)
                })
                .map_err(ViewError::Input)?;
            self.apply_cell_drag(&active.body, &request, pointer, permit)?;
            self.clear_cell_pending(pending);
            return Ok(());
        }
        if waiting_group.get() {
            return Ok(());
        }
        if self
            .active_cell
            .borrow()
            .as_ref()
            .is_some_and(|active| active.body.preparing.get() || active.body.has_pending_input())
        {
            // First-group handoff precedes child display notification/head release; the original completion exit returns here again.
            // The admitted parent endpoint awaits that same actual input; intermediate Busy/Pending is not fatal Body failure.
            return Ok(());
        }
        let initial = down.borrow().clone();
        let actual = initial.as_ref().unwrap_or(&pointer);
        // An admitted Up does not cancel initial Down's first hit; preserve Down's role without renewing drag authority.
        let accepted_up = (initial.is_some() && pointer.mouse_up).then_some(&pointer);
        let result = self
            .view()?
            .apply_deferred_pointer(&request, actual, accepted_up);
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?;
        }
        match result {
            Ok(true) => {
                let successor = self.pending_cell.borrow().clone();
                if let Some(successor) = successor
                    && !Rc::ptr_eq(&successor, pending)
                {
                    // When initial Down obtains its actual Grid hit, original ownership may transition this same slot into Pointer.
                    // Early Drag/Up still belongs to this Down; hand it to the new stage instead of losing its endpoint when the old Rc leaves.
                    if let PendingCell::Pointer {
                        pointer: original,
                        endpoint,
                        ..
                    } = &*successor
                        && Weak::ptr_eq(&original.intent, &pointer.intent)
                        && (pointer.dragging || pointer.mouse_up)
                    {
                        *endpoint.borrow_mut() = Some(pointer.clone());
                    }
                    if let PendingCell::Reopen {
                        pointer: Some(reopen),
                        ..
                    } = &*successor
                        && Weak::ptr_eq(&reopen.intent, &pointer.intent)
                        && (pointer.dragging || pointer.mouse_up)
                    {
                        *reopen.endpoint.borrow_mut() = Some(pointer.clone());
                    } // Reveal also inherits this admitted endpoint; absent current text intersection cannot discard original Up.
                    return Ok(());
                }
                if initial.as_ref().is_some_and(|down| down.clicks == 2) {
                    down.borrow_mut().take();
                    waiting_group.set(true); // The initial query is admitted; never submit that Down a second time.
                } else {
                    self.clear_cell_pending(pending);
                }
            }
            Ok(false) => {}
            Err(ViewError::Revoked) => self.clear_cell_pending(pending),
            Err(cause) => return Err(cause.into()),
        }
        Ok(())
    }

    /// Only an actual empty raw cell bound to this Reopen continuation lacks parent glyph evidence.
    /// Find/mode targets do not own this slot and cannot use this condition to obtain empty character-visibility proof.
    pub(super) fn reopening_empty_cell_target(
        &self,
        original: &Rc<target::SourceTargetRequest>,
    ) -> Result<bool> {
        let pending = self.pending_cell.borrow().clone();
        let Some(pending) = pending else {
            return Ok(false);
        };
        let PendingCell::Reopen {
            destination,
            target,
            ..
        } = &*pending
        else {
            return Ok(false);
        };
        if !target
            .borrow()
            .as_ref()
            .is_some_and(|target| Rc::ptr_eq(target, original))
        {
            return Ok(false);
        }
        let destination = destination.borrow();
        let Some(destination) = destination.as_ref() else {
            return Ok(false);
        };
        let document = self
            .document
            .borrow()
            .as_ref()
            .expect("重挂仍归原文档")
            .clone();
        let document = document.try_borrow().map_err(|_| ViewError::Busy)?;
        Ok(destination
            .source
            .canonical_range_for(&document)?
            .is_empty())
    }

    /// Original parent quanta handle actual navigation; Busy lets existing Font/CellInline continue, while Pending creates no second semantic advance.
    pub(super) fn advance_pending_cell(
        &self,
        producer: &mut Producer,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<bool> {
        let pending = self.pending_cell.borrow().clone();
        let Some(pending) = pending else {
            return Ok(false);
        };
        match &*pending {
            PendingCell::Navigation {
                owner,
                action,
                result,
                ..
            } => {
                if result.borrow().is_some() {
                    return Ok(true);
                }
                let Some(active) = owner.upgrade().filter(|active| self.owns_cell(active)) else {
                    self.clear_cell_pending(&pending);
                    return Ok(false);
                };
                let document = self
                    .document
                    .borrow()
                    .as_ref()
                    .expect("原格导航文档")
                    .clone();
                let (_, _, column) = active.source.coordinate_for(&document.borrow())?;
                let step = producer.navigate_cell(self, &active.row, column, *action)?;
                match step {
                    semantics::CellNavigationStep::Occupied => return Ok(false),
                    semantics::CellNavigationStep::Pending => self.schedule()?,
                    step => *result.borrow_mut() = Some(step),
                }
                Ok(true)
            }
            PendingCell::Reopen {
                state,
                position,
                column,
                next_row,
                destination,
                target,
                pointer,
            } => {
                if let Some(pointer) = pointer
                    && pointer.cancelled.get()
                {
                    // Drain actual started request work and consume its result; create no replacement Source or flag-based completion.
                    if !producer.drain_cell_navigation(self)? {
                        self.schedule()?;
                        return Ok(true);
                    }
                    let original_target = target.borrow_mut().take();
                    if let Some(original) = original_target {
                        original.revoke();
                        let mut slot = self.source_target.borrow_mut();
                        if slot
                            .as_ref()
                            .is_some_and(|actual| Rc::ptr_eq(actual, &original))
                        {
                            slot.take();
                        }
                    }
                    let replacement = pointer.replacement.borrow_mut().take();
                    let endpoint = pointer.endpoint.borrow_mut().take();
                    if let Some(replacement) = replacement
                        && replacement.request.same(request)
                    {
                        // Retain this latest Down/endpoint in the same slot. After drain, resume only if original geometry still matches;
                        // external editing/resize cannot renew old window points through the current request.
                        let end = endpoint.unwrap_or_else(|| replacement.location.clone());
                        *self.pending_cell.borrow_mut() =
                            Some(Rc::new(PendingCell::ParentPointer {
                                request: RefCell::new(replacement.request),
                                pointer: RefCell::new(end),
                                down: RefCell::new(Some(replacement.location)),
                                waiting_group: Cell::new(false),
                                child: None,
                            }));
                    } else {
                        self.clear_cell_pending(&pending);
                    }
                    self.schedule()?;
                    return Ok(true); // This turn returns old Source only; new Down starts next quantum without combining budgets.
                }
                if request.input != *state {
                    self.clear_cell_pending(&pending);
                    return Ok(false);
                }
                if destination.borrow().is_none() {
                    match producer.activate_cell(self, *position, *column, *next_row)? {
                        semantics::CellNavigationStep::Occupied => return Ok(false),
                        semantics::CellNavigationStep::Pending => {
                            self.schedule()?;
                            return Ok(true);
                        }
                        semantics::CellNavigationStep::Ready(ready) => {
                            *destination.borrow_mut() = Some(ready)
                        }
                        _ => return Err(ViewError::Revoked.into()),
                    }
                }
                if target.borrow().is_none() {
                    let document = self
                        .document
                        .borrow()
                        .as_ref()
                        .expect("新格来源仍归同一Doc")
                        .clone();
                    let start = destination
                        .borrow()
                        .as_ref()
                        .expect("真实格已Ready")
                        .source
                        .canonical_range_for(&document.borrow())?
                        .start;
                    let position = pointer.as_ref().map_or(start, |pointer| pointer.head);
                    let source = producer.target_source_authority(request, self.serial.get())?;
                    let desired = self.admit_source_target(
                        request,
                        source,
                        NSRange::new(position, 0),
                        &|| {
                            self.pending_cell
                                .borrow()
                                .as_ref()
                                .is_some_and(|actual| Rc::ptr_eq(actual, &pending))
                        },
                    )?;
                    self.install_source_target(desired.clone())?;
                    *target.borrow_mut() = Some(desired);
                }
                Ok(false) // Actual row/glyph positioning continues through the same Producer; mount new Input only after Ready.
            }
            PendingCell::Closing { drained, .. } => {
                if drained.get() {
                    return Ok(false);
                }
                if producer.drain_cell_navigation(self)? {
                    drained.set(true);
                } else {
                    self.schedule()?;
                }
                Ok(true) // This turn drains original work only, without starting another cell or letting ordinary layout seize its Source slot.
            }
            PendingCell::Pointer { .. } | PendingCell::ParentPointer { .. } => Ok(false),
        }
    }

    pub(super) fn accept_cell_action(
        &self,
        child: &NativeMarkdownPagedBody,
        action: NativeMarkdownPagedCellAction,
        permit: NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<(), ViewError> {
        let active = self
            .active_cell
            .borrow()
            .clone()
            .ok_or(ViewError::Revoked)?;
        if !Weak::ptr_eq(&active.body.weak, &child.weak) {
            return Err(ViewError::Revoked);
        }
        child
            .input
            .check_permit(&permit, &|epoch| child.current(epoch))?;
        if action == NativeMarkdownPagedCellAction::Cancel {
            child.cancel_cell(permit)?;
            // Replace intent only after actual draft cancellation; Busy/revocation rejection preserves original navigation/focus.
            // RootLeaf keeps the same in-flight request; Closing drains only, admitting no next cell or appended row.
            *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::Closing {
                owner: Rc::downgrade(&active),
                drained: Cell::new(false),
            }));
            return self.schedule().map_err(cell_view_error); // Detach Native child after the input stack returns; later Busy does not mean draft cancellation never happened.
        }
        if self.pending_cell.borrow().is_some() || child.has_pending_input() {
            return Err(ViewError::Pending);
        }
        *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::Navigation {
            owner: Rc::downgrade(&active),
            permit: RefCell::new(Some(permit)),
            action,
            result: RefCell::new(None),
        }));
        self.schedule().map_err(cell_view_error)
    }

    fn finish_cell_navigation(
        &self,
        pending: &Rc<PendingCell>,
        active: &Rc<ActiveCell>,
        action: NativeMarkdownPagedCellAction,
        step: semantics::CellNavigationStep,
    ) -> Result<()> {
        // Actual RequiredRow/CellInline results determine the next cell. Only
        // Next at certified TableEnd may append; Boundary/other End leaves the
        // original draft and its local history editable.
        let (position, column, terminal, next_row) = match step {
            semantics::CellNavigationStep::Ready(destination) => {
                let document = self.document.borrow().as_ref().expect("原导航Doc").clone();
                let (_, _, column) = destination.source.coordinate_for(&document.borrow())?;
                (destination.row.source_range().start, column, None, false)
            }
            semantics::CellNavigationStep::End {
                terminal,
                authority,
            } if action == NativeMarkdownPagedCellAction::Next => (
                active.row.source_range().start,
                0,
                Some((terminal, authority)),
                true,
            ),
            semantics::CellNavigationStep::End { .. } | semantics::CellNavigationStep::Boundary => {
                self.clear_cell_pending(pending);
                return Ok(()); // Enter without a next row or reverse movement at the first cell preserves original draft/local history.
            }
            _ => return Err(ViewError::Revoked.into()),
        };
        let PendingCell::Navigation { permit, .. } = &**pending else {
            unreachable!("原导航结果只交原导航槽");
        };
        let permit = permit.borrow_mut().take().expect("原输入票只消费一次");
        active
            .body
            .input
            .check_permit(&permit, &|epoch| active.body.current(epoch))
            .map_err(ViewError::Input)?;
        let append = terminal
            .as_ref()
            .map(|(terminal, authority)| (terminal, *authority));
        let committed =
            match self.commit_owned_cell(active, append, Some(&permit), None, None, None) {
                Ok(committed) => committed,
                Err(cause) => {
                    self.clear_cell_pending(pending);
                    crate::native_surface::present_native_markdown_error(&cause.to_string());
                    return Ok(()); // Pre-publication draft rejection leaves Body active and draft/focus with the same owner.
                }
            };
        let position = map_position(position, &committed.outcome)?;
        let state = self.input.state().map_err(ViewError::Input)?;
        *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::Reopen {
            state,
            position,
            column,
            next_row,
            destination: RefCell::new(None),
            target: RefCell::new(None),
            pointer: None,
        }));
        self.focus_cell_parent()?;
        self.schedule()
    }

    fn focus_cell_parent(&self) -> Result<()> {
        // Focus the existing parent View only; a rejected Native resignation is
        // revocation, never a reason to recreate a window or replay cell commit.
        let view = self.view()?;
        if let Some(window) = view.window()
            && !window.makeFirstResponder(Some(&**view))
        {
            return Err(ViewError::Revoked.into());
        }
        Ok(())
    }

    /// This actual child-boundary event queries the parent hit synchronously, without forwarding mouse methods or retaining/replaying events.
    /// End Doc/slot borrows before Native conversion; verify the same child permit and original parent Scene geometry on both sides.
    fn cell_drag_target(
        &self,
        active: &Rc<ActiveCell>,
        permit: &NativeMarkdownPagedInputPermit,
        pointer: &NativeMarkdownPagedPointerLocation,
    ) -> std::result::Result<
        crate::native_surface::paged_text_view::NativeMarkdownPagedCellExtension,
        ViewError,
    > {
        let parent = self.view().map_err(cell_view_error)?;
        let request = parent.capture_scene_request()?;
        let scene = self.ready_scene(&request)?;
        let current = || self.scene_current(scene.key()) && self.owns_cell(active);
        scene.validate_ready(&current)?;
        let point = pointer.window_point;
        scene.validate_ready(&current)?;
        active.body.input.check_permit(permit, &|epoch| {
            active.body.current(epoch) && self.owns_cell(active)
        })?;
        let local = parent.convertPoint_fromView(point, None);
        scene.validate_ready(&current)?;
        if !local.x.is_finite() || !local.y.is_finite() {
            return Err(ViewError::InvalidGeometry);
        }
        let actual = parent.capture_scene_request();
        scene.validate_ready(&current)?; // Native/current revocation cannot replace actual original Source failure.
        if !actual?.same(&request) {
            return Err(ViewError::Revoked);
        }
        active.body.input.check_permit(permit, &|epoch| {
            active.body.current(epoch) && self.owns_cell(active)
        })?;
        use crate::native_surface::paged_text_view::{
            NativeMarkdownPagedCellDragAnchor as Anchor,
            NativeMarkdownPagedCellExtension as Extension,
            NativeMarkdownPagedWordSelection as WordSelection,
        };
        let target = match pointer.anchor.ok_or(ViewError::Pending)? {
            Anchor::Caret { .. } => {
                let (selection, head_affinity) =
                    scene.mouse_selection(local, pointer.clicks, None, false)?;
                Extension::Selection {
                    selection,
                    head_affinity,
                    intent: pointer.intent.clone(),
                }
            }
            Anchor::Word(anchor) => {
                // An actual Grid hit also supplies row/column query intent; a bare canonical position cannot authorize another cell display domain.
                let (head_utf16, backward_bias, cell) = scene.word_hit(local)?;
                Extension::Word {
                    selection: WordSelection {
                        head_utf16,
                        backward_bias,
                        anchor: Some(anchor),
                        cell,
                    },
                    intent: pointer.intent.clone(),
                }
            }
        };
        scene.validate_ready(&current)?;
        Ok(target) // Return temporary Scene/Reader claims before the subsequent canonical commit.
    }

    /// Only actual Word results transfer the endpoint to after-state; ordinary input/new Down cannot renew an old request from current state.
    pub(super) fn finish_pointer_word_group(
        &self,
        before: &NativeMarkdownPagedSceneRequest,
        after: &NativeMarkdownPagedSceneRequest,
        anchor: crate::native_surface::paged_text_view::NativeMarkdownPagedWordAnchor,
        intent: &Weak<()>,
        commit: Option<&crate::native_surface::paged_input::NativeMarkdownPagedInputCommit>,
    ) -> Result<()> {
        if !before.target.same(&after.target)
            || !Weak::ptr_eq(&before.document, &after.document)
            || before.input.generation != after.input.generation
        {
            return Err(ViewError::Revoked.into());
        }
        if let Some(commit) = commit {
            commit
                .check_current(&|epoch| self.current(epoch))
                .map_err(ViewError::Input)?;
            if commit.state != after.input || commit.invalidation.characters_changed {
                return Err(ViewError::Revoked.into());
            }
        } else if !before.same(after) {
            return Err(ViewError::Revoked.into());
        }
        let pending = self.pending_cell.borrow().clone();
        if let Some(pending) = pending
            && let PendingCell::ParentPointer {
                request,
                pointer,
                waiting_group,
                child: None,
                ..
            } = &*pending
            && request.borrow().same(before)
            && Weak::ptr_eq(&pointer.borrow().intent, intent)
        {
            let finished = {
                let mut pointer = pointer.borrow_mut();
                pointer.anchor = Some(
                    crate::native_surface::paged_text_view::NativeMarkdownPagedCellDragAnchor::Word(
                        anchor,
                    ),
                );
                !pointer.dragging && !pointer.mouse_up
            };
            *request.borrow_mut() = after.clone();
            waiting_group.set(false);
            if finished {
                self.clear_cell_pending(&pending);
            } else {
                self.schedule()?;
            }
        }
        if self.input.is_cell()
            && let Some(parent) = self.cell_parent.borrow().upgrade()
        {
            parent.accept_child_word_group(self, before, after, anchor, intent)?;
        }
        Ok(())
    }

    /// Parent Down maps an actual group from unchanged raw text into canonical; child-owned Down stays local until the same sanitizer transaction.
    /// The original slot proves admitted Up; clearing View's Down Rc cannot discard the newly completed first group.
    fn accept_child_word_group(
        &self,
        child: &Self,
        before: &NativeMarkdownPagedSceneRequest,
        after: &NativeMarkdownPagedSceneRequest,
        anchor: crate::native_surface::paged_text_view::NativeMarkdownPagedWordAnchor,
        intent: &Weak<()>,
    ) -> Result<()> {
        use crate::native_surface::paged_text_view::{
            NativeMarkdownPagedCellDragAnchor as Drag, NativeMarkdownPagedWordAnchor as Anchor,
        };
        let pending = self.pending_cell.borrow().clone();
        let Some(pending) = pending else {
            return Ok(());
        };
        let active = self.active_cell.borrow().clone();
        let Some(active) = active.filter(|active| Weak::ptr_eq(&active.body.weak, &child.weak))
        else {
            return Ok(());
        };
        if !child.view()?.capture_scene_request()?.same(after) || !self.owns_cell(&active) {
            return Err(ViewError::Revoked.into());
        }
        match &*pending {
            PendingCell::Pointer {
                owner,
                pointer,
                applied,
                endpoint,
            } if applied.get()
                && Weak::ptr_eq(&pointer.intent, intent)
                && owner
                    .upgrade()
                    .is_some_and(|owner| Rc::ptr_eq(&owner, &active)) =>
            {
                if after.input.generation != 0 {
                    return Err(ViewError::Pending.into());
                }
                let range = {
                    let document = self
                        .document
                        .borrow()
                        .as_ref()
                        .expect("原raw仍属于父Doc")
                        .clone();
                    active
                        .source
                        .canonical_range_for(&document.borrow())?
                        .clone()
                };
                let position = |position: usize| {
                    if position > range.end - range.start {
                        Err(ViewError::InvalidRange)
                    } else {
                        Ok(range.start + position)
                    }
                };
                let canonical = match anchor {
                    Anchor::Selection(anchor) => Anchor::Selection(position(anchor)?),
                    Anchor::Group {
                        from_utf16,
                        to_utf16,
                        origin_utf16,
                    } => Anchor::Group {
                        from_utf16: position(from_utf16)?,
                        to_utf16: position(to_utf16)?,
                        origin_utf16: position(origin_utf16)?,
                    },
                    Anchor::Word { .. } => unreachable!("只接原UBA实际完成的词组"),
                };
                let view = self.view()?;
                let request = view.capture_scene_request()?;
                if request.input != active.parent_request.input {
                    return Err(ViewError::Revoked.into());
                }
                if let Anchor::Group {
                    from_utf16,
                    to_utf16,
                    ..
                } = canonical
                {
                    view.accept_pointer_word_group(&request, from_utf16..to_utf16, intent)?;
                }
                let endpoint = endpoint.borrow_mut().take();
                if let Some(mut endpoint) = endpoint {
                    endpoint.anchor = Some(Drag::Word(canonical));
                    *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::ParentPointer {
                        request: RefCell::new(request),
                        pointer: RefCell::new(endpoint),
                        down: RefCell::new(None),
                        waiting_group: Cell::new(false),
                        child: None,
                    }));
                    self.schedule()?;
                } else {
                    self.clear_cell_pending(&pending);
                }
            }
            PendingCell::ParentPointer {
                request,
                pointer,
                waiting_group,
                child: Some(owner),
                ..
            } if owner
                .upgrade()
                .is_some_and(|owner| Rc::ptr_eq(&owner, &active))
                && request.borrow().same(before)
                && Weak::ptr_eq(&pointer.borrow().intent, intent) =>
            {
                *request.borrow_mut() = after.clone();
                pointer.borrow_mut().anchor = Some(Drag::Word(anchor));
                waiting_group.set(false);
                self.schedule()?;
            }
            _ => {} // New Down revoked the old slot after Up; a late result cannot recreate endpoints or drag lifetime.
        }
        Ok(())
    }

    /// Child-boundary movement preserves original Down's Caret/Word granularity; an actual completed first group cannot be replaced by its bare seed.
    /// Publish the draft once. The sanitizer receipt maps selection, first group, and original click together; parent never re-expands the old word.
    pub(super) fn accept_cell_drag(
        &self,
        child: &Self,
        request: &NativeMarkdownPagedSceneRequest,
        pointer: NativeMarkdownPagedPointerLocation,
        permit: NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        let active = self
            .active_cell
            .borrow()
            .clone()
            .filter(|active| Weak::ptr_eq(&active.body.weak, &child.weak))
            .ok_or(ViewError::Revoked)?;
        child.input.check_permit(&permit, &|epoch| {
            child.current(epoch) && self.owns_cell(&active)
        })?;
        let view = child.view().map_err(cell_view_error)?;
        if !view.cell_pointer_is_current(&pointer.intent)
            || !view.capture_scene_request()?.same(request)
            || !view.cell_pointer_is_current(&pointer.intent)
        {
            return Err(ViewError::Revoked);
        }
        let pending = self.pending_cell.borrow().clone();
        if let Some(pending) = pending {
            if let PendingCell::ParentPointer {
                request: original,
                pointer: latest,
                child: Some(owner),
                ..
            } = &*pending
                && owner
                    .upgrade()
                    .is_some_and(|owner| Rc::ptr_eq(&owner, &active))
                && original.borrow().same(request)
                && Weak::ptr_eq(&latest.borrow().intent, &pointer.intent)
                && !latest.borrow().mouse_up
            {
                *latest.borrow_mut() = pointer;
                self.schedule().map_err(cell_view_error)?;
                return Ok(true);
            }
            return Err(ViewError::Pending);
        }
        let waiting = matches!(
            pointer.anchor,
            None | Some(
                crate::native_surface::paged_text_view::NativeMarkdownPagedCellDragAnchor::Word(
                    crate::native_surface::paged_text_view::NativeMarkdownPagedWordAnchor::Word { .. }
                )
            )
        );
        *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::ParentPointer {
            request: RefCell::new(request.clone()),
            pointer: RefCell::new(pointer),
            down: RefCell::new(None),
            waiting_group: Cell::new(waiting),
            child: Some(Rc::downgrade(&active)),
        }));
        self.schedule().map_err(cell_view_error)?;
        Ok(true) // Admit original endpoint only; the same slot consumes it after actual Group/parent Scene completion, without event retention/replay.
    }

    fn apply_cell_drag(
        &self,
        child: &Self,
        request: &NativeMarkdownPagedSceneRequest,
        pointer: NativeMarkdownPagedPointerLocation,
        permit: NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        use crate::native_surface::paged_text_view::{
            NativeMarkdownPagedCellDragAnchor as Drag,
            NativeMarkdownPagedCellExtension as Extension, NativeMarkdownPagedWordAnchor as Anchor,
        };
        let active = self
            .active_cell
            .borrow()
            .clone()
            .ok_or(ViewError::Revoked)?;
        if !Weak::ptr_eq(&active.body.weak, &child.weak) {
            return Err(ViewError::Revoked);
        }
        child
            .input
            .check_permit(&permit, &|epoch| child.current(epoch))?;
        if !request.target.same(&child.input.target_domain()?)
            || (!pointer.mouse_up
                && !child
                    .view()
                    .map_err(cell_view_error)?
                    .cell_pointer_is_current(&pointer.intent))
        {
            return Err(ViewError::Revoked);
        }
        let state = child.input.state()?;
        if !state.selection_is_directed {
            return Err(ViewError::Revoked);
        }
        let pending = self.pending_cell.borrow().clone();
        let accepted = pending.as_ref().is_some_and(|pending| {
            matches!(&**pending,
            PendingCell::ParentPointer { pointer: actual, child: Some(owner), .. }
                if Weak::ptr_eq(&actual.borrow().intent, &pointer.intent)
                    && owner.upgrade().is_some_and(|owner| Rc::ptr_eq(&owner, &active)))
        });
        if !accepted || child.has_pending_input() {
            return Err(ViewError::Pending); // Do not cancel an unfinished first group or treat uncommitted state as completed cross-cell selection.
        }
        if pointer.mouse_up && !pointer.dragging {
            // Original Down and same-physical-point Up add no extension; after actual first-group completion they end the gesture only.
            // Still validate active raw Source against the same Doc/root; omitting another hit cannot hide actual mmap/FD failure.
            let original = self
                .document
                .borrow()
                .as_ref()
                .expect("原格 Up 仍属于父文档")
                .clone();
            active
                .source
                .canonical_range_for(&original.borrow())
                .map_err(|cause| cell_view_error(cause.into()))?;
            return Ok(true); // Do not commit the draft or interpret glyph movement after visibility changes as another user drag.
        }
        let drag_anchor = pointer.anchor.ok_or(ViewError::Pending)?;
        let word_anchor = match drag_anchor {
            Drag::Caret { anchor_utf16 } => {
                if state.selection.anchor_utf16 != anchor_utf16 {
                    return Err(ViewError::Revoked);
                }
                None // Original Input and the same transaction's selection receipt map the actual directed caret anchor.
            }
            Drag::Word(Anchor::Word { .. }) => return Err(ViewError::Pending),
            Drag::Word(Anchor::Selection(position)) => Some(MarkdownCellDraftWordAnchor {
                range: MarkdownTextSelection::caret(position),
                origin_utf16: position,
            }),
            Drag::Word(Anchor::Group {
                from_utf16,
                to_utf16,
                origin_utf16,
            }) => Some(MarkdownCellDraftWordAnchor {
                range: MarkdownTextSelection {
                    anchor_utf16: from_utf16,
                    head_utf16: to_utf16,
                },
                origin_utf16,
            }),
        };
        let extension = self.cell_drag_target(&active, &permit, &pointer)?;
        let head = match &extension {
            Extension::Selection { selection, .. } => selection.head_utf16,
            Extension::Word { selection, .. } => selection.head_utf16,
        };
        let range = {
            let document = self
                .document
                .borrow()
                .as_ref()
                .expect("原格拖选正文")
                .clone();
            let document = document.borrow();
            active
                .source
                .canonical_range_for(&document)
                .map_err(|cause| cell_view_error(cause.into()))?
                .clone()
        };
        if range.contains(&head) {
            return Err(ViewError::Pending); // Old parent display coordinates cannot locate modified child Source inside that same raw cell.
        }
        let committed = self
            .commit_owned_cell(
                &active,
                None,
                Some(&permit),
                word_anchor,
                Some((&pointer.intent, drag_anchor)),
                None,
            )
            .map_err(cell_view_error)?;
        // The Native mouse stack still owns child; queued detachment follows its return. Consume only this actual publication result here.
        // Successor revocation cannot restore/replay the old draft; any failure remains an already-committed effect.
        let effects = (|| -> std::result::Result<bool, ViewError> {
            let current = |epoch| {
                self.current(epoch)
                    && match &committed.outcome {
                        NativeMarkdownPagedInputOutcome::Committed(commit) => {
                            commit.check_current(&|epoch| self.current(epoch)).is_ok()
                        }
                        NativeMarkdownPagedInputOutcome::Unchanged(state) => {
                            self.input.state().is_ok_and(|actual| actual == *state)
                        }
                    }
            };
            let permit = self.input.capture(self.configuration.epoch, &current)?;
            match extension {
                Extension::Selection {
                    selection,
                    head_affinity,
                    ..
                } => {
                    let head = map_position(selection.head_utf16, &committed.outcome)
                        .map_err(cell_view_error)?;
                    let outcome = self.input.set_selection_intent(
                        &permit,
                        MarkdownTextSelection {
                            anchor_utf16: committed.receipt.selection.anchor_utf16,
                            head_utf16: head,
                        },
                        Some(head_affinity.is_upstream()),
                        None,
                        &current,
                    )?;
                    if let NativeMarkdownPagedInputOutcome::Committed(commit) = outcome {
                        let view = self.view().map_err(cell_view_error)?;
                        view.finish_word_delete_commit(&commit)?;
                        view.adopt_cell_pointer_selection(&commit, &pointer.intent, None)?;
                    }
                }
                Extension::Word {
                    mut selection,
                    intent,
                } => {
                    selection.head_utf16 = map_position(selection.head_utf16, &committed.outcome)
                        .map_err(cell_view_error)?;
                    selection.cell = map_cell_word_target(selection.cell, &committed.outcome)
                        .map_err(cell_view_error)?;
                    let mapped = committed
                        .receipt
                        .word_anchor
                        .expect("原Word三点已在发布前真实映射");
                    selection.anchor = Some(match drag_anchor {
                        Drag::Word(Anchor::Selection(_)) => {
                            Anchor::Selection(mapped.range.anchor_utf16)
                        }
                        Drag::Word(Anchor::Group { .. }) => Anchor::Group {
                            from_utf16: mapped.range.anchor_utf16,
                            to_utf16: mapped.range.head_utf16,
                            origin_utf16: mapped.origin_utf16,
                        },
                        _ => unreachable!("只有已完成的原Word入口产生Word目标"),
                    });
                    let view = self.view().map_err(cell_view_error)?;
                    let request = view.capture_scene_request()?;
                    self.select_word(&request, selection, Some(intent), permit)?;
                }
            }
            Ok(true)
        })();
        effects.map_err(|cause| ViewError::CommittedEffect(Box::new(cause)))
    }

    /// When the pointer leaves active raw bounds, commit that draft first, then map the original directed selection through the actual Source change.
    /// Inside replacement use Core's actual endpoint mapping; only external anchors may use the actual change delta.
    pub(super) fn extend_cell_selection(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        extension: crate::native_surface::paged_text_view::NativeMarkdownPagedCellExtension,
        permit: NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        use crate::native_surface::paged_text_view::{
            NativeMarkdownPagedCellExtension as Extension, NativeMarkdownPagedWordAnchor as Anchor,
        };
        let active = self.active_cell.borrow().clone();
        let Some(active) = active else {
            return Ok(false);
        };
        self.input
            .check_permit(&permit, &|epoch| self.current(epoch))?;
        if self.input.state()? != request.input || active.body.has_pending_input() {
            return Err(ViewError::Pending);
        }
        let document = self.document.borrow().as_ref().expect("原扩选Doc").clone();
        let range = active
            .source
            .canonical_range_for(&document.borrow())
            .map_err(|cause| cell_view_error(cause.into()))?
            .clone();
        // A first group inside the active cell must pass both actual endpoints through this sanitizer; a multiword current selection cannot substitute.
        // Wait for an unready seed and reject groups crossing unauthorized raw boundaries; neither permits early draft publication.
        let word_anchor = match &extension {
            Extension::Word { selection, .. } => match selection.anchor {
                Some(Anchor::Word { head_utf16, .. }) if range.contains(&head_utf16) => {
                    return Err(ViewError::Pending);
                }
                Some(Anchor::Selection(position))
                    if range.start <= position && position <= range.end =>
                {
                    Some(MarkdownCellDraftWordAnchor {
                        range: MarkdownTextSelection::caret(position - range.start),
                        origin_utf16: position - range.start,
                    })
                }
                Some(Anchor::Group {
                    from_utf16,
                    to_utf16,
                    origin_utf16,
                }) if range.start <= from_utf16 && to_utf16 <= range.end => {
                    Some(MarkdownCellDraftWordAnchor {
                        range: MarkdownTextSelection {
                            anchor_utf16: from_utf16 - range.start,
                            head_utf16: to_utf16 - range.start,
                        },
                        origin_utf16: origin_utf16 - range.start,
                    })
                }
                Some(Anchor::Group {
                    from_utf16,
                    to_utf16,
                    ..
                }) if from_utf16 < range.end && range.start < to_utf16 => {
                    return Err(ViewError::InvalidRange);
                }
                _ => None, // An external first group uses this actual replacement delta and borrows no position inside active raw text.
            },
            Extension::Selection { .. } => None,
        };
        let intent = match &extension {
            Extension::Selection { intent, .. } | Extension::Word { intent, .. } => intent,
        };
        let original_pointer = self.pending_cell.borrow().clone().filter(|pending| {
            matches!(&**pending, PendingCell::ParentPointer {
                request: original,
                pointer,
                child: None,
                ..
            } if original.borrow().same(request) && Weak::ptr_eq(&pointer.borrow().intent, intent))
        });
        if original_pointer.is_none() {
            self.pending_cell.borrow_mut().take();
        } // The admitted parent endpoint keeps its sole slot through publication; new Down revokes this exact scope even if Input remains equal.
        let result = self.commit_owned_cell(
            &active,
            None,
            None,
            word_anchor,
            None,
            original_pointer.as_ref(),
        );
        if let Some(original) = &original_pointer {
            self.clear_cell_pending(original);
        } // Remove the consumed original endpoint only; an old exit cannot clear a new slot installed during Native reentry.
        let committed = result.map_err(cell_view_error)?;
        let view = self.view().map_err(cell_view_error)?;
        let current = |epoch| {
            self.current(epoch)
                && match &committed.outcome {
                    NativeMarkdownPagedInputOutcome::Committed(commit) => {
                        commit.check_current(&|epoch| self.current(epoch)).is_ok()
                    }
                    NativeMarkdownPagedInputOutcome::Unchanged(state) => {
                        self.input.state().is_ok_and(|actual| actual == *state)
                    }
                }
        };
        let permit = self.input.capture(self.configuration.epoch, &current)?;
        match extension {
            Extension::Selection {
                selection,
                head_affinity,
                intent,
            } => {
                let head = map_position(selection.head_utf16, &committed.outcome)
                    .map_err(cell_view_error)?;
                let anchor = if selection.anchor_utf16 == selection.head_utf16 {
                    head // A new Down's empty caret inherits no old anchor from the previous cell draft.
                } else if range.start <= selection.anchor_utf16
                    && selection.anchor_utf16 <= range.end
                {
                    committed.receipt.selection.anchor_utf16
                } else {
                    map_position(selection.anchor_utf16, &committed.outcome)
                        .map_err(cell_view_error)?
                };
                let outcome = self.input.set_selection_intent(
                    &permit,
                    MarkdownTextSelection {
                        anchor_utf16: anchor,
                        head_utf16: head,
                    },
                    Some(head_affinity.is_upstream()),
                    None,
                    &current,
                )?;
                if let NativeMarkdownPagedInputOutcome::Committed(commit) = outcome {
                    view.finish_word_delete_commit(&commit)?;
                    view.adopt_cell_pointer_selection(&commit, &intent, None)?;
                }
            }
            Extension::Word {
                mut selection,
                intent,
            } => {
                selection.head_utf16 = map_position(selection.head_utf16, &committed.outcome)
                    .map_err(cell_view_error)?;
                selection.cell = map_cell_word_target(selection.cell, &committed.outcome)
                    .map_err(cell_view_error)?;
                selection.anchor = selection
                    .anchor
                    .map(|anchor| {
                        if let Some(mapped) = committed.receipt.word_anchor {
                            return Ok(match anchor {
                                Anchor::Selection(_) => {
                                    Anchor::Selection(mapped.range.anchor_utf16)
                                }
                                Anchor::Group { .. } => Anchor::Group {
                                    from_utf16: mapped.range.anchor_utf16,
                                    to_utf16: mapped.range.head_utf16,
                                    origin_utf16: mapped.origin_utf16,
                                },
                                Anchor::Word { .. } => unreachable!("未Ready种子在提交前已拒绝"),
                            });
                        }
                        map_word_anchor(anchor, &committed.outcome)
                    })
                    .transpose()
                    .map_err(cell_view_error)?;
                let request = view.capture_scene_request()?;
                self.select_word(&request, selection, Some(intent), permit)?;
                // The same original gesture delivers its later target group; create no Commit or revived Down before actual Ready.
            }
        }
        self.focus_cell_parent().map_err(cell_view_error)?;
        Ok(true)
    }

    /// The bounded editor follows actual original virtual-viewport translation. Without new measurements, move only its known prefix frame;
    /// an offscreen row must not lose its draft or borrow another row's current frame.
    fn reposition_cell(&self) -> Result<()> {
        let active = self.active_cell.borrow().clone();
        let Some(active) = active else {
            return Ok(());
        };
        let request = self.view()?.capture_scene_request()?;
        let before = &active.parent_request;
        let delta = NSPoint::new(
            before.virtual_viewport.document_origin.x - request.virtual_viewport.document_origin.x,
            request.viewport.size.height - before.viewport.size.height
                + request.virtual_viewport.document_origin.y
                - before.virtual_viewport.document_origin.y,
        );
        let mut frame = active.scroll.frame();
        frame.origin = NSPoint::new(
            active.content_frame.origin.x + delta.x,
            active.content_frame.origin.y + delta.y,
        );
        if active.scroll.frame() != frame {
            active.scroll.setFrame(frame);
        }
        Ok(())
    }
}

pub(super) fn cell_view_error(cause: NativeMarkdownPagedBodyError) -> ViewError {
    // Preserve typed View/Busy results at the Native boundary. Other actual Body
    // failures are displayed once here and revoke this individual View operation.
    match cause {
        NativeMarkdownPagedBodyError::View(cause) => cause,
        NativeMarkdownPagedBodyError::Busy => ViewError::Busy,
        cause => {
            crate::native_surface::present_native_markdown_error(&cause.to_string());
            ViewError::Revoked
        }
    }
}

/// Map existing anchors outside the actual replacement only; interior endpoints require Core's same-transaction receipt.selection, never guesses.
fn map_position(position: usize, outcome: &NativeMarkdownPagedInputOutcome) -> Result<usize> {
    let NativeMarkdownPagedInputOutcome::Committed(commit) = outcome else {
        return Ok(position);
    };
    let Some(range) = &commit.invalidation.replaced_utf16_range else {
        return Ok(position);
    };
    if position <= range.start {
        return Ok(position);
    }
    if position < range.end {
        return Err(ViewError::InvalidRange.into());
    }
    position
        .checked_add_signed(commit.invalidation.change_in_utf16_length)
        .ok_or(ViewError::InvalidRange.into())
}

fn map_pointer(
    mut pointer: ReopenPointer,
    outcome: &NativeMarkdownPagedInputOutcome,
) -> Result<ReopenPointer> {
    // Map only scalar intent across the actual canonical change. Reopen must
    // later reacquire row/raw/frame from its successor Scene, not reuse old tickets.
    pointer.head = map_position(pointer.head, outcome)?;
    pointer.anchor = pointer
        .anchor
        .map(|position| map_position(position, outcome))
        .transpose()?;
    pointer.word_anchor = pointer
        .word_anchor
        .map(|anchor| map_word_anchor(anchor, outcome))
        .transpose()?;
    Ok(pointer)
}

/// Row/column is actual hit query intent only; commit maps the known row position without renewing old Source tickets.
/// Successor Producer must reacquire actual row/raw/display at that position/column before querying the target word group.
fn map_cell_word_target(
    target: Option<crate::native_surface::paged_text_view::NativeMarkdownPagedCellWordTarget>,
    outcome: &NativeMarkdownPagedInputOutcome,
) -> Result<Option<crate::native_surface::paged_text_view::NativeMarkdownPagedCellWordTarget>> {
    target
        .map(|mut target| {
            target.row_start = map_position(target.row_start, outcome)?;
            Ok(target)
        })
        .transpose()
}

fn map_word_anchor(
    anchor: crate::native_surface::paged_text_view::NativeMarkdownPagedWordAnchor,
    outcome: &NativeMarkdownPagedInputOutcome,
) -> Result<crate::native_surface::paged_text_view::NativeMarkdownPagedWordAnchor> {
    // Keep each original anchor role and map every coordinate through the same
    // receipt; interior replacement coordinates require the sanitizer's direct
    // result instead of this external-position helper.
    use crate::native_surface::paged_text_view::NativeMarkdownPagedWordAnchor as Anchor;
    Ok(match anchor {
        Anchor::Selection(position) => Anchor::Selection(map_position(position, outcome)?),
        Anchor::Group {
            from_utf16,
            to_utf16,
            origin_utf16,
        } => Anchor::Group {
            from_utf16: map_position(from_utf16, outcome)?,
            to_utf16: map_position(to_utf16, outcome)?,
            origin_utf16: map_position(origin_utf16, outcome)?,
        },
        Anchor::Word {
            head_utf16,
            backward_bias,
            cell,
        } => Anchor::Word {
            head_utf16: map_position(head_utf16, outcome)?,
            backward_bias,
            cell: map_cell_word_target(cell, outcome)?,
        },
    })
}

impl NativeMarkdownPagedBody {
    /// Validate the same actual owner before detaching its slot; Native removal occurs outside borrows and its receipt joins the original parent close graph.
    fn detach_cell(&self, active: &Rc<ActiveCell>) -> Result<Rc<NativeMarkdownPagedCloseReceipt>> {
        if !self.owns_cell(active) {
            return Err(ViewError::Revoked.into());
        }
        let receipt = active.body.begin_close()?;
        self.active_cell.borrow_mut().take();
        active.scroll.removeFromSuperview();
        *self.cell_close.borrow_mut() = Some(receipt.clone());
        let weak = MainThreadBound::new(self.weak.clone(), self.mtm);
        let original = Rc::downgrade(&receipt);
        receipt.when_released(move || {
            let mtm = MainThreadMarker::new().expect("格像素归还仍在原主队列");
            let Some(parent) = weak.get(mtm).upgrade() else {
                return;
            };
            let same = parent
                .cell_close
                .borrow()
                .as_ref()
                .is_some_and(|receipt| Weak::ptr_eq(&original, &Rc::downgrade(receipt)));
            if same {
                let receipt = parent.cell_close.borrow_mut().take().expect("已核原关闭票");
                if receipt.verify_released().is_err() {
                    *parent.cell_close.borrow_mut() = Some(receipt);
                    parent.fail(NativeMarkdownPagedBodyError::ResourceReleaseFailed);
                } else if parent.current_body()
                    && let Err(cause) = parent.schedule()
                {
                    parent.fail(cause);
                }
            }
        });
        self.notify_local_cell_input();
        Ok(receipt)
    }

    /// Core may seal while the actual mouse callback owns child Native stack; physical detachment must await that stack's return.
    /// Register weak work before commit notifications schedule layout; handle this exact owner without occupying the pointer continuation slot.
    fn queue_finished_cell_detach(&self, active: &Rc<ActiveCell>) {
        let original = MainThreadBound::new((self.weak.clone(), Rc::downgrade(active)), self.mtm);
        super::super::enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("格拆图仍归原主队列");
            let (parent, active) = original.get(mtm);
            let (Some(parent), Some(active)) = (parent.upgrade(), active.upgrade()) else {
                return;
            };
            if !parent.current_body() || !parent.owns_cell(&active) {
                return; // Once product close owns this graph, do not repeat removal or acquire a receipt for a successor cell.
            }
            let result = parent
                .detach_cell(&active)
                .and_then(|_| parent.focus_cell_parent());
            if let Err(cause) = result {
                parent.fail(cause); // This is actual cleanup failure; queued work or Core seal is not resource return.
            }
        });
    }

    /// Save/handoff/mode preflight admits only this actual draft; pending selection/navigation rejects rather than masquerading as saved state.
    /// Success includes its original canonical receipt; external commands validate it without acquiring newer after-state to renew old intent.
    pub(in crate::native_surface) fn commit_active_cell(
        &self,
    ) -> Result<Option<NativeMarkdownPagedCellCommit>> {
        if self.preparing.get() || self.pending_cell.borrow().is_some() {
            return Err(NativeMarkdownPagedBodyError::Busy);
        }
        let Some(active) = self.active_cell.borrow().clone() else {
            return Ok(None);
        };
        active.body.preflight_release()?;
        if active.body.has_pending_input() {
            return Err(NativeMarkdownPagedBodyError::Busy);
        }
        active.body.view()?.commit_composition_before_release()?;
        self.commit_owned_cell(&active, None, None, None, None, None)
            .map(Some)
    }

    /// Complete the same prepared transaction, return parent/child read graphs, then publish once. Retirement changes display serial,
    /// while current checks original parent/child Input and sole-slot ownership, not a retired Scene nonce as editing permission.
    fn commit_owned_cell(
        &self,
        active: &Rc<ActiveCell>,
        append: Option<(
            &omni_markdown_core::markdown_document_buffer::VerifiedMarkdownTableRowSource,
            omni_markdown_core::MarkdownViewportSourceAuthority,
        )>,
        original_cell: Option<&NativeMarkdownPagedInputPermit>,
        word_anchor: Option<MarkdownCellDraftWordAnchor>,
        transfer: Option<(
            &Weak<()>,
            crate::native_surface::paged_text_view::NativeMarkdownPagedCellDragAnchor,
        )>,
        parent_pointer: Option<&Rc<PendingCell>>,
    ) -> Result<NativeMarkdownPagedCellCommit> {
        if self.preparing.get() || active.body.preparing.get() || !self.owns_cell(active) {
            return Err(NativeMarkdownPagedBodyError::Busy);
        }
        let pointer_current = || {
            // Parent Up ended dragging but retains this one admitted endpoint. Recheck its exact slot immediately before actual publish;
            // a live Weak or equal InputState cannot prove a newer Down did not revoke it.
            parent_pointer.is_none_or(|original| {
                self.pending_cell
                    .borrow()
                    .as_ref()
                    .is_some_and(|actual| Rc::ptr_eq(actual, original))
            }) && transfer.is_none_or(|(intent, _)| {
            active.body.view().is_ok_and(|view| view.cell_pointer_is_current(intent))
                || self.pending_cell.borrow().as_ref().is_some_and(|pending| matches!(&**pending,
                    PendingCell::ParentPointer { pointer, child: Some(owner), .. }
                        if pointer.borrow().mouse_up && Weak::ptr_eq(&pointer.borrow().intent, intent)
                            && owner.upgrade().is_some_and(|owner| Rc::ptr_eq(&owner, active))))
        })
        }; // An admitted Up endpoint and live dragging are distinct; irreversible publish still requires its original slot not revoked by new Down.
        let body_current =
            |epoch| self.current(epoch) && self.owns_cell(active) && pointer_current();
        let cell_current =
            |epoch| active.body.current(epoch) && self.owns_cell(active) && pointer_current();
        let body_permit = self
            .input
            .capture(self.configuration.epoch, &body_current)
            .map_err(ViewError::Input)?;
        let cell_permit = active
            .body
            .input
            .capture(active.body.configuration.epoch, &cell_current)
            .map_err(ViewError::Input)?;
        let prepared = active
            .body
            .input
            .prepare_cell_commit(
                original_cell.unwrap_or(&cell_permit),
                append,
                word_anchor,
                &cell_current,
            )
            .map_err(ViewError::Input)?;
        active.body.release_cell_readers()?;
        if let Err(cause) = self.release_cell_readers() {
            active.body.schedule()?;
            self.schedule()?;
            return Err(cause);
        }
        let mut committed = match active.body.input.commit_cell_to_body(
            prepared,
            &self.input,
            &body_permit,
            &cell_current,
            &body_current,
        ) {
            Ok(committed) => committed,
            Err(cause) => {
                // Core rejection left the draft open; the same owner rebuilds its read graph without preparing again or replaying commit.
                let restored =
                    producer::merge_preparation_cleanup(active.body.schedule(), self.schedule());
                return producer::merge_preparation_cleanup(
                    Err(NativeMarkdownPagedBodyError::CellCommit(cause)),
                    restored,
                )
                .map(|()| unreachable!("合并的原操作失败不能变为成功"));
            }
        };
        // Core committed; subsequent failures are committed Native effects and cannot restore or replay the old draft.
        if transfer.is_some() {
            self.queue_finished_cell_detach(active);
        } // Detach after original mouse stack return; queue before notifications create parent layout and retain the actual close receipt.
        let canonical_invalidation = match &committed.outcome {
            NativeMarkdownPagedInputOutcome::Committed(commit) => Some(commit.invalidation.clone()),
            NativeMarkdownPagedInputOutcome::Unchanged(_) => None,
        };
        let effects = (|| -> std::result::Result<(), ViewError> {
            match &committed.outcome {
                NativeMarkdownPagedInputOutcome::Committed(commit) => {
                    self.view()
                        .map_err(|_| ViewError::Revoked)?
                        .finish_word_delete_commit(commit)?;
                    commit.check_current(&body_current)?;
                }
                NativeMarkdownPagedInputOutcome::Unchanged(state) => {
                    if self.input.state()? != *state {
                        return Err(ViewError::Revoked);
                    }
                }
            }
            let permit = self
                .input
                .capture(self.configuration.epoch, &body_current)?;
            if canonical_invalidation.is_none()
                && transfer.is_none()
                && parent_pointer.is_none()
                && word_anchor.is_none()
            {
                // Ordinary save/resignation seals an unchanged-byte draft only; mapped receipt positions do not imply parent-selection intent.
                // Local formatting can restore original bytes while leaving another directed local selection; this cannot move canonical selection,
                // advance its selection serial, or rewrite navigation side. Check original health/currentness, then return through normal close.
                // Explicit drag handoff still maps below; independent extension consumes this receipt to publish its own final selection.
                self.input.check_permit(&permit, &body_current)?;
                return Ok(());
            }
            let mut mapped = self.input.set_selection_intent(
                &permit,
                committed.receipt.selection,
                None,
                None,
                &body_current,
            )?;
            if let NativeMarkdownPagedInputOutcome::Committed(commit) = &mapped {
                self.view()
                    .map_err(|_| ViewError::Revoked)?
                    .finish_word_delete_commit(commit)?;
                commit.check_current(&body_current)?;
            }
            if let NativeMarkdownPagedInputOutcome::Committed(commit) = &mut mapped {
                if let Some(original) = &canonical_invalidation {
                    commit.invalidation.replaced_utf16_range =
                        original.replaced_utf16_range.clone();
                    commit.invalidation.change_in_utf16_length = original.change_in_utf16_length;
                    commit.invalidation.characters_changed = original.characters_changed;
                    commit.invalidation.folds_changed |= original.folds_changed;
                }
                committed.outcome = mapped;
            } // Selection Unchanged preserves the original text commit; a new selection receipt retains this full Source change's actual range/delta.
            Ok(())
        })();
        let effects = effects.and_then(|()| -> std::result::Result<(), ViewError> {
            if let Some((intent, anchor)) = transfer {
                use crate::native_surface::paged_text_view::{
                    NativeMarkdownPagedCellDragAnchor as Drag,
                    NativeMarkdownPagedWordAnchor as Anchor,
                };
                let word_anchor = match anchor {
                    Drag::Caret { .. } => None,
                    Drag::Word(anchor) => {
                        let mapped = committed
                            .receipt
                            .word_anchor
                            .expect("原首词组已经由同趟sanitizer映射");
                        Some(match anchor {
                            Anchor::Selection(_) => Anchor::Selection(mapped.range.anchor_utf16),
                            Anchor::Group { .. } => Anchor::Group {
                                from_utf16: mapped.range.anchor_utf16,
                                to_utf16: mapped.range.head_utf16,
                                origin_utf16: mapped.origin_utf16,
                            },
                            Anchor::Word { .. } => {
                                unreachable!("未完成的首组不得取得canonical提交票")
                            }
                        })
                    }
                };
                active
                    .body
                    .view()
                    .map_err(cell_view_error)?
                    .transfer_cell_pointer_to(
                        &self.view().map_err(cell_view_error)?,
                        &committed.outcome,
                        intent,
                        word_anchor,
                    )?;
            }
            Ok(())
        });
        let detached = if transfer.is_some() {
            Ok(()) // Only synchronous commit finished; parent still owns this child graph until queued actual detachment.
        } else {
            self.detach_cell(active).map(|_| ())
        };
        if let Err(cause) = effects {
            let original = ViewError::CommittedEffect(Box::new(cause));
            return producer::merge_preparation_cleanup(Err(original.into()), detached)
                .map(|()| unreachable!("合并的原操作失败不能变为成功"));
        }
        detached.map_err(|cause| {
            ViewError::CommittedEffect(Box::new(match cause {
                NativeMarkdownPagedBodyError::View(cause) => cause,
                _ => ViewError::Revoked,
            }))
        })?;
        Ok(committed)
    }
}

impl NativeMarkdownPagedBody {
    /// Return all display reads for this input graph; production cell commit calls this separately for parent and child,
    /// retaining original Input/session and displayed pixels so failure can restore this same draft.
    pub(super) fn release_cell_readers(&self) -> Result<()> {
        let next = self
            .serial
            .get()
            .checked_add(1)
            .ok_or(NativeMarkdownPagedBodyError::CounterExhausted)?;
        self.view()?.release_scene_readers()?; // Synchronous paint reports Busy before any draft sealing.
        self.serial.set(next);
        self.scheduled.set(None);
        self.deferred.set(false);
        drop(self.pending_vertical.borrow_mut().take());
        drop(self.pending_page_scroll.borrow_mut().take());
        drop(self.scene.borrow_mut().take());
        let producer = self.producer.borrow_mut().take();
        let producer_result = producer.map_or(Ok(()), Producer::retire);
        let worker = self.worker.borrow_mut().take();
        let worker_result = worker.map_or(Ok(()), FontWorker::close);
        self.font_notices.borrow_mut().clear();
        // Actually consume both old result owners before next capture/commit; earlier failure cannot skip joining original worker.
        producer::merge_preparation_cleanup(producer_result, worker_result)
    }

    /// Original Cell and existing canonical Input each retain their tickets for one commit; this entry does not mount table UI or implement Tab navigation.
    pub fn commit_cell_to_body(
        &self,
        body: &Rc<NativeMarkdownPagedInput>,
        body_permit: &NativeMarkdownPagedInputPermit,
        body_current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedCellCommit> {
        if self.preparing.get() || !self.input.is_cell() {
            return Err(NativeMarkdownPagedBodyError::Busy);
        }
        let view = self.view()?;
        let request = view.capture_scene_request()?;
        let current = |epoch| {
            self.current(epoch)
                && view
                    .capture_scene_request()
                    .is_ok_and(|actual| actual.same(&request))
        };
        let permit = self
            .input
            .capture(self.configuration.epoch, &current)
            .map_err(ViewError::Input)?;
        // Original ticket, marked state, document budget, or candidate-storage preparation failure returns here;
        // leave original Scene/frame intact without prematurely clearing display or truncating a long draft.
        let prepared = self
            .input
            .prepare_cell_commit(&permit, None, None, &current)
            .map_err(ViewError::Input)?;
        self.release_cell_readers()?;
        let result =
            self.input
                .commit_cell_to_body(prepared, body, body_permit, &current, body_current);
        match result {
            Ok(outcome) => {
                drop(self.selection_reveal.borrow_mut().take());
                self.close_finished_cell();
                Ok(outcome) // Core sealed the original Cell; only actual success revokes its reader-free caret intent.
            }
            Err(cause) => {
                let error = NativeMarkdownPagedBodyError::CellCommit(cause);
                if let Err(rebuild) = self.schedule() {
                    self.fail(rebuild); // Display restoration failure remains with the original owner without replacing this actual commit failure.
                } // The draft remains unchanged; rebuild actual Source through the same Input without replaying consumed prepared state.
                Err(error)
            }
        }
    }

    /// Esc consumes original Cell input authority only; after drawing/font return cancel the same token, closing Native graph after callback return.
    pub(super) fn cancel_cell(
        &self,
        permit: NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<(), ViewError> {
        if self.preparing.get() {
            return Err(ViewError::Busy);
        }
        self.input
            .check_permit(&permit, &|epoch| self.current(epoch))?;
        self.release_cell_readers().map_err(|cause| {
            self.fail(cause);
            ViewError::Revoked
        })?;
        // Actual context/font destruction may reenter input; the same original permit must remain valid, never cancelling a successor draft.
        if let Err(cause) = self
            .input
            .check_permit(&permit, &|epoch| self.current(epoch))
        {
            if let Err(rebuild) = self.schedule() {
                self.fail(rebuild); // If destruction reentry changed input, restore display only without replaying cancellation or renewing the old permit.
            }
            return Err(cause.into());
        }
        let session = self
            .cell_session
            .borrow()
            .clone()
            .ok_or(ViewError::Closed)?;
        session.close()?;
        drop(self.selection_reveal.borrow_mut().take()); // Revoke original intent only after actual same-token cancellation; rejection preserves successor display.
        self.input.close()?;
        self.close_finished_cell();
        Ok(())
    }

    /// After actual token commit/cancel, revoke callback authority immediately; Native detachment awaits input-stack return without rereading released draft.
    fn close_finished_cell(&self) {
        self.active.set(false); // Old clip/input/worker callbacks cannot rebuild; actual Native close waits for original stack return.
        let weak = MainThreadBound::new(self.weak.clone(), self.mtm);
        super::super::enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("单格关闭只在主线程完成");
            if let Some(owner) = weak.get(mtm).upgrade()
                && let Err(cause) = owner.begin_close()
            {
                owner.fail(cause);
            }
        });
    }
}
