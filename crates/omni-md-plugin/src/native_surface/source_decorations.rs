//! Find, bracket, and control-character cues share the settled reference
//! TextKit viewport and its existing bounded decoration vector.
//!
//! Painting consumes prepared Copy rectangles only; it neither reads Source nor
//! constructs another document or forces offscreen layout.

use super::*;
use objc2_app_kit::{
    NSAttributedStringNSStringDrawing, NSTextLayoutManagerSegmentOptions,
    NSTextLayoutManagerSegmentType,
};
use omni_markdown_core::markdown_brackets::{MarkdownBracketQuery, markdown_bracket_match};

// The reference graph uses the same visible-Find limits, owned by FindBar;
// duplicating numerical policies here would let the two paths drift.
use super::find_bar::{MAX_DECORATION_VIEWPORT_UNITS, MAX_SPECIAL_CHARACTER_MARKERS};

/// The original vector owns finite cue/Find geometry. Batches retain weak
/// authority tickets rather than document text or Native layout objects.
#[derive(Clone)]
pub(super) enum NativeMarkdownDecorationGeometry {
    Source {
        frame: NSRect,
        matched: Option<bool>,
    },
    Find(Rc<NativeMarkdownFindHighlightBatch>),
    PreviewVisibility(Rc<NativeMarkdownPreviewVisibility>),
}

/// Visible style facts belong to this decoration batch alone. It retains no
/// parser snapshot, document payload, Native object, or Find query.
pub(super) struct NativeMarkdownPreviewVisibility {
    document: WeakMarkdownDocument,
    selection: MarkdownTextSelection,
    semantic_epoch: u64,
    storage_input: u64,
    view_input: u64,
    folds: u64,
    font: u64,
    key: NativeMarkdownTableLayoutRequestKey,
    runs: Vec<omni_markdown_core::MarkdownPresentationQuery>,
}

impl NativeMarkdownPreviewVisibility {
    /// Semantic visibility is independent of Find intent. Source, selection,
    /// mode, fold, font, and input changes still revoke this original proof.
    fn current(
        &self,
        view: &PieceBackedMarkdownTextView,
        storage: &PieceBackedMarkdownTextStorage,
    ) -> bool {
        if view.ivars().textkit_graph_teardown.get()
            || view.ivars().textkit_input_depth.get() != 0
            || !storage.ivars().accepts_edits.get()
            || storage.ivars().composition_active.get()
            || storage.presentation_mode() != MarkdownPresentationMode::InlinePreview
            || storage.ivars().presentation_semantic_epoch.get() != Some(self.semantic_epoch)
            || storage.ivars().input_transaction_epoch.get() != self.storage_input
            || storage.ivars().font_run_scratch.epoch() != self.font
            || storage.ivars().table_geometry_epoch.get() != self.key.table_geometry_epoch
            || storage.ivars().table_projection_width_key.get() != self.key.text_view_width
            || view.ivars().table_input_intent_epoch.get() != self.view_input
            || view.ivars().folds.revision() != self.folds
            || !WeakMarkdownDocument::ptr_eq(&self.document, &storage.ivars().document.borrow())
        {
            // Equal generation numbers cannot authorize another document or
            // geometry; retain the original weak identity and every input epoch.
            return false;
        }
        self.document.upgrade().is_some_and(|document| {
            // This is a Rust-only authority check. Release the canonical borrow
            // before callers resume any reentrant AppKit operation.
            let source_matches = view.ivars().folds.matches_document(&document);
            let document = document.borrow();
            source_matches
                && document.generation() == self.key.document_generation
                && document.utf16_len() == self.key.document_length
                && document.selection() == self.selection
        })
    }

    /// Pair the actual clip, column width, and source window before publication;
    /// checking existing scalar facts does not reenter Native geometry.
    fn covers(
        &self,
        view: &PieceBackedMarkdownTextView,
        storage: &PieceBackedMarkdownTextStorage,
        visible: NSRect,
        source: &std::ops::Range<usize>,
    ) -> bool {
        self.current(view, storage)
            && self.key.visible_origin_x == native_markdown_layout_metric(visible.origin.x)
            && self.key.visible_origin_y == native_markdown_layout_metric(visible.origin.y)
            && self.key.visible_width == native_markdown_layout_metric(visible.size.width)
            && self.key.visible_height == native_markdown_layout_metric(visible.size.height)
            && self.key.native_viewport_start <= source.start
            && source.end <= self.key.native_viewport_end
    }

