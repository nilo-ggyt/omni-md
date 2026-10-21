//! One storage owns one source continuation. Actual visible preparation schedules
//! bounded steps; getters consume only published results of the same source.
//!
//! Initial synchronous TextKit layout, reverse requests without checkpoints, and
//! uncovered syntax still use the existing TextKit/reference path. This adapter
//! alone therefore does not establish strict viewport-only loading for that path.

use super::*;
use omni_markdown_core::markdown_document_buffer::{
    MarkdownOrdinarySemanticState, MarkdownTableRequestError, MarkdownTableViewportCoverage,
    MarkdownViewportSourceAuthority, MarkdownViewportSourceRequest, MarkdownViewportSourceStep,
    VerifiedMarkdownViewportSources,
};
use std::ops::Range;

const SOURCE_STEP: usize = 4096;
const MAX_SOURCE_VIEWPORT: usize = 16_384;

#[derive(Clone)]
/// Original input context and complete selection direction cannot be reacquired
/// afterward. Font scratch/table geometry changes do not change Core source identity.
struct SourceKey {
    document: WeakMarkdownDocument,
    generation: u64,
    length: usize,
    selection: MarkdownTextSelection,
    semantic_epoch: u64,
    input_epoch: u64,
}

/// Core completion and Native publication are separate boundaries. Getters cannot
/// see candidates; keep the old result until the same batch switches Native keys.
#[derive(Default)]
struct SourcePublications {
    committed: Option<Rc<VerifiedMarkdownViewportSources>>,
    staged: Option<Rc<VerifiedMarkdownViewportSources>>,
}

impl SourcePublications {
    /// Stage one completed Core result while preserving the last committed getter
    /// source. A second candidate cannot replace an unfinished Native publication.
    fn stage(&mut self, source: VerifiedMarkdownViewportSources) -> bool {
        if self.staged.is_some() {
            return false; // One original request permits only one candidate awaiting Native commit.
        }
        self.staged = Some(Rc::new(source));
        true
    }

    /// Callers first verify original-source/settled authority, then switch font and
    /// preview keys without Native calls. Return the prior claim for later destruction.
    fn commit_staged(&mut self) -> Option<Option<Rc<VerifiedMarkdownViewportSources>>> {
        let source = self.staged.take()?;
        Some(self.committed.replace(source))
    }
}

impl SourceKey {
    /// Scalar equality is necessary but insufficient: weak allocation identity
    /// separately prevents equal generations in different documents from matching.
    fn matches(&self, other: &Self) -> bool {
        std::rc::Weak::ptr_eq(&self.document, &other.document)
            && self.generation == other.generation
            && self.length == other.length
            && self.selection == other.selection
            && self.semantic_epoch == other.semantic_epoch
            && self.input_epoch == other.input_epoch
    }
    fn authority(&self) -> MarkdownViewportSourceAuthority {
        MarkdownViewportSourceAuthority {
            context_revision: self.semantic_epoch,
            selection: self.selection,
            cancelled: false,
        }
    }
}

/// This storage slot owns the request/publications. Queued tasks retain weak
/// identities only, so a continuation cannot keep the closed graph alive.
pub(in crate::native_surface) struct NativeMarkdownSourceOwner {
    key: SourceKey,
    view: Weak<PieceBackedMarkdownTextView>,
    request: RefCell<Option<MarkdownViewportSourceRequest>>,
    publications: RefCell<SourcePublications>,
    window: RefCell<Range<usize>>,
    desired: RefCell<Range<usize>>,
    pending: Cell<bool>,
    publication_pending: Cell<bool>,
    queued: Cell<bool>,
    failed: Cell<bool>,
}

impl NativeMarkdownSourceOwner {
    /// Seal this original continuation immediately. Discard uncommitted output and
    /// cancel Core scratch locally rather than waiting for a later queued callback.
    fn stop(&self) {
        self.pending.set(false);
        self.publication_pending.set(false);
        self.failed.set(true);
        let staged = self.publications.borrow_mut().staged.take();
        drop(staged); // Failed candidates never reach getters; old committed output still requires its own original-source checks.
        let mut request = self.request.borrow_mut().take();
        if let Some(request) = request.as_mut() {
            request.cancel(); // Core synchronously fixes cancellation and returns scratch without another weak-task turn.
        }
        drop(request);
    }
}

#[derive(Clone)]
/// A no-table proof weakly identifies the same Sources publication. It neither
/// copies indexes nor promotes an ordinary gap into proved plain semantics.
pub(in crate::native_surface) struct NativeMarkdownNoTablesProof {
    source: std::rc::Weak<VerifiedMarkdownViewportSources>,
    window: Range<usize>,
}

impl NativeMarkdownNoTablesProof {
    /// Require this exact publication still in the current owner's committed slot
    /// before consulting its original document coverage. No Native getters are used.
    pub(in crate::native_surface) fn is_current(
        &self,
        storage: &PieceBackedMarkdownTextStorage,
        document: &MarkdownDocumentBuffer,
    ) -> bool {
        let Some(owner) = storage.current_source_owner() else {
            return false; // New input or closure cannot preserve an old no-table receipt through equal scalar values.
        };
        let Some(source) = self.source.upgrade() else {
            return false; // The original publication ended; do not reconstruct another source with equal coordinates.
        };
        let owns = owner
            .publications
            .borrow()
            .committed
            .as_ref()
            .is_some_and(|current| Rc::ptr_eq(current, &source));
        owns && no_tables_covered(&source, document, owner.key.authority(), &self.window)
    }
}