    /// Query only admitted, clipped runs. A gap or a point outside the envelope
    /// has no implicit Plain-style visibility authority.
    fn query(&self, index: usize) -> Option<omni_markdown_core::MarkdownPresentationQuery> {
        let ordinal = self
            .runs
            .partition_point(|run| run.utf16_range.end <= index);
        self.runs
            .get(ordinal)
            .filter(|run| run.utf16_range.contains(&index))
            .cloned()
    }
}

/// Clip metadata from the original complete parse within the shared run/window
/// limits. Neither Source bytes nor the complete snapshot enter the result.
fn clipped_preview_visibility_runs(
    snapshot: &omni_markdown_core::MarkdownViewportPresentationSnapshot,
    window: std::ops::Range<usize>,
) -> Option<Vec<omni_markdown_core::MarkdownPresentationQuery>> {
    if window.is_empty()
        || window.len() > MAX_DECORATION_VIEWPORT_UNITS
        || snapshot.utf16_window().start > window.start
        || window.end > snapshot.utf16_window().end
    {
        // A proof must cover an admitted finite viewport inside this snapshot;
        // unknown or oversized text cannot be treated as established styles.
        return None;
    }
    let mut cursor = window.start;
    let mut runs = Vec::new();
    while cursor < window.end {
        if runs.len() == MAX_SPECIAL_CHARACTER_MARKERS {
            // A partial prefix cannot certify the whole requested window. The
            // real RootLeaf continuation may establish it in a later publication.
            return None;
        }
        let mut query = snapshot.query(cursor)?;
        query.utf16_range = cursor..query.utf16_range.end.min(window.end);
        cursor = query.utf16_range.end;
        // Keep only range/style facts; URLs, document text, and the original
        // parser directory remain with their existing owners.
        runs.push(query);
    }
    Some(runs)
}

impl PartialEq for NativeMarkdownDecorationGeometry {
    /// Redraw when actual geometry or original Find authority changes; separate
    /// cue roles cannot reuse each other's pixels.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Source {
                    frame: left,
                    matched: left_match,
                },
                Self::Source {
                    frame: right,
                    matched: right_match,
                },
            ) => left == right && left_match == right_match,
            (Self::Find(left), Self::Find(right)) => left.same_geometry(right),
            (Self::PreviewVisibility(left), Self::PreviewVisibility(right)) => {
                // Ordinary query changes can reuse this same visible proof;
                // rereading equivalent metadata alone does not require paint.
                Rc::ptr_eq(left, right)
            }
            _ => false,
        }
    }
}

/// Match the preserved special-character predicate while excluding canonical CR/LF line separators.
fn is_visible_special_character(unit: u16) -> bool {
    matches!(unit, 0x0000..=0x0008 | 0x000b..=0x000c | 0x000e..=0x001f | 0x007f..=0x009f | 0x00ad | 0x061c | 0x200b | 0x200e..=0x200f | 0x2028..=0x2029 | 0x202d..=0x202e | 0x2066..=0x2067 | 0x2069 | 0xfeff | 0xfff9..=0xfffc)
}

impl PieceBackedMarkdownTextView {
    /// Read actual TextKit segments for the admitted original range. Every
    /// reentrant Native step remains guarded by the same original ticket.
    fn source_decoration_frames(
        &self,
        source: std::ops::Range<usize>,
        maximum_frames: usize,
        current: &impl Fn() -> bool,
    ) -> Option<Vec<NSRect>> {
        let manager =
            native_markdown_table_intent_step(current, || self.textLayoutManager()).ok()??;
        let content =
            native_markdown_table_intent_step(current, || manager.textContentManager()).ok()??;
        if native_markdown_table_intent_step(current, || content.hasEditingTransaction()).ok()? {
            // An active editing transaction supplies no settled glyph authority;
            // decorations must not force layout to manufacture it.
            return None;
        }
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
        let document_range =
            native_markdown_table_intent_step(current, || provider.documentRange()).ok()?;
        let start =
            native_markdown_table_intent_step(current, || document_range.location()).ok()?;
        let (from, to) = (
            isize::try_from(source.start).ok()?,
            isize::try_from(source.end).ok()?,
        );
        let location = native_markdown_table_intent_step(current, || {
            content.locationFromLocation_withOffset(&start, from)
        })
        .ok()??;
        let end = native_markdown_table_intent_step(current, || {
            content.locationFromLocation_withOffset(&start, to)
        })
        .ok()??;
        let range = native_markdown_table_intent_step(current, || {
            NSTextRange::initWithLocation_endLocation(NSTextRange::alloc(), &location, Some(&end))
        })
        .ok()??;
        let origin =
            native_markdown_table_intent_step(current, || self.textContainerOrigin()).ok()?;
        let frames = RefCell::new(Vec::new());
        {
            let block = block2::StackBlock::new(
                |_range: *mut NSTextRange,
                 frame: NSRect,
                 _baseline: f64,
                 _container: NonNull<NSTextContainer>| {
                    if !current() || frames.borrow().len() >= maximum_frames {
                        // Cancellation or the finite frame cap ends enumeration;
                        // the synchronous callback retains no Native object.
                        return objc2::runtime::Bool::NO;
                    }
                    if [
                        frame.origin.x,
                        frame.origin.y,
                        frame.size.width,
                        frame.size.height,
                    ]
                    .into_iter()
                    .all(f64::is_finite)
                        && frame.size.width >= 0.0
                        && frame.size.height >= 0.0
                    {
                        // Find uses actual glyph rectangles. The minimum width
                        // for control cues must not enlarge ordinary matches.
                        frames.borrow_mut().push(NSRect::new(
                            NSPoint::new(frame.origin.x + origin.x, frame.origin.y + origin.y),
                            frame.size,
                        ));
                    }
                    objc2::runtime::Bool::YES
                },
            );
            native_markdown_table_intent_step(current, || {
                manager.enumerateTextSegmentsInRange_type_options_usingBlock(
                    &range,
                    NSTextLayoutManagerSegmentType::Standard,
                    NSTextLayoutManagerSegmentOptions::RangeNotRequired,
                    &block,
                );
            })
            .ok()?;
        } // End the synchronous block's lexical scope before returning Copy frames.
        Some(frames.into_inner())
    }

    /// Capture current visible styles before the first document Native getter.
    /// Later attribute-window eviction does not erase these finite facts.
    fn capture_preview_visibility(
        &self,
        storage: &PieceBackedMarkdownTextStorage,
    ) -> Option<Rc<NativeMarkdownPreviewVisibility>> {
        let (key, identity) = self
            .ivars()
            .visible_table_layout_cache
            .borrow()
            .as_ref()
            .map(|cache| (cache.request_key, cache.document_identity.clone()))?;
        if key.presentation_mode != MarkdownPresentationMode::InlinePreview
            || !WeakMarkdownDocument::ptr_eq(&identity, &storage.ivars().document.borrow())
        {
            // Only this document's published preview viewport can authorize the
            // proof; an old parser envelope cannot stand in for a real viewport.
            return None;
        }
        let previous =
            self.ivars()
                .source_decorations
                .borrow()
                .iter()
                .find_map(|item| match item {
                    NativeMarkdownDecorationGeometry::PreviewVisibility(proof)
                        if proof.key == key && proof.current(self, storage) =>
                    {
                        Some(Rc::clone(proof))
                    }
                    // Bracket/Find geometry and expired proofs grant no semantic
                    // authority for a newly captured visible window.
                    _ => None,
                });
        if previous.is_some() {
            // Reuse the same finite metadata for this actual viewport rather
            // than allocating it again for an ordinary query change.
            return previous;
        }
        let document = identity.upgrade()?;
        let selection = {
            let document = document.borrow();
            if document.generation() != key.document_generation
                || document.utf16_len() != key.document_length
            {
                // A stale cache cannot acquire authority for its old key from a
                // newer canonical document or parser result.
                return None;
            }
            document.selection()
        };
        let semantic_epoch = storage.ivars().presentation_semantic_epoch.get()?;
        let runs = {
            let presentation = storage.ivars().presentation.borrow();
            let snapshot = presentation.semantic_snapshot()?;
            if snapshot.generation() != key.document_generation {
                // SourceFallback has no complete snapshot; a complete directory
                // from another generation is equally unauthorized here.
                return None;
            }
            clipped_preview_visibility_runs(
                &snapshot,
                key.native_viewport_start..key.native_viewport_end,
            )?
        }; // Return the parser snapshot after Rust-only clipping, before any Native getter.
        let proof = Rc::new(NativeMarkdownPreviewVisibility {
            document: identity,
            selection,
            semantic_epoch,
            storage_input: storage.ivars().input_transaction_epoch.get(),
            view_input: self.ivars().table_input_intent_epoch.get(),
            folds: self.ivars().folds.revision(),
            font: storage.ivars().font_run_scratch.epoch(),
            key,
            runs,
        });
        proof.current(self, storage).then_some(proof)
    }