/// Require continuous actual Gap coverage in this same Sources object. An
/// intersecting table, coverage hole, or out-of-coverage range has no no-table authority.
fn no_tables_covered(
    sources: &VerifiedMarkdownViewportSources,
    document: &MarkdownDocumentBuffer,
    authority: MarkdownViewportSourceAuthority,
    requested: &Range<usize>,
) -> bool {
    if requested.is_empty() {
        return false; // Empty snapshots cannot establish completed visible semantics.
    }
    let Ok(table) = sources.table_sources_for(document, authority) else {
        return false;
    };
    let Ok(coverage) = table.coverage_for(document) else {
        return false;
    };
    let mut cursor = requested.start;
    for part in coverage {
        let range = part.utf16_range();
        if range.end <= cursor {
            continue; // Actual coverage is source-ordered; skip only proved segments before this request.
        }
        if range.start > cursor {
            return false; // The envelope of surrounding gaps cannot fill an unknown middle segment.
        }
        let MarkdownTableViewportCoverage::Gap { gap_index, .. } = part else {
            return false; // Actual table coverage must retain the existing table feature path.
        };
        let Ok(Some(gap)) = table.gap_at_for(document, *gap_index) else {
            return false;
        };
        let Ok(actual) = gap.utf16_range_for(document) else {
            return false;
        };
        if actual.start > range.start || actual.end < range.end {
            return false; // Coverage must belong to this original gap record; coordinate comparison alone cannot authorize it.
        }
        cursor = range.end.min(requested.end);
        if cursor == requested.end {
            return true; // Every requested character has actual no-table coverage, without claiming inline semantics are ready.
        }
    }
    false
}

/// Source optimization cannot revoke foreground actions waiting for original
/// geometry/paint. Consume publication only after an actual settled event.
fn publication_is_settled(view: &PieceBackedMarkdownTextView) -> bool {
    let settled = !view.ivars().textkit_graph_teardown.get()
        && view.ivars().textkit_input_depth.get() == 0
        && !view.ivars().table_layout_rebuilding.get()
        && view
            .ivars()
            .marked_text_range
            .get()
            .is_none_or(|range| range.length == 0)
        && view.ivars().find_reveal.borrow().is_none()
        && view.ivars().mode_viewport.borrow().is_none()
        && view.ivars().table_mutation_anchor.borrow().is_none()
        && view.ivars().active_table_cell.borrow().is_none()
        && view.ivars().pending_table_cell_activation.is_empty();
    #[cfg(feature = "native-table-row-append")]
    {
        settled && !view.table_append_pending() // A final-cell continuation still owning commit authority also protects its original geometry.
    }
    #[cfg(not(feature = "native-table-row-append"))]
    {
        settled
    }
}

impl PieceBackedMarkdownTextStorage {
    /// Capture only stable original input metadata. Composition and incomplete
    /// Native transactions cannot authorize derived source preparation/publication.
    fn source_key(&self) -> Option<SourceKey> {
        if !self.ivars().accepts_edits.get()
            || self.ivars().composition_active.get()
            || self.ivars().native_input_depth.get() != 0
            || self.ivars().input_transaction.borrow().is_some()
        {
            return None; // A later actual preparation event may request work after this temporary input stage ends.
        }
        let semantic_epoch = self.ivars().presentation_semantic_epoch.get()?;
        let document = self.document()?;
        let current = document.borrow();
        Some(SourceKey {
            document: Rc::downgrade(&document),
            generation: current.generation(),
            length: current.utf16_len(),
            selection: current.selection(),
            semantic_epoch,
            input_epoch: self.ivars().input_transaction_epoch.get(),
        })
    }

    /// Clone the existing owner only when its complete original key still matches;
    /// this query never creates a replacement request or expands its budget.
    fn current_source_owner(&self) -> Option<Rc<NativeMarkdownSourceOwner>> {
        let key = self.source_key()?;
        let owner = self
            .ivars()
            .source_semantics
            .borrow()
            .as_ref()
            .map(Rc::clone)?;
        owner.key.matches(&key).then_some(owner)
    }

    fn owns_source(&self, original: &Rc<NativeMarkdownSourceOwner>) -> bool {
        self.current_source_owner()
            .is_some_and(|current| Rc::ptr_eq(&current, original))
    }

    /// Actual edit/selection/mode revocation and accepted release share this
    /// cancellation boundary; getters cannot reconstruct a global source request.
    pub(super) fn cancel_source_semantics(&self) {
        let owner = self.ivars().source_semantics.borrow_mut().take();
        if let Some(owner) = owner {
            owner.stop();
        } // End the slot borrow first; bounded Core destruction retains no document or Native claim.
    }