    /// Prepare matching/filtering/segment geometry in the existing settled
    /// refresh. Painting later consumes this batch without repeating that work.
    fn prepare_find_highlight_geometry(
        &self,
        storage: &PieceBackedMarkdownTextStorage,
        current: &impl Fn() -> bool,
        visible: NSRect,
        source_range: std::ops::Range<usize>,
        preview_visibility: Option<&NativeMarkdownPreviewVisibility>,
    ) -> Option<NativeMarkdownFindHighlightBatch> {
        // Preserve the requested range for geometry verification without copying
        // either the query text or document payload.
        let requested_source = source_range.clone();
        let controller =
            native_markdown_table_intent_step(current, || self.ivars().controller.get()?.load())
                .ok()??;
        let bar = controller.current_find_input_bar()?;
        let feedback = NativeMarkdownFindViewportFeedback::capture(
            &bar,
            &controller,
            controller.find_input_intent(),
            false,
        )?;
        let prepared =
            feedback.prepare_highlights(&bar, self, storage, &controller, visible, source_range)?;
        let mut batch = prepared.batch;
        let width = native_markdown_table_intent_step(current, || self.bounds().size.width).ok()?;
        let original = || current() && batch.current(self, storage, &controller, visible, width);
        let preview_visibility = preview_visibility.filter(|proof| {
            proof.covers(self, storage, visible, &requested_source)
                && proof.key.text_view_width == native_markdown_layout_metric(width)
        }); // Recheck original geometry after Native getters; old metadata cannot authorize a new width.
        let mut frames = Vec::new();
        for source in prepared.ranges {
            if !original() {
                // Reentry revokes the whole old batch; publishing a partial set
                // would mix coordinates from different inputs.
                return None;
            }
            if frames.len() == MAX_SPECIAL_CHARACTER_MARKERS {
                // Multiline hits share the actual frame cap; a long query must
                // not create a whole-paragraph geometry cache.
                break;
            }
            if !self.find_source_is_visible(storage, &source, preview_visibility) {
                // Custom tables, concealed markers, and folds have no standard
                // TextKit glyph authority; do not highlight their placeholders.
                continue;
            }
            let Some(found) = self.source_decoration_frames(
                source,
                MAX_SPECIAL_CHARACTER_MARKERS - frames.len(),
                &original,
            ) else {
                // Missing actual Native geometry is not a screen coordinate;
                // discard this batch rather than drawing from Source offsets.
                return None;
            };
            for frame in found {
                if frame.size.width > 0.0
                    && frame.size.height > 0.0
                    && NSIntersectsRect(frame, visible)
                {
                    frames.push(frame); // The bounded vector owns Copy frames, not TextKit segments.
                }
            }
        }
        if !original() || frames.is_empty() {
            return None;
        }
        batch.frames = frames;
        Some(batch)
    }

    /// Preview requires visibility proof for every semantic run. Unknown styles
    /// do not trigger parsing; literal Source mode keeps original text visible.
    fn find_source_is_visible(
        &self,
        storage: &PieceBackedMarkdownTextStorage,
        source: &std::ops::Range<usize>,
        preview_visibility: Option<&NativeMarkdownPreviewVisibility>,
    ) -> bool {
        if self.ivars().folds.borrow().iter().any(|fold| {
            source.start < fold.hidden_utf16_range.end && source.end > fold.hidden_utf16_range.start
        }) {
            return false; // A hit crossing a fold has no contiguous visible glyph range.
        }
        if storage.presentation_mode() == MarkdownPresentationMode::Source {
            return true;
        }
        let generation = self.ivars().folds.generation.get();
        let mut cursor = source.start;
        while cursor < source.end {
            let query = storage
                .source_leaf_query(cursor)
                .or_else(|| {
                    preview_visibility
                        .filter(|proof| proof.current(self, storage))
                        .and_then(|proof| proof.query(cursor))
                })
                .or_else(|| {
                    let presentation = storage.ivars().presentation.borrow();
                    let snapshot = presentation.semantic_snapshot()?;
                    (snapshot.generation() == generation)
                        .then(|| snapshot.query(cursor))
                        .flatten()
                }); // Only complete parse/RootLeaf facts authorize visibility; fallback empty styles do not.
            let Some(query) = query else {
                return false; // An unpublished semantic gap cannot default to visible Plain text.
            };
            if [
                MarkdownPresentationStyles::MARKER,
                MarkdownPresentationStyles::HIDDEN_MARKER,
                MarkdownPresentationStyles::TABLE,
                MarkdownPresentationStyles::TABLE_ROW_PLACEHOLDER,
            ]
            .into_iter()
            .any(|style| query.styles.contains(style))
            {
                // Tables use their own CTFrame and markers are concealed. Their
                // actual source-map geometry must authorize separate highlights.
                return false;
            }
            // Core supplies a nonempty run containing this scalar; advance by
            // that established range without rereading Source.
            cursor = query.utf16_range.end.min(source.end);
        }
        true
    }

    /// The existing coalesced executor prepares cues after the real editing
    /// transaction. One original ticket governs every Native stage in this refresh.
    pub(super) fn refresh_source_decorations(&self) -> bool {
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if self.ivars().textkit_graph_teardown.get() {
            // Late calls must acquire neither the canonical owner nor new
            // geometry capacity after teardown has released the graph.
            return false;
        }
        let Some(storage) = self.ivars().text_storage.load() else {
            // Teardown releases old cue geometry without replacing its expired document owner.
            let changed = !self.ivars().source_decorations.borrow().is_empty();
            self.ivars().source_decorations.borrow_mut().clear();
            return changed;
        };
        let Some(document) = storage.document() else {
            return false; // A detached Native shell cannot rebuild document cues or Find geometry.
        };
        let (generation, length) = {
            // End canonical borrows before viewport/segment calls can reenter native text callbacks.
            let document = document.borrow();
            (document.generation(), document.utf16_len())
        };
        let preview_visibility = self.capture_preview_visibility(&storage);
        let selection = self.selectedRange();
        let mut positions = Vec::with_capacity(MAX_SPECIAL_CHARACTER_MARKERS + 2);
        if selection.length == 0 && selection.location <= length {
            // Parsing/classification stays portable and bounded; release the canonical borrow before native geometry.
            let query = markdown_bracket_match(&document.borrow(), selection.location);
            match query {
                // Only a proven admitted token context supplies visible bracket handles.
                Ok(MarkdownBracketQuery::Match(found)) => {
                    positions.push((found.start_utf16, Some(found.matched)));
                    if let Some(end) = found.end_utf16 {
                        // Complete matching or mismatched pairs display both canonical source handles.
                        positions.push((end, Some(found.matched)));
                    }
                }
                // Ordinary carets have no nearby bracket cue and require no native segment queries.
                Ok(MarkdownBracketQuery::NoMatch) => {}
                // A bounded search cannot honestly label an unseen partner as unmatched.
                Ok(MarkdownBracketQuery::BudgetExhausted) => {}
                // Unestablished parser context is omitted rather than assigning a misleading cross-token pair.
                Ok(MarkdownBracketQuery::ContextUnavailable) => {}
                // Invalid/stale canonical coordinates produce no partial decoration or document mutation.
                Err(_) => {}
            }
        }
        let Some((visible, paragraph_window)) = self.visible_utf16_window(length) else {
            // Require a real clip/range pair established after viewport layout;
            // a zero-range substitute grants no matching or paint authority.
            return false;
        };
        let viewport = match (
            self.insertion_offset_at_view_point(NSPoint::new(visible.origin.x, visible.origin.y)),
            self.insertion_offset_at_view_point(NSPoint::new(
                visible.origin.x + visible.size.width - 1.0,
                visible.origin.y + visible.size.height - 1.0,
            )),
        ) {
            // Point navigation narrows giant wrapped paragraphs to the actual visible character interval.
            (Some(start), Some(end)) => {
                start.min(end)..start.max(end).saturating_add(1).min(length)
            }
            // Unavailable transient point geometry keeps the existing bounded paragraph fallback.
            _ => paragraph_window,
        };
        if !self.projection_generation_is_current(generation) {
            // Viewport positioning may reenter editing. Reject its old range
            // before further canonical scans or Native segment queries.
            return false;
        }
        let mode = storage.presentation_mode();
        let fold_revision = self.ivars().folds.revision();
        let input_epoch = self.ivars().table_input_intent_epoch.get();
        let font_epoch = storage.ivars().font_run_scratch.epoch();
        let identity = Rc::downgrade(&document);
        let current = || {
            !self.ivars().textkit_graph_teardown.get()
                && document.borrow().generation() == generation
                && self.ivars().folds.matches_document(&document)
                && self.ivars().folds.revision() == fold_revision
                && self.ivars().table_input_intent_epoch.get() == input_epoch
                && storage.presentation_mode() == mode
                && storage.ivars().font_run_scratch.epoch() == font_epoch
                && WeakMarkdownDocument::ptr_eq(&identity, &storage.ivars().document.borrow())
        };
        let folds = self.ivars().folds.borrow().clone();
        let bracket_count = positions.len();
        let mut cursor = viewport.start;
        let mut scanned = 0_usize;
        let mut scratch = [0_u16; 512];
        while cursor < viewport.end
            && scanned < MAX_DECORATION_VIEWPORT_UNITS
            && positions.len() - bracket_count < MAX_SPECIAL_CHARACTER_MARKERS
        {
            // Skip collapsed source intervals by metadata; they consume no copy/scan budget or native query.
            if let Some(hidden) = folds
                .iter()
                .find(|fold| fold.hidden_utf16_range.contains(&cursor))
            {
                // The canonical hidden text remains saved/searchable but contributes no visible marker geometry.
                cursor = hidden.hidden_utf16_range.end;
                continue;
            }
            let next_hidden = folds
                .iter()
                .filter(|fold| fold.hidden_utf16_range.start > cursor)
                .map(|fold| fold.hidden_utf16_range.start)
                .min()
                .unwrap_or(viewport.end);
            let count = scratch
                .len()
                .min(viewport.end - cursor)
                .min(next_hidden - cursor)
                .min(MAX_DECORATION_VIEWPORT_UNITS - scanned);
            if document
                .borrow()
                .copy_utf16_code_units(cursor, &mut scratch[..count])
                .is_err()
            {
                // A changed/disconnected canonical interval contributes no partially initialized control cues.
                break;
            }
            for (index, unit) in scratch[..count].iter().enumerate() {
                // Every inspected control comes from the fixed visible scratch and finite retained marker cap.
                if positions.len() - bracket_count >= MAX_SPECIAL_CHARACTER_MARKERS {
                    // Further visible controls are discovered only after a future viewport/selection request.
                    break;
                }
                if is_visible_special_character(*unit) {
                    // A dot identifies a control without changing or retaining the underlying source code unit.
                    positions.push((cursor + index..cursor + index + 1, None));
                }
            }
            cursor += count;
            scanned += count;
        }
        let maximum_geometry = MAX_SPECIAL_CHARACTER_MARKERS + 2;
        let mut geometry = Vec::with_capacity(positions.len());
        let preview_visibility =
            preview_visibility.filter(|proof| proof.covers(self, &storage, visible, &viewport));
        for (source, matched) in positions {
            if !current() {
                // A preceding segment query may change generation through
                // reentry; stop before querying another expired position.
                return false;
            }
            if geometry.len() >= maximum_geometry {
                // A wrapped handle cannot expand persistent native geometry beyond the shared finite viewport budget.
                break;
            }
            if source.start >= viewport.end
                || source.end <= viewport.start
                || folds.iter().any(|fold| {
                    // A cue crossing omitted canonical text cannot claim one contiguous visible native handle.
                    source.start < fold.hidden_utf16_range.end
                        && source.end > fold.hidden_utf16_range.start
                })
            {
                // Offscreen partners and deliberately hidden source never force native layout or scrolling.
                continue;
            }
            let visible_source = source.start.max(viewport.start)..source.end.min(viewport.end);
            let Some(frames) = self.source_decoration_frames(
                visible_source,
                maximum_geometry - geometry.len(),
                &current,
            ) else {
                continue;
            };
            for frame in frames {
                // Preserve the original minimum cue size. Expand a zero-width
                // control to its visible dot before testing the viewport clip.
                let frame = NSRect::new(
                    frame.origin,
                    NSSize::new(frame.size.width.max(7.0), frame.size.height.max(14.0)),
                );
                if NSIntersectsRect(frame, visible) {
                    // Retain copied rectangles only for actual visible handle segments and control positions.
                    geometry.push(NativeMarkdownDecorationGeometry::Source { frame, matched });
                }
            }
        }
        if !current() {
            // TextKit may reenter synchronously; invalid geometry is dropped
            // locally after the Native calls, never installed over newer state.
            return false;
        }
        if let Some(proof) = preview_visibility
            .as_ref()
            .filter(|proof| proof.covers(self, &storage, visible, &viewport))
        {
            // Keep at most one visible proof. Native glyph/attribute getters may
            // evict the parser window without revoking this original semantic fact.
            geometry.push(NativeMarkdownDecorationGeometry::PreviewVisibility(
                Rc::clone(proof),
            ));
        }
        if let Some(find) = self.prepare_find_highlight_geometry(
            &storage,
            &current,
            visible,
            viewport,
            preview_visibility.as_deref(),
        ) {
            geometry.push(NativeMarkdownDecorationGeometry::Find(Rc::new(find)));
        }
        if !current() {
            // Find's field/segment getters may also reenter; the old complete
            // batch must not replace decorations belonging to successor input.
            return false;
        }
        let changed = *self.ivars().source_decorations.borrow() != geometry;
        *self.ivars().source_decorations.borrow_mut() = geometry;
        self.ivars().source_decoration_generation.set(generation);
        self.ivars().source_decorations_dirty.set(false);
        changed
    }