    /// Only settled visible preparation calls this entry. One live context creates
    /// one original SourceRequest; later windows use its actual forward continuation.
    pub(in crate::native_surface) fn prepare_source_semantics(
        &self,
        view: &PieceBackedMarkdownTextView,
        viewport: Range<usize>,
    ) {
        let Some(key) = self.source_key() else {
            return;
        };
        if viewport.is_empty() || viewport.len() > MAX_SOURCE_VIEWPORT || viewport.end > key.length
        {
            return; // Admit only bounded ranges from actual Native visible lines, never expanded paragraph/document extents.
        }
        let owner = if let Some(owner) = self.current_source_owner() {
            #[cfg(feature = "native-markdown-runtime-probe")]
            if *owner.desired.borrow() != viewport {
                native_markdown_trace! {
                    // Diagnose actual requested-window changes, not every Pending
                    // quantum or attribute getter invocation.
                    eprintln!("native_source_semantics event=prepare_existing generation={} selection={:?} semantic_epoch={} input_epoch={} window={:?} desired={:?} requested={viewport:?} pending={} publication_pending={} failed={} dirty={} formal=false",
                        owner.key.generation, owner.key.selection, owner.key.semantic_epoch, owner.key.input_epoch,
                        *owner.window.borrow(), *owner.desired.borrow(), owner.pending.get(), owner.publication_pending.get(),
                        owner.failed.get(), view.ivars().source_decorations_dirty.get());
                }
            }
            *owner.desired.borrow_mut() = viewport;
            owner // Failure/reverse movement in the same context cannot reconstruct a factory to obtain fresh global work or quota.
        } else {
            self.cancel_source_semantics();
            let request = key.document.upgrade().and_then(|document| {
                let request = document
                    .borrow()
                    .viewport_leaf_source_request(
                        viewport.clone(),
                        key.semantic_epoch,
                        key.selection,
                    );
                #[cfg(feature = "native-markdown-runtime-probe")]
                if let Err(error) = &request {
                    native_markdown_trace! {
                        eprintln!("native_source_semantics event=create_failed generation={} requested={viewport:?} error={error:?} formal=false", key.generation);
                    }
                }
                request.ok()
            });
            let failed = request.is_none();
            let owner = Rc::new(NativeMarkdownSourceOwner {
                key,
                view: Weak::from(view),
                request: RefCell::new(request),
                publications: RefCell::new(SourcePublications::default()),
                window: RefCell::new(viewport.clone()),
                desired: RefCell::new(viewport),
                pending: Cell::new(!failed),
                publication_pending: Cell::new(false),
                queued: Cell::new(false),
                failed: Cell::new(failed),
            });
            let previous = self
                .ivars()
                .source_semantics
                .replace(Some(Rc::clone(&owner)));
            drop(previous); // Publish the new slot and end the RefCell borrow before dropping the previous pure-Core owner.
            #[cfg(feature = "native-markdown-runtime-probe")]
            native_markdown_trace! {
                eprintln!("native_source_semantics event=prepare_new generation={} length={} selection={:?} semantic_epoch={} input_epoch={} window={:?} desired={:?} pending={} failed={} dirty={} formal=false",
                    owner.key.generation, owner.key.length, owner.key.selection, owner.key.semantic_epoch, owner.key.input_epoch,
                    *owner.window.borrow(), *owner.desired.borrow(), owner.pending.get(), owner.failed.get(),
                    view.ivars().source_decorations_dirty.get());
            }
            owner // Retain a payload-free failed owner so each draw cannot renew this same context's budget.
        };
        if !owner.pending.get() && !owner.failed.get() {
            self.forward_source_if_needed(&owner);
        }
        self.enqueue_source_step(&owner);
    }

    /// Reuse the committed result and original continuation only after its Native
    /// publication ended. Reverse gaps require an existing checkpoint, not a new scan from zero.
    fn forward_source_if_needed(&self, owner: &Rc<NativeMarkdownSourceOwner>) {
        if !self.owns_source(owner)
            || owner.pending.get()
            || owner.failed.get()
            || owner.publication_pending.get()
        {
            return;
        }
        let desired = owner.desired.borrow().clone();
        let window = owner.window.borrow().clone();
        if window.start <= desired.start && desired.end <= window.end {
            return; // The current publication already covers this request; repeated preparation adds no work.
        }
        let Some(previous) = owner
            .publications
            .borrow()
            .committed
            .as_ref()
            .map(Rc::clone)
        else {
            return;
        };
        let Some(document) = owner.key.document.upgrade() else {
            owner.stop();
            return;
        };
        let result = owner
            .request
            .borrow_mut()
            .as_mut()
            .ok_or(MarkdownTableRequestError::InvalidRequest)
            .and_then(|request| {
                request.continue_forward(
                    &document.borrow(),
                    &previous,
                    desired.clone(),
                    owner.key.authority(),
                )
            });
        match result {
            Ok(()) => {
                *owner.window.borrow_mut() = desired;
                owner.pending.set(true); // Original global/joint quota accepted one successor; this is not Ready publication.
            }
            Err(error @ MarkdownTableRequestError::CoverageRequiresCheckpoint) => {
                #[cfg(feature = "native-markdown-runtime-probe")]
                native_markdown_trace! {
                    eprintln!("native_source_semantics event=forward_failed generation={} window={window:?} desired={desired:?} error={error:?} formal=false", owner.key.generation);
                }
                let _ = error; // Keep this specific branch in nondiagnostic builds too.
                // Missing reverse checkpoints retain the existing TextKit feature
                // path and original request; never rebuild global work from zero.
            }
            Err(error) => {
                #[cfg(feature = "native-markdown-runtime-probe")]
                native_markdown_trace! {
                    eprintln!("native_source_semantics event=forward_failed generation={} window={window:?} desired={desired:?} error={error:?} formal=false", owner.key.generation);
                }
                let _ = error;
                owner.stop(); // Failure is sticky, with no fresh budget; original checks still govern old read-only publication access.
            }
        }
    }