    /// Paint prepared bracket/control/Find frames without enumerating document
    /// elements or changing the user's selection and history.
    pub(super) fn draw_source_decorations(&self, dirty: NSRect) {
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if self.ivars().textkit_graph_teardown.get() {
            // Standalone draw calls also lose access to old cues and the
            // canonical owner once teardown begins.
            return;
        }
        if self.ivars().textkit_input_depth.get() != 0
            || self
                .ivars()
                .marked_text_range
                .get()
                .is_some_and(|range| range.length != 0)
        {
            // Composition keeps native provisional glyphs unobscured and never paints stale cue positions.
            return;
        }
        let storage = self.ivars().text_storage.load();
        let generation = storage
            .as_ref()
            .and_then(|storage| storage.document())
            .map(|document| document.borrow().generation());
        if generation != Some(self.ivars().source_decoration_generation.get()) {
            // Canonical edits invalidate all old rectangles before the next queued layout transaction.
            return;
        }
        let geometry = self.ivars().source_decorations.borrow().clone();
        NSGraphicsContext::saveGraphicsState_class();
        for cue in geometry {
            let NativeMarkdownDecorationGeometry::Source { frame, matched } = cue else {
                if let NativeMarkdownDecorationGeometry::Find(batch) = cue
                    && let Some(storage) = storage.as_ref()
                {
                    self.draw_find_highlight_batch(&batch, storage, dirty);
                }
                // Find shares the current graphics context; no extra layer,
                // Source acquisition, or layout work is introduced during paint.
                continue;
            };
            // Copying the bounded cache ends state borrows before native drawing can reenter AppKit.
            if !NSIntersectsRect(frame, dirty) {
                // Undamaged viewport cues need no native draw work.
                continue;
            }
            let color = match matched {
                // Balanced pairs use an unobtrusive green outline around their existing source glyphs.
                Some(true) => NSColor::colorWithSRGBRed_green_blue_alpha(0.4, 0.8, 0.6, 0.7),
                // Invalid/unclosed pairs retain a visible red outline without changing editable text.
                Some(false) => NSColor::systemRedColor(),
                // Special control characters use the same visible warning tint as the preserved editor.
                None => NSColor::systemRedColor(),
            };
            color.setStroke();
            if matched.is_some() {
                // Outlines preserve the native glyph, selection and accessibility source value.
                NSBezierPath::strokeRect(NSInsetRect(frame, -1.0, -1.0));
            } else {
                // A red dot reveals invisible control presence while its canonical code unit remains unchanged.
                let font = NSFont::systemFontOfSize(12.0);
                // SAFETY: these are immutable AppKit exported font/color keys with process lifetime.
                let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
                let values: [&AnyObject; 2] = [&font, &color];
                let attributes = MarkdownTextAttributes::from_slices(&keys, &values);
                // SAFETY: the complete immutable dictionary uses valid font/color objects retained during this draw.
                let label = unsafe {
                    NSAttributedString::initWithString_attributes(
                        NSAttributedString::alloc(),
                        ns_string!("•"),
                        Some(&attributes),
                    )
                };
                label.drawAtPoint(frame.origin);
            }
        }
        NSGraphicsContext::restoreGraphicsState_class();
    }

    /// Draw finite frames under their original ticket. A changed clip, mode, or
    /// input revokes this batch before it can add pixels to a successor frame.
    fn draw_find_highlight_batch(
        &self,
        batch: &NativeMarkdownFindHighlightBatch,
        storage: &PieceBackedMarkdownTextStorage,
        dirty: NSRect,
    ) {
        let Some(controller) = self.ivars().controller.get().and_then(Weak::load) else {
            return; // Another window cannot grant paint authority after the original controller ends.
        };
        let authority = || batch.authority_current(self, storage, &controller);
        let Ok(visible) = native_markdown_table_intent_step(&authority, || self.visibleRect())
        else {
            return;
        };
        let Ok(width) = native_markdown_table_intent_step(&authority, || self.bounds().size.width)
        else {
            return;
        };
        let current = || batch.current(self, storage, &controller, visible, width);
        let Ok(color) = native_markdown_table_intent_step(&current, || {
            NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.75, 0.12, 0.25)
        }) else {
            return;
        };
        if native_markdown_table_intent_step(&current, || color.setFill()).is_err() {
            return;
        }
        for frame in &batch.frames {
            if !current() {
                // Reentry during fill revokes old drawing. It grants no right
                // to change selection or rerun Find on the successor input.
                return;
            }
            if NSIntersectsRect(*frame, dirty)
                && native_markdown_table_intent_step(&current, || NSBezierPath::fillRect(*frame))
                    .is_err()
            {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipped_preview_visibility_keeps_real_hidden_markers_and_utf16_intersections() {
        let text = "前 **中文🙂** 后\n";
        let mut presentation = MarkdownViewportPresentation::default();
        presentation
            .rebuild(7, 100..100 + text.encode_utf16().count(), text)
            .unwrap();
        let snapshot = presentation.semantic_snapshot().unwrap();
        let clipped = clipped_preview_visibility_runs(&snapshot, 103..111).unwrap();
        assert_eq!(clipped.first().unwrap().utf16_range.start, 103);
        assert_eq!(clipped.last().unwrap().utf16_range.end, 111);
        assert!(clipped.iter().any(|run| {
            run.utf16_range.contains(&103)
                && run.styles.contains(MarkdownPresentationStyles::MARKER)
                && run
                    .styles
                    .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
        })); // A clipped half-marker remains hidden rather than becoming Plain at the edge.
        assert!(clipped.iter().any(|run| {
            run.utf16_range.start <= 104
                && 108 <= run.utf16_range.end
                && run.styles.contains(MarkdownPresentationStyles::STRONG)
        })); // Chinese and the emoji surrogate pair retain original UTF-16 coordinates.
        assert!(
            clipped
                .iter()
                .any(|run| { run.utf16_range.contains(&110) && run.styles.is_empty() })
        );
        drop(snapshot);
        presentation.rebuild(7, 900..905, "other").unwrap();
        assert!(presentation.query(104).is_none());
        assert!(clipped.iter().any(|run| {
            run.utf16_range.contains(&104)
                && run.styles.contains(MarkdownPresentationStyles::STRONG)
        })); // After real directory eviction, retained facts still describe only the original 103..111.
    }

    #[test]
    fn clipped_preview_visibility_rejects_fallback_outside_and_incomplete_budgets() {
        let mut presentation = MarkdownViewportPresentation::default();
        presentation
            .record_source_fallback_window(7, 100..120)
            .unwrap();
        assert!(presentation.query(105).is_some());
        assert!(presentation.semantic_snapshot().is_none()); // Fallback empty styles cannot supply a complete snapshot.
        let text = "x".repeat(MAX_DECORATION_VIEWPORT_UNITS + 1);
        presentation
            .rebuild(7, 100..100 + text.len(), &text)
            .unwrap();
        let snapshot = presentation.semantic_snapshot().unwrap();
        assert!(clipped_preview_visibility_runs(&snapshot, 99..101).is_none());
        assert!(clipped_preview_visibility_runs(&snapshot, 100..100).is_none());
        assert!(clipped_preview_visibility_runs(&snapshot, 100..100 + text.len()).is_none());
        assert!(
            clipped_preview_visibility_runs(&snapshot, 100..100 + MAX_DECORATION_VIEWPORT_UNITS,)
                .is_some()
        ); // Admit the complete window at the actual limit, never a partial oversized proof.
        let marked = "**x** ".repeat(MAX_SPECIAL_CHARACTER_MARKERS);
        presentation.rebuild(8, 0..marked.len(), &marked).unwrap();
        let snapshot = presentation.semantic_snapshot().unwrap();
        assert!(clipped_preview_visibility_runs(&snapshot, 0..marked.len()).is_none());
        // A rejected high-run-count envelope does not invalidate an admitted finite subwindow.
        assert!(clipped_preview_visibility_runs(&snapshot, 0..6).is_some());
    }

    #[test]
    fn invisible_controls_match_the_preserved_editor_predicate_without_marking_emoji_joiners() {
        for unit in [0, 7, 0x061c, 0x200b, 0x202e, 0x2066, 0xfeff, 0xfffc] {
            // Explicit controls/bidi markers are visible cues; ordinary line breaks and emoji joiners are excluded.
            assert!(is_visible_special_character(unit));
        }
        for unit in [9, 10, 13, 32, 0x200c, 0x200d, 0x2068, 0xd83d] {
            // The exact baseline predicate does not treat every format character or surrogate as a special marker.
            assert!(!is_visible_special_character(unit));
        }
    }
}