    /// Queue at most one bounded continuation for this actual owner. Main-thread
    /// transfer retains weak Native/Core identities rather than their graphs.
    fn enqueue_source_step(&self, owner: &Rc<NativeMarkdownSourceOwner>) {
        if !self.owns_source(owner)
            || !owner.pending.get()
            || owner.failed.get()
            || owner.queued.replace(true)
        {
            return; // Repeated attributes/draw requests cannot add more than one queued continuation for this owner.
        }
        let Some(mtm) = MainThreadMarker::new() else {
            owner.queued.set(false);
            owner.stop();
            return;
        };
        let weak = MainThreadBound::new((Weak::from(self), Rc::downgrade(owner)), mtm);
        enqueue_native_markdown_main_task(move || {
            // SAFETY: The existing executor runs on the main queue. MainThreadBound
            // carries weak identities only, retaining no previous graph/Core payload.
            let mtm = unsafe { MainThreadMarker::new_unchecked() };
            let (storage, owner) = weak.get(mtm);
            let (Some(storage), Some(owner)) = (storage.load(), owner.upgrade()) else {
                return;
            };
            owner.queued.set(false);
            storage.advance_source_step(&owner);
        });
    }

    /// Advance the original Core request by one fixed quantum outside any Native
    /// borrow. Stage completed output, then leave Native publication to its settled executor.
    fn advance_source_step(&self, owner: &Rc<NativeMarkdownSourceOwner>) {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if !self.owns_source(owner) || !owner.pending.get() || owner.failed.get() {
            return; // An old queue callback cannot acquire a successor owner or revive a cancelled source.
        }
        let Some(view) = owner.view.load() else {
            owner.stop();
            return;
        };
        let original_storage = view.ivars().text_storage.load();
        let same_storage = original_storage.as_ref().is_some_and(|original| {
            std::ptr::eq::<PieceBackedMarkdownTextStorage>(&**original, self)
        });
        // Return the temporary weak-load claim, then revalidate the original owner;
        // another view/storage graph cannot become this source's publication target.
        drop(original_storage);
        if !same_storage || !self.owns_source(owner) || view.ivars().textkit_graph_teardown.get() {
            owner.stop();
            return; // Weak loads/releases may reenter Native; validate original authority before returning to Core.
        }
        let Some(document) = owner.key.document.upgrade() else {
            owner.stop();
            return;
        };
        let step = {
            let current = document.borrow();
            let mut request = owner.request.borrow_mut();
            request
                .as_mut()
                .ok_or(MarkdownTableRequestError::InvalidRequest)
                .and_then(|request| request.advance(&current, owner.key.authority(), SOURCE_STEP))
        }; // End every Core/RefCell borrow before Native publication or the next queue turn.
        match step {
            Ok(MarkdownViewportSourceStep::Progress { work }) => {
                if work.source_bytes == 0
                    && work.structural_transitions == 0
                    && work.metadata_bytes == 0
                {
                    owner.stop();
                    return; // No actual work means explicit termination, not queued polling until Ready.
                }
                self.enqueue_source_step(owner); // One bounded quantum per turn; cumulative fuel remains with the original Core request.
            }
            Ok(MarkdownViewportSourceStep::SourcesAvailable { .. }) => {
                let source = {
                    let current = document.borrow();
                    owner.request.borrow_mut().as_mut().and_then(|request| {
                        request
                            .take_sources_for(&current, owner.key.authority())
                            .ok()
                            .flatten()
                    })
                };
                let Some(source) = source else {
                    owner.stop();
                    return;
                };
                if !self.owns_source(owner) {
                    return;
                }
                #[cfg(feature = "native-markdown-runtime-probe")]
                native_markdown_trace! {
                    // Read this actual SourcesAvailable result's existing semantic
                    // tag only, without parsing/materializing text or advancing source work.
                    let state = document.borrow();
                    let semantic = match source.ordinary_semantics_for(&state, owner.key.authority()) {
                        Ok(MarkdownOrdinarySemanticState::SourcesOnly) => "sources_only",
                        Ok(MarkdownOrdinarySemanticState::Pending(_)) => "pending",
                        Ok(MarkdownOrdinarySemanticState::PlainReady(_)) => "plain_ready",
                        Ok(MarkdownOrdinarySemanticState::LeafPending(reason)) => {
                            eprintln!("native_source_semantics event=leaf_pending reason={reason:?} generation={} window={:?} formal=false", owner.key.generation, *owner.window.borrow());
                            "leaf_pending"
                        }
                        Ok(MarkdownOrdinarySemanticState::RootLeafReady(_)) => "root_leaf_ready",
                        Err(_) => "revoked",
                    };
                    eprintln!("native_source_semantics event=sources_available generation={} selection={:?} semantic_epoch={} input_epoch={} window={:?} desired={:?} semantic={semantic} dirty={} formal=false",
                        owner.key.generation, owner.key.selection, owner.key.semantic_epoch, owner.key.input_epoch,
                        *owner.window.borrow(), *owner.desired.borrow(), view.ivars().source_decorations_dirty.get());
                }
                let staged = { owner.publications.borrow_mut().stage(source) };
                if !staged {
                    owner.stop();
                    return; // Another completion cannot overwrite the unconsumed candidate or create a third live publication.
                }
                owner.pending.set(false);
                owner.publication_pending.set(true);
                // Only the original settled executor applies Native effects;
                // this callback cannot bypass the outer TextKit edit-transaction checks.
                if self.owns_source(owner) {
                    view.schedule_visible_table_layout_update();
                } // Do not forward an uncommitted candidate; Core's last output stays traceable to Native's current publication.
            }
            Err(error) => {
                #[cfg(feature = "native-markdown-runtime-probe")]
                native_markdown_trace! {
                    eprintln!("native_source_semantics event=advance_failed generation={} window={:?} desired={:?} error={error:?} formal=false", owner.key.generation, *owner.window.borrow(), *owner.desired.borrow());
                }
                let _ = error;
                owner.stop(); // Cancellation, source changes, and exhausted budgets release the continuation without retrying the same facts.
            }
        }
    }

    /// Commit original staged metadata only after foreground geometry/input settles.
    /// Notify bounded affected ranges, preserving authority across Native reentry.
    fn publish_source_ready(
        &self,
        owner: &Rc<NativeMarkdownSourceOwner>,
        view: &PieceBackedMarkdownTextView,
    ) {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if !self.owns_source(owner)
            || !publication_is_settled(view)
            || !owner.publication_pending.get()
        {
            return; // During Find/mode/table input, keep the candidate private and getters on the old committed semantics.
        }
        let ranges = owner.key.document.upgrade().and_then(|document| {
            let current = document.borrow();
            let publications = owner.publications.borrow();
            let source = publications.staged.as_ref()?;
            match source
                .ordinary_semantics_for(&current, owner.key.authority())
                .ok()?
            {
                MarkdownOrdinarySemanticState::RootLeafReady(ready) => Some(
                    ready
                        .covered_ranges_for(&current, owner.key.authority())
                        .ok()?
                        .cloned()
                        .collect::<Vec<_>>(),
                ),
                _ => Some(Vec::new()), // Sources/LeafPending supply actual table gaps, not fabricated ordinary-style notifications.
            }
        });
        let Some(ranges) = ranges else {
            owner.stop();
            return; // Failed original-publication validation cannot emit a successful empty Native notification.
        }; // At most 256 bounded intersections, with no copied text, runs, or source indexes.
        if !self.owns_source(owner) || !publication_is_settled(view) {
            return;
        }
        #[cfg(feature = "native-markdown-runtime-probe")]
        native_markdown_trace! {
            eprintln!("native_source_semantics event=publish_begin generation={} selection={:?} semantic_epoch={} input_epoch={} window={:?} desired={:?} ranges={ranges:?} dirty={} formal=false",
                owner.key.generation, owner.key.selection, owner.key.semantic_epoch, owner.key.input_epoch,
                *owner.window.borrow(), *owner.desired.borrow(), view.ivars().source_decorations_dirty.get());
        }
        let previous = owner.publications.borrow_mut().commit_staged();
        let Some(previous) = previous else {
            owner.stop();
            return; // Flags cannot replace an actual candidate; current getters cannot manufacture a successful publication.
        };
        owner.publication_pending.set(false);
        if !ranges.is_empty() {
            // Only actual RootLeaf style intersections change Native attributes.
            // Sources/LeafPending may add no-table/index proofs without ordinary
            // styles. An empty style notification must not revoke verified table
            // heights, temporarily restore literal getters, then rebuild identical
            // heights and disrupt just-completed viewport geometry.
            self.ivars().font_run_scratch.invalidate();
            self.advance_table_geometry_epoch(); // Actual code/rule styling can change preceding line heights, revoking old geometry.
            // Style/font/variant distinguish dictionary entries. Revoke in-flight
            // loans only, without additional Native destruction halfway through commit.
            self.ivars().attribute_cache.invalidate_loans();
        } // Zero style intersections still commit source indexes/budgets without inventing ordinary Ready or cancelling work.
        drop(previous); // Candidate/font keys committed together; release the old live inventory without a publication history chain.
        // Preview keys already include font epoch, synchronously revoking old
        // geometry/attempts before the scheduler prepares the new key. Do not call
        // release_preview_blocks here: its cancellation callback could erase new
        // reentrant work before the first Native notification. Emit one bounded
        // metadata envelope rather than sending an old batch after another Native
        // notification begins foreground work. Unknown interior ranges retain
        // their existing query path; the envelope grants no uncovered semantics or source read.
        let invalidated = ranges
            .first()
            .zip(ranges.last())
            .map(|(first, last)| first.start..last.end);
        self.notify_or_defer_attribute_range(invalidated);
        if !self.owns_source(owner) {
            return; // Native notification reentry ends this batch only, without reauthorizing a successor or rolling back committed metadata.
        }
        // Actual new coverage can resolve a prior Find query's unknown range.
        // Even equal query/viewport values cannot preserve its old empty geometry.
        // Rebuild bounded decorations through the original settled executor, never
        // layout, Find execution, or another source continuation on this publication stack.
        view.ivars().source_decorations_dirty.set(true);
        view.schedule_visible_table_layout_update(); // Continue the existing settled executor and Find/mode flows.
        if self.owns_source(owner) {
            // Publication covers only the original bounded viewport. Logical full-
            // document bounds cannot request a new full-body backing from AppKit.
            // The helper reads actual visibleRect and rejects closed/invalid geometry;
            // scrolling and later coverage remain with the same executor.
            view.invalidate_visible_body(); // Actual new publication authorizes drawing, not draw-driven Core advancement or range expansion.
        }
        if self.owns_source(owner) {
            self.forward_source_if_needed(owner);
            self.enqueue_source_step(owner);
        } // Only after Native commit may this unique new publication continue original global work and jointly charge old/new inventory.
        #[cfg(feature = "native-markdown-runtime-probe")]
        native_markdown_trace! {
            eprintln!("native_source_semantics event=publish_end generation={} current={} window={:?} desired={:?} pending={} publication_pending={} failed={} dirty={} formal=false",
                owner.key.generation, self.owns_source(owner), *owner.window.borrow(), *owner.desired.borrow(),
                owner.pending.get(), owner.publication_pending.get(), owner.failed.get(), view.ivars().source_decorations_dirty.get());
        }
    }

    /// The existing scheduler reads one publication flag; no actual new output
    /// means no additional background preparation opportunity.
    pub(in crate::native_surface) fn source_publication_needs_preparation(
        &self,
        view: &PieceBackedMarkdownTextView,
    ) -> bool {
        publication_is_settled(view)
            && self
                .current_source_owner()
                .is_some_and(|owner| owner.publication_pending.get())
    }

    /// Only the existing settled executor calls this entry. Late Core results
    /// neither notify Native nor advance source work from a drawing getter.
    pub(in crate::native_surface) fn finish_source_publication(
        &self,
        view: &PieceBackedMarkdownTextView,
    ) {
        let Some(owner) = self.current_source_owner() else {
            return;
        };
        if !owner.publication_pending.get() || !publication_is_settled(view) {
            return;
        }
        let Some(original) = owner.view.load() else {
            owner.stop();
            return;
        };
        if std::ptr::eq::<PieceBackedMarkdownTextView>(&*original, view) && self.owns_source(&owner)
        {
            self.publish_source_ready(&owner, view);
        } // A mismatched original weak view abandons this attempt; it cannot replace the owner's publication target.
    }

    /// Attribute/background getters perform pure queries. None retains the existing
    /// feature path without creating or advancing any source request.
    pub(in crate::native_surface) fn source_leaf_query(
        &self,
        index: usize,
    ) -> Option<omni_markdown_core::MarkdownPresentationQuery> {
        let owner = self.current_source_owner()?;
        let document = owner.key.document.upgrade()?;
        let current = document.borrow();
        let publications = owner.publications.borrow();
        let source = publications.committed.as_ref()?;
        let MarkdownOrdinarySemanticState::RootLeafReady(leaf) = source
            .ordinary_semantics_for(&current, owner.key.authority())
            .ok()?
        else {
            return None;
        };
        leaf.query_for(&current, owner.key.authority(), index)
            .ok()
            .flatten()
    }

    /// Prove absence of tables in this window only. Ordinary-gap inline styles
    /// remain unknown, granting no authority to hide or rewrite document text.
    pub(super) fn source_no_tables_proof(
        &self,
        window: &Range<usize>,
    ) -> Option<NativeMarkdownNoTablesProof> {
        let owner = self.current_source_owner()?;
        let document = owner.key.document.upgrade()?;
        let publications = owner.publications.borrow();
        let source = publications.committed.as_ref()?;
        let current = document.borrow();
        no_tables_covered(source, &current, owner.key.authority(), window).then(|| {
            NativeMarkdownNoTablesProof {
                source: Rc::downgrade(source),
                window: window.clone(),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Observe actual payload copying separately from parser input. RootLeaf may
    /// own normalized URI output while retaining no copied document text.
    struct SourceWork {
        copied_source_bytes: usize,
        parser_input_bytes: usize,
    }

    /// Keep the actual synthetic file/directory alive through its document and
    /// completed source request. These fixtures never open the user's vault.
    fn sources(
        source: &str,
    ) -> (
        tempfile::TempDir,
        MarkdownDocumentBuffer,
        VerifiedMarkdownViewportSources,
        SourceWork,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("原来源无表证明.md");
        std::fs::write(&path, source).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
        let authority = authority(&document);
        let mut request = document
            .viewport_leaf_source_request(
                0..document.utf16_len(),
                authority.context_revision,
                authority.selection,
            )
            .unwrap();
        let (sources, work) = complete_request(&document, authority, &mut request);
        (directory, document, sources, work)
    }

    /// Drive actual advance/take within a finite test loop. Forward continuation
    /// of this request cannot reconstruct global work to evade inventory charging.
    fn complete_request(
        document: &MarkdownDocumentBuffer,
        authority: MarkdownViewportSourceAuthority,
        request: &mut MarkdownViewportSourceRequest,
    ) -> (VerifiedMarkdownViewportSources, SourceWork) {
        for _ in 0..100_000 {
            let (MarkdownViewportSourceStep::Progress { work }
            | MarkdownViewportSourceStep::SourcesAvailable { work }) =
                request.advance(document, authority, SOURCE_STEP).unwrap();
            assert!(
                work.source_bytes <= SOURCE_STEP
                    && work.structural_transitions <= SOURCE_STEP
                    && work.metadata_bytes <= SOURCE_STEP
                    && work.copied_source_bytes <= SOURCE_STEP
                    && work.parser_input_bytes <= SOURCE_STEP
            );
            if let Some(sources) = request.take_sources_for(document, authority).unwrap() {
                // Tables copy bounded rows under existing budgets. RootLeaf admits
                // actual URI output fragments only; each case verifies their precise origin.
                let total = request.total_work();
                return (
                    sources,
                    SourceWork {
                        copied_source_bytes: total.copied_source_bytes,
                        parser_input_bytes: total.parser_input_bytes,
                    },
                );
            }
        }
        panic!("有限真实来源请求没有完成");
    }

    fn authority(document: &MarkdownDocumentBuffer) -> MarkdownViewportSourceAuthority {
        MarkdownViewportSourceAuthority {
            context_revision: 71,
            selection: document.selection(),
            cancelled: false,
        }
    }

    #[test]
    /// Real Core sources exercise this adapter's local stage/commit ownership.
    /// This does not invoke Native setters or prove a Native layout commit completed.
    fn staged_forward_result_keeps_old_committed_publication_until_native_boundary() {
        let source = "```text\n第一段😀\n```\n\n```text\n第二段中文\n```\n";
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("同请求候选提交.md");
        std::fs::write(&path, source).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
        let authority = authority(&document);
        let first_end = source[..source.find("\n\n").unwrap()]
            .encode_utf16()
            .count();
        let mut request = document
            .viewport_leaf_source_request(
                0..first_end,
                authority.context_revision,
                authority.selection,
            )
            .unwrap();
        let (first, first_work) = complete_request(&document, authority, &mut request);
        assert_eq!(first_work.copied_source_bytes, 0);
        assert_eq!(first_work.parser_input_bytes, 0);
        let mut publications = SourcePublications::default();
        assert!(publications.stage(first));
        assert!(
            publications.committed.is_none(),
            "首候选不能提前对getter可见"
        );
        assert!(publications.commit_staged().unwrap().is_none());
        let original = publications.committed.as_ref().unwrap();
        let original_weak = Rc::downgrade(original);
        let original_publication = Rc::as_ptr(original);

        // Continue through the same actual last publication. Core jointly reserves
        // metadata/run inventory for old output and its candidate before advancing.
        request
            .continue_forward(&document, original, 0..document.utf16_len(), authority)
            .unwrap();
        let (next, next_work) = complete_request(&document, authority, &mut request);
        assert_eq!(next_work.copied_source_bytes, 0);
        assert_eq!(next_work.parser_input_bytes, 0);
        assert!(publications.stage(next));
        assert_eq!(
            Rc::as_ptr(publications.committed.as_ref().unwrap()),
            original_publication,
            "未到Native提交边界，三个消费者仍只能取得旧committed"
        );
        assert!(no_tables_covered(
            publications.committed.as_ref().unwrap(),
            &document,
            authority,
            &(0..first_end)
        ));
        assert!(no_tables_covered(
            publications.staged.as_ref().unwrap(),
            &document,
            authority,
            &(0..document.utf16_len())
        ));

        let previous = publications.commit_staged().unwrap().unwrap();
        assert_eq!(Rc::as_ptr(&previous), original_publication);
        assert_ne!(
            Rc::as_ptr(publications.committed.as_ref().unwrap()),
            original_publication
        );
        drop(previous);
        assert!(
            original_weak.upgrade().is_none(),
            "提交后不保旧publication链"
        );
        assert!(publications.staged.is_none());
        assert!(
            publications.commit_staged().is_none(),
            "无候选不能补签一次提交"
        );
    }

    #[test]
    /// The same no-table window still needs independent source/semantic authority.
    /// Supported direct links obtain their own complete semantics; a source-only
    /// request cannot borrow a neighboring completed request's semantics through an equal gap.
    fn gap_proof_and_plain_semantics_keep_independent_authority() {
        // Retain the original long label, target, and code fixture. Direct links
        // are actually supported, so obsolete Unsupported behavior cannot remain
        // a negative authority expectation. Only normalized destination bytes may
        // be copied, never label contents or complete Root text.
        const DESTINATION: &str = "https://example.invalid";
        let source = format!(
            "普通正文\n\n[复杂{}仍未知](https://example.invalid)\n\n```text\n中文😀\n```\n\n尾部\n",
            "正文".repeat(400)
        );
        let (directory, document, sources, work) = sources(&source);
        let backing = document.original_backing_observer().unwrap();
        let authority = authority(&document);
        let window = 0..document.utf16_len();
        assert_eq!(work.copied_source_bytes, DESTINATION.len());
        assert_eq!(work.parser_input_bytes, 0);
        assert!(no_tables_covered(&sources, &document, authority, &window));
        assert!(!no_tables_covered(&sources, &document, authority, &(0..0)));
        assert!(!no_tables_covered(
            &sources,
            &document,
            authority,
            &(0..document.utf16_len() + 1)
        ));
        let MarkdownOrdinarySemanticState::RootLeafReady(ready) = sources
            .ordinary_semantics_for(&document, authority)
            .unwrap()
        else {
            panic!("普通闭合块和代码交集必须来自真实语义证明");
        };
        assert_eq!(
            ready
                .query_for(&document, authority, 0)
                .unwrap()
                .unwrap()
                .styles,
            omni_markdown_core::MarkdownPresentationStyles::default(),
            "普通块由原 Plain proof 授权，不能再把真实证明误判成仅有gap"
        );
        let link_open = source[..source.find("[复杂").unwrap()]
            .encode_utf16()
            .count();
        let label_start = link_open + 1;
        let label_end = source[..source.find("](https://example.invalid)").unwrap()]
            .encode_utf16()
            .count();
        let link = ready
            .link_at_for(&document, authority, label_start)
            .unwrap()
            .expect("原完整长标签必须取得自己的真实直接链接");
        assert_eq!(link.label_utf16_range, label_start..label_end);
        assert_eq!(link.destination.clone().into_string(), DESTINATION);
        for index in [link_open, label_end, label_end + 2] {
            assert!(
                ready
                    .link_at_for(&document, authority, index)
                    .unwrap()
                    .is_none()
            );
        } // Brackets/URI have marker styles but no clickable-label authority; the owner's envelope cannot broaden it.

        // An independent full parser consumes this bounded fixture only to verify
        // label/marker styles. It participates in neither paged production nor copy
        // accounting, and derives no expectations from candidate RootLeaf defaults.
        let mut original = MarkdownViewportPresentation::default();
        original.rebuild(0, window.clone(), &source).unwrap();
        for index in link_open..label_end + 2 + DESTINATION.len() + 1 {
            assert_eq!(
                ready
                    .query_for(&document, authority, index)
                    .unwrap()
                    .unwrap()
                    .styles,
                original.query(index).unwrap().styles,
                "原直接链接的 label/marker 样式必须符合完整 Parser：source={index}"
            );
        }
        let mut stale = authority;
        stale.context_revision += 1;
        assert!(ready.link_at_for(&document, stale, label_start).is_err());
        let cancelled = MarkdownViewportSourceAuthority {
            cancelled: true,
            ..authority
        };
        assert!(
            ready
                .link_at_for(&document, cancelled, label_start)
                .is_err()
        );
        let changed_selection = MarkdownViewportSourceAuthority {
            selection: MarkdownTextSelection::caret(1),
            ..authority
        };
        assert!(
            ready
                .link_at_for(&document, changed_selection, label_start)
                .is_err()
        );
        assert!(
            ready
                .link_at_for(&document, authority, label_start)
                .unwrap()
                .is_some()
        );

        // The source-only factory has the same actual no-table fact but no Ready
        // semantics. Those two capabilities cannot be converted into each other.
        let mut request = document
            .viewport_source_request(
                window.clone(),
                authority.context_revision,
                authority.selection,
            )
            .unwrap();
        let (gap_only, work) = complete_request(&document, authority, &mut request);
        assert!(no_tables_covered(&gap_only, &document, authority, &window));
        assert!(matches!(
            gap_only
                .ordinary_semantics_for(&document, authority)
                .unwrap(),
            MarkdownOrdinarySemanticState::SourcesOnly
        ));
        assert_eq!(work.copied_source_bytes, 0);
        assert_eq!(work.parser_input_bytes, 0);
        assert_eq!(document.generation(), 0);
        assert_eq!(document.selection(), authority.selection);
        assert!(!document.can_undo() && !document.can_redo() && !document.is_dirty());
        assert!(document.original_file_identity_is_unchanged());
        assert_eq!(
            std::fs::read(directory.path().join("原来源无表证明.md")).unwrap(),
            source.as_bytes()
        );
        drop((gap_only, request, sources, document));
        assert_eq!(backing.mapped_bytes(), 0);
        assert_eq!(backing.managed_resident_bytes(), 0);
    }

    #[test]
    fn actual_table_cannot_be_hidden_by_gap_envelope_or_empty_snapshot() {
        let source = "前言\n\n| 左 | 右 |\n| --- | --- |\n| 中文 | 😀 |\n\n尾部\n";
        let (_directory, document, sources, _) = sources(source);
        // Only actual structural coverage grants no-table authority. Structural
        // rows may publish without copied text/full-window parsing; verify that
        // tables remain present, independent of the particular copy/parser implementation.
        assert!(!no_tables_covered(
            &sources,
            &document,
            authority(&document),
            &(0..document.utf16_len())
        ));
        assert!(no_tables_covered(
            &sources,
            &document,
            authority(&document),
            &(0..1)
        ));
        let table = source[..source.find('|').unwrap()].encode_utf16().count();
        assert!(!no_tables_covered(
            &sources,
            &document,
            authority(&document),
            &(table..table + 1)
        ));
        // Source authority must actually own this row without materializing all
        // cell semantics for the independent full-window reference backend first.
        assert!(
            sources
                .table_row_at_for(&document, authority(&document), table)
                .unwrap()
                .is_some(),
            "必须取得真实结构表行，不能把未知来源当作表格覆盖"
        );
    }

    #[test]
    /// Use actual separate files and a real edit/Undo to prove source identity and
    /// revision revocation; equal restored bytes cannot renew an old publication.
    fn no_table_fact_rejects_cancelled_context_foreign_root_and_post_edit_undo() {
        let source = "普通中文\n\n***\n";
        let (_directory, mut document, sources, work) = sources(source);
        assert_eq!(work.copied_source_bytes, 0);
        assert_eq!(work.parser_input_bytes, 0);
        let window = 0..document.utf16_len();
        let mut cancelled = authority(&document);
        cancelled.cancelled = true;
        assert!(!no_tables_covered(&sources, &document, cancelled, &window));
        let mut stale = authority(&document);
        stale.context_revision += 1;
        assert!(!no_tables_covered(&sources, &document, stale, &window));
        let (_other_directory, other, _, _) = self::sources(source);
        assert!(!no_tables_covered(
            &sources,
            &other,
            authority(&other),
            &window
        ));
        document
            .insert_utf16(0, "改", MarkdownTextSelection::caret(1))
            .unwrap();
        document.undo().unwrap();
        assert!(!no_tables_covered(
            &sources,
            &document,
            authority(&document),
            &window
        ));
    }
}
