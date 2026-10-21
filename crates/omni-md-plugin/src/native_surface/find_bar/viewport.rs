//! Input changes revoke old feedback and pixels. The original settled refresh
//! produces bounded counts, source ranges, and actual literal highlights together.

use super::*;
/// Immediate visible feedback shares fixed source/result budgets, independent of document length.
pub(in crate::native_surface) const MAX_DECORATION_VIEWPORT_UNITS: usize = 16_384;
pub(in crate::native_surface) const MAX_SPECIAL_CHARACTER_MARKERS: usize = 128;
use omni_markdown_core::MarkdownDocumentBufferError;
use omni_markdown_core::markdown_find::{
    MarkdownLiteralFindOptions, visit_markdown_literal_matches_cancellable,
};

/// Two actual sources in one original Scene preparation. Cells borrow their visible
/// windows; a canonical envelope orders them but does not authorize searching that envelope.
#[cfg(feature = "native-paged-production")]
pub(in crate::native_surface) enum NativeMarkdownFindVisibleSource<'a> {
    Body(std::ops::Range<usize>),
    Cell {
        canonical: std::ops::Range<usize>,
        window: &'a crate::native_surface::paged_line_viewport::NativeMarkdownComposedTableCell,
    },
}

/// Display-window and canonical-source failures are distinct boundaries. Preserve
/// the first Source error and the original geometry error for the owning Body to map.
#[cfg(feature = "native-paged-production")]
pub(in crate::native_surface) enum NativeMarkdownPagedFindHighlightError {
    Source(MarkdownDocumentBufferError),
    Window(crate::native_surface::paged_line_viewport::NativeMarkdownComposedViewportError),
}

#[cfg(feature = "native-paged-production")]
impl From<MarkdownDocumentBufferError> for NativeMarkdownPagedFindHighlightError {
    fn from(cause: MarkdownDocumentBufferError) -> Self {
        Self::Source(cause)
    }
}

/// Queued feedback retains original weak bar/document identities and input-time
/// revisions, never query text, document contents, or strong Native references.
#[derive(Clone)]
pub(in crate::native_surface) struct NativeMarkdownFindViewportFeedback {
    bar: std::rc::Weak<NativeMarkdownFindBar>,
    document: WeakMarkdownDocument,
    generation: u64,
    owner: usize,
    intent: u64,
    bar_revision: u64,
    visible: bool,
    refresh: bool,
}

impl NativeMarkdownFindViewportFeedback {
    /// The bar fixes its document at creation. Capture its generation before any
    /// Native getter; delayed execution cannot acquire the then-current document.
    pub(in crate::native_surface) fn capture(
        bar: &Rc<NativeMarkdownFindBar>,
        controller: &NativeMarkdownController,
        intent: u64,
        refresh: bool,
    ) -> Option<Self> {
        let document = bar.document.upgrade()?;
        let generation = document.borrow().generation();
        let request = Self {
            bar: Rc::downgrade(bar),
            document: bar.document.clone(),
            generation,
            owner: std::ptr::from_ref(controller).addr(),
            intent,
            bar_revision: bar.layout_revision.get(),
            visible: bar.visible.get(),
            refresh,
        };
        request.current(controller).then_some(request)
    }

    /// Check only original Rust authority; identity comparison neither upgrades a
    /// Native weak reference nor borrows the active surface.
    pub(in crate::native_surface) fn current(&self, controller: &NativeMarkdownController) -> bool {
        std::ptr::from_ref(controller).addr() == self.owner
            && native_markdown_controller_has_foreground_authority(controller)
            && controller.find_input_intent() == self.intent
            && controller.owns_find_input_bar(&self.bar)
            && self.bar.upgrade().is_some_and(|bar| {
                !bar.closed.get()
                    && bar.visible.get() == self.visible
                    && bar.layout_revision.get() == self.bar_revision
                    && WeakMarkdownDocument::ptr_eq(&bar.document, &self.document)
            })
            && self
                .document
                .upgrade()
                .is_some_and(|document| document.borrow().generation() == self.generation)
    }

    /// Clear old result/replacement status immediately for new input. No bar-state
    /// borrow remains when the native setter can reenter.
    pub(in crate::native_surface) fn clear(&self, controller: &NativeMarkdownController) {
        if self.current(controller)
            && let Some(bar) = self.bar.upgrade()
        {
            bar.status.setStringValue(ns_string!(""));
        }
    }

    /// Hand completed input to the existing coalescing executor immediately; the
    /// feedback callback need not wait another main-queue turn to request geometry.
    pub(in crate::native_surface) fn schedule_highlights(
        &self,
        controller: &NativeMarkdownController,
    ) {
        if !self.current(controller) {
            return;
        }
        let Some(bar) = self.bar.upgrade() else {
            return;
        };
        let binding = bar.presentation_body.borrow().clone();
        if let Some(body) = binding.and_then(|body| body.upgrade()) {
            body.refresh_find_highlights(&bar, controller, self);
        }
    }

    /// The original Scene publishes visible matches. Input feedback validates regex
    /// syntax without creating a temporary document graph.
    pub(in crate::native_surface) fn run(&self, controller: &NativeMarkdownController) {
        if !self.current(controller) {
            return;
        }
        let Some(bar) = self.bar.upgrade() else {
            return;
        };
        if bar.presentation_body.borrow().is_none() {
            return;
        }
        let current = || self.current(controller);
        if self.visible && self.refresh {
            let result = (|| -> Result<(), String> {
                if let Some(query) = read_visible_query(&bar, &current)?
                    && query.regex
                {
                    native_markdown_table_intent_step(&current, || query.compiled_regex())
                        .map_err(|_| "原正则输入已变化")??;
                    self.consume_status(&bar, &current, "");
                }
                Ok(())
            })();
            if let Err(cause) = result {
                self.consume_status(&bar, &current, &cause);
            }
        }
    }

    /// The independent TextKit fixture observes literal highlights through its own
    /// existing actual graph. The production entry never calls this reference path.
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    pub(in crate::native_surface) fn prepare_highlights(
        &self,
        bar: &NativeMarkdownFindBar,
        view: &PieceBackedMarkdownTextView,
        storage: &PieceBackedMarkdownTextStorage,
        controller: &NativeMarkdownController,
        visible: NSRect,
        source_range: std::ops::Range<usize>,
    ) -> Option<NativeMarkdownFindHighlightPreparation> {
        if !self.visible || !self.current(controller) {
            return None;
        }
        match self.read_highlight_matches(bar, view, storage, controller, visible, source_range) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.consume_status(bar, &|| self.current(controller), &error);
                // Read/geometry failure publishes no partial batch; original text and explicit Find remain usable.
                None
            }
        }
    }

    /// Query text lives only for this refresh. Source uses original text; Preview
    /// permits only semantic ranges subsequently proved visible.
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    fn read_highlight_matches(
        &self,
        bar: &NativeMarkdownFindBar,
        view: &PieceBackedMarkdownTextView,
        storage: &PieceBackedMarkdownTextStorage,
        controller: &NativeMarkdownController,
        visible: NSRect,
        range: std::ops::Range<usize>,
    ) -> Result<Option<NativeMarkdownFindHighlightPreparation>, String> {
        let document = self.document.upgrade().ok_or("原查找文稿已关闭")?;
        if !view.ivars().folds.matches_document(&document)
            || !WeakMarkdownDocument::ptr_eq(&self.document, &storage.ivars().document.borrow())
        {
            // Equal generation numbers cannot authorize another graph's visible ranges.
            return Ok(None);
        }
        let selection = view
            .ivars()
            .folds
            .capture_selection()
            .map_err(|_| "当前编辑状态暂不能读取可见范围")?;
        let table_epoch = view.ivars().table_input_intent_epoch.get();
        let mode = storage.presentation_mode();
        let fold_revision = view.ivars().folds.revision();
        let font_epoch = storage.ivars().font_run_scratch.epoch();
        let current = || {
            self.current(controller)
                && selection.current()
                && !view.ivars().textkit_graph_teardown.get()
                && view.ivars().table_input_intent_epoch.get() == table_epoch
                && storage.presentation_mode() == mode
                && view.ivars().folds.revision() == fold_revision
                && storage.ivars().font_run_scratch.epoch() == font_epoch
        };
        let Some(query) = read_visible_query(bar, &current)? else {
            // A marked query provides neither candidate text nor temporary highlights.
            return Ok(None);
        };
        if query.search.is_empty() {
            self.consume_status(bar, &current, "");
            // Empty input keeps empty feedback; do not enumerate zero-length matches or select text.
            return Ok(None);
        }
        if query.regex {
            // Lookaround can depend on source outside the viewport. Syntax feedback
            // belongs to the original input queue; a clipped string cannot supply
            // full-document semantics or justify automatic regex highlights here.
            return Ok(None);
        }
        let length = document.borrow().utf16_len();
        if length == 0 {
            self.consume_status(bar, &current, "当前文档为空");
            return Ok(None);
        }
        let width = native_markdown_table_intent_step(&current, || view.bounds().size.width)
            .map_err(|_| "原查找宽度已变化")?;
        if range.is_empty() {
            // No proved visible source range means unknown coverage, not a zero-match result.
            return Ok(None);
        }
        let outcome = visible_literal_matches(
            &document.borrow(),
            &unquote(&query.search),
            range,
            MarkdownLiteralFindOptions {
                case_sensitive: query.case_sensitive,
                whole_word: query.word,
                overlapping: true,
            },
            MAX_SPECIAL_CHARACTER_MARKERS,
            current,
        )
        .map_err(|cause| {
            // Diagnose the actual source failure; fixed Chinese product status must
            // not incorporate system English or note contents.
            eprintln!("native_find_viewport event=source_read_failed cause={cause}");
            "当前可见区域查找失败，请重试".to_owned()
        })?;
        if current() {
            self.consume_status(bar, &current, &outcome.status());
        }
        if !current() {
            // Status-setter reentry can revoke the old ranges' geometry authority too.
            return Ok(None);
        }
        Ok(Some(NativeMarkdownFindHighlightPreparation {
            ranges: outcome.ranges,
            batch: NativeMarkdownFindHighlightBatch {
                feedback: self.clone(),
                mode,
                fold_revision,
                font_epoch,
                table_epoch,
                width: native_markdown_layout_metric(width),
                visible,
                frames: Vec::new(),
            },
        }))
    }

    /// Paged publication borrows ordered actual visible source domains. All rows and
    /// cells share 16K UTF-16 units and 128 matches rather than renewing per-cell budgets.
    /// Query text is call-local. Return typed Source failures instead of zero/success feedback.
    #[cfg(feature = "native-paged-production")]
    pub(in crate::native_surface) fn prepare_paged_highlights(
        &self,
        bar: &NativeMarkdownFindBar,
        document: &Rc<RefCell<MarkdownDocumentBuffer>>,
        sources: &[NativeMarkdownFindVisibleSource<'_>],
        current: &impl Fn() -> bool,
    ) -> Result<Box<[std::ops::Range<usize>]>, NativeMarkdownPagedFindHighlightError> {
        if !self.visible || !current() {
            return Ok(Box::new([]));
        }
        let query = match read_visible_query(bar, current) {
            Ok(Some(query)) => query,
            // Composition neither exposes candidates nor keeps pixels from an earlier query.
            Ok(None) => return Ok(Box::new([])),
            Err(message) => {
                self.consume_status(bar, current, &message);
                return Ok(Box::new([]));
            }
        };
        if query.search.is_empty() {
            self.consume_status(bar, current, "");
            return Ok(Box::new([]));
        }
        if query.regex || sources.is_empty() {
            // Regex syntax belongs to the original input queue. Viewport-clipped
            // lookaround must not pretend to implement whole-document semantics.
            return Ok(Box::new([]));
        }
        let search = unquote(&query.search);
        let options = MarkdownLiteralFindOptions {
            case_sensitive: query.case_sensitive,
            whole_word: query.word,
            overlapping: true,
        };
        let mut outcome = VisibleLiteralMatches {
            count: 0,
            ranges: Vec::new(),
            source_limited: false,
            count_limited: false,
        };
        let mut remaining = MAX_DECORATION_VIEWPORT_UNITS;
        for (ordinal, source) in sources.iter().enumerate() {
            let maximum_units = remaining;
            // Match each original contiguous raw domain once. The cell iterator
            // clips its display end to this shared remaining budget and advances
            // sealed records incrementally; it never collects an enormous cell's
            // raw fragments or grants each cell a fresh 16K allowance.
            let limited = {
                let mut consume =
                    |range: std::ops::Range<usize>| -> Result<bool, MarkdownDocumentBufferError> {
                        let end = {
                            let document = document.borrow();
                            markdown_utf16_boundary_down(
                                &document,
                                range.start.saturating_add(remaining).min(range.end),
                            )?
                        };
                        let found = visible_literal_matches(
                            &document.borrow(),
                            &search,
                            range.start..end,
                            options,
                            MAX_SPECIAL_CHARACTER_MARKERS - outcome.count,
                            current,
                        )?;
                        remaining -= end - range.start;
                        outcome.count += found.count;
                        outcome.ranges.extend(found.ranges);
                        outcome.count_limited = found.count_limited;
                        outcome.source_limited |= end < range.end;
                        Ok(!outcome.count_limited && !outcome.source_limited && remaining != 0)
                    };
                match source {
                    NativeMarkdownFindVisibleSource::Body(range) => {
                        consume(range.clone())?;
                        false
                    }
                    NativeMarkdownFindVisibleSource::Cell { window, .. } => {
                        let ranges = window
                            .visible_find_ranges(maximum_units, current)
                            .map_err(NativeMarkdownPagedFindHighlightError::Window)?;
                        if let Some(ranges) = ranges {
                            let limited = ranges.source_limited();
                            for range in ranges {
                                if !consume(range?)? {
                                    break;
                                }
                            }
                            limited
                        } else {
                            // This ticket proves no text intersects the clip; invent no empty source.
                            false
                        }
                    }
                }
            };
            outcome.source_limited |= limited;
            if outcome.count_limited || outcome.source_limited || remaining == 0 {
                outcome.source_limited |= ordinal + 1 < sources.len();
                break;
            }
        }
        if current() {
            self.consume_status(bar, current, &outcome.status());
        }
        // Report actual overlapping hits, but draw their union so translucent
        // highlight paint does not accumulate twice over the same pixel.
        let mut merged: Vec<std::ops::Range<usize>> = Vec::with_capacity(outcome.ranges.len());
        for range in outcome.ranges {
            if let Some(last) = merged.last_mut()
                && range.start <= last.end
            {
                last.end = last.end.max(range.end);
            } else {
                merged.push(range);
            }
        }
        Ok(merged.into_boxed_slice())
    }

    /// Publish status only while this actual input awaits feedback. Scrolling and
    /// explicit actions cannot reacquire status-publication authority.
    fn consume_status(
        &self,
        bar: &NativeMarkdownFindBar,
        current: &impl Fn() -> bool,
        message: &str,
    ) {
        let pending = || current() && bar.viewport_feedback_intent.get() == Some(self.intent);
        write_viewport_status(bar, &pending, message);
        if pending() {
            bar.viewport_feedback_intent.set(None);
        } // Setter reentry may register a successor intent; never clear that pending feedback.
    }
}

/// Observe the original query's composition without committing/cancelling its IME.
/// Every Native getter must retain the same original authority.
fn read_visible_query(
    bar: &NativeMarkdownFindBar,
    current: &impl Fn() -> bool,
) -> Result<Option<Query>, String> {
    let editor = native_markdown_table_intent_step(current, || bar.query.currentEditor())
        .map_err(|_| "原查询输入已变化")?;
    if let Some(editor) = editor {
        let editor = editor
            .downcast_ref::<NSTextView>()
            .ok_or("查找字段的原输入上下文不可用")?;
        if native_markdown_table_intent_step(current, || editor.hasMarkedText())
            .map_err(|_| "原查询输入已变化")?
        {
            // During composition revoke old pixels without reading the candidate query.
            return Ok(None);
        }
    }
    Query::read_viewport(bar, current).map(Some)
}

/// Source ranges live only during this settled preparation. Geometry publication
/// must not retain a match table or query string afterward.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(in crate::native_surface) struct NativeMarkdownFindHighlightPreparation {
    pub(in crate::native_surface) ranges: Vec<std::ops::Range<usize>>,
    pub(in crate::native_surface) batch: NativeMarkdownFindHighlightBatch,
}

/// The existing decoration vector owns bounded rectangles and a weak ticket, not
/// the document, TextKit objects, query text, or a second layout.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(in crate::native_surface) struct NativeMarkdownFindHighlightBatch {
    feedback: NativeMarkdownFindViewportFeedback,
    mode: MarkdownPresentationMode,
    fold_revision: u64,
    font_epoch: u64,
    table_epoch: u64,
    width: i64,
    pub(in crate::native_surface) visible: NSRect,
    pub(in crate::native_surface) frames: Vec<NSRect>,
}

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
impl NativeMarkdownFindHighlightBatch {
    /// Prepared rectangles still require the original bar/document, presentation,
    /// font, folds, and visible geometry. Drawing never enumerates document contents.
    pub(in crate::native_surface) fn current(
        &self,
        view: &PieceBackedMarkdownTextView,
        storage: &PieceBackedMarkdownTextStorage,
        controller: &NativeMarkdownController,
        visible: NSRect,
        width: f64,
    ) -> bool {
        self.authority_current(view, storage, controller)
            && self.width == native_markdown_layout_metric(width)
            && self.visible == visible
    }

    /// Validate original authority before reading actual clip/width. A Native
    /// getter cannot renew a batch revoked by closure or a presentation change.
    pub(in crate::native_surface) fn authority_current(
        &self,
        view: &PieceBackedMarkdownTextView,
        storage: &PieceBackedMarkdownTextStorage,
        controller: &NativeMarkdownController,
    ) -> bool {
        self.feedback.current(controller)
            && self.feedback.visible
            && !view.ivars().textkit_graph_teardown.get()
            && view.ivars().textkit_input_depth.get() == 0
            && !view
                .ivars()
                .marked_text_range
                .get()
                .is_some_and(|range| range.length != 0)
            && self.mode == storage.presentation_mode()
            && self.fold_revision == view.ivars().folds.revision()
            && self.font_epoch == storage.ivars().font_run_scratch.epoch()
            && self.table_epoch == view.ivars().table_input_intent_epoch.get()
            && WeakMarkdownDocument::ptr_eq(
                &self.feedback.document,
                &storage.ivars().document.borrow(),
            )
    }

    /// Cache equality includes original authority and rectangles; equal pixels do
    /// not authorize a stale batch from another query or document.
    pub(in crate::native_surface) fn same_geometry(&self, other: &Self) -> bool {
        std::rc::Weak::ptr_eq(&self.feedback.bar, &other.feedback.bar)
            && WeakMarkdownDocument::ptr_eq(&self.feedback.document, &other.feedback.document)
            && self.feedback.intent == other.feedback.intent
            && self.feedback.generation == other.feedback.generation
            && self.feedback.bar_revision == other.feedback.bar_revision
            && self.mode == other.mode
            && self.fold_revision == other.fold_revision
            && self.font_epoch == other.font_epoch
            && self.table_epoch == other.table_epoch
            && self.width == other.width
            && self.visible == other.visible
            && self.frames == other.frames
    }
}

/// String construction and the setter separately check the original viewport
/// ticket, preventing Native reentry from overwriting successor input or position.
fn write_viewport_status(bar: &NativeMarkdownFindBar, current: &impl Fn() -> bool, message: &str) {
    if let Ok(message) = native_markdown_table_intent_step(current, || NSString::from_str(message))
    {
        let _ = native_markdown_table_intent_step(current, || bar.status.setStringValue(&message));
    }
}

/// One visitor supplies counts and bounded source ranges, released after geometry
/// consumption. Exhausted budgets must be reported as partial or at-least counts.
#[derive(Debug, Eq, PartialEq)]
struct VisibleLiteralMatches {
    count: usize,
    ranges: Vec<std::ops::Range<usize>>,
    source_limited: bool,
    count_limited: bool,
}

impl VisibleLiteralMatches {
    /// Describe actual current coverage; exhausted budgets or local zero matches
    /// cannot establish a document-wide result.
    fn status(&self) -> String {
        if self.count_limited {
            format!("当前可见区域至少 {} 处匹配（已达反馈上限）", self.count)
        } else if self.source_limited {
            format!("当前可见区域已检查部分，找到 {} 处完整匹配", self.count)
        } else {
            format!("当前可见区域找到 {} 处匹配", self.count)
        }
    }
}

/// Pass original document ranges to the existing visitor. Whole-word matching reads
/// actual source neighbors instead of treating a clipped window as a word boundary.
fn visible_literal_matches(
    document: &MarkdownDocumentBuffer,
    search: &str,
    viewport: std::ops::Range<usize>,
    options: MarkdownLiteralFindOptions,
    maximum_matches: usize,
    keep_searching: impl FnMut() -> bool,
) -> Result<VisibleLiteralMatches, MarkdownDocumentBufferError> {
    let end = markdown_utf16_boundary_down(
        document,
        viewport
            .start
            .saturating_add(MAX_DECORATION_VIEWPORT_UNITS)
            .min(viewport.end),
    )?;
    let mut result = VisibleLiteralMatches {
        count: 0,
        ranges: Vec::new(),
        source_limited: end < viewport.end,
        count_limited: false,
    };
    visit_markdown_literal_matches_cancellable(
        document,
        search,
        viewport.start..end,
        options,
        keep_searching,
        |found| {
            result.count += 1;
            // Deliver counts and coordinates from the same visit; geometry must not rescan.
            result.ranges.push(found.range_utf16);
            result.count_limited = result.count == maximum_matches;
            // Stop the original visitor at its budget; status text cannot justify scanning more source.
            Ok(!result.count_limited)
        },
    )?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Open only a synthetic original file. Tests borrow the same canonical source,
    /// without AppKit initialization or a full-document Native mirror.
    fn document(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("visible-find.md");
        std::fs::write(&path, source).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
        (directory, document)
    }

    #[test]
    fn whole_word_uses_original_neighbors_on_both_viewport_edges() {
        let (_directory, document) = document("foobar foo");
        let before = document.selection();
        let options = MarkdownLiteralFindOptions {
            whole_word: true,
            ..MarkdownLiteralFindOptions::default()
        };
        for (query, range, count) in [("bar", 3..6, 0), ("foo", 0..3, 0), ("foo", 7..10, 1)] {
            // Actual neighbors reject clipped words on either viewport edge while
            // the complete trailing word remains searchable.
            let found = visible_literal_matches(
                &document,
                query,
                range,
                options,
                MAX_SPECIAL_CHARACTER_MARKERS,
                || true,
            )
            .unwrap();
            assert_eq!(found.count, count);
            assert!(!found.source_limited);
            assert!(!found.count_limited);
        }
        assert_eq!(document.selection(), before);
        assert_eq!(document.generation(), 0); // Visible feedback changes no selection, edit, or history generation.
    }

    #[test]
    fn clipped_source_does_not_split_an_emoji_or_claim_a_complete_visible_total() {
        let source = format!("{}😀目标", "a".repeat(MAX_DECORATION_VIEWPORT_UNITS - 1));
        let (_directory, document) = document(&source);
        let found = visible_literal_matches(
            &document,
            "😀",
            0..document.utf16_len(),
            MarkdownLiteralFindOptions::default(),
            MAX_SPECIAL_CHARACTER_MARKERS,
            || true,
        )
        .unwrap();
        assert_eq!(found.count, 0);
        assert!(found.source_limited);
        assert!(!found.count_limited);
        assert!(found.status().contains("已检查部分"));
        assert_eq!(document.utf16_len(), MAX_DECORATION_VIEWPORT_UNITS + 3);
        assert_eq!(document.generation(), 0); // A cap inside an emoji narrows the read, never the original text.
    }

    #[test]
    fn dense_viewport_stops_at_the_real_feedback_limit_and_reports_at_least() {
        let (_directory, document) = document(&"x".repeat(MAX_SPECIAL_CHARACTER_MARKERS * 2));
        let found = visible_literal_matches(
            &document,
            "x",
            0..document.utf16_len(),
            MarkdownLiteralFindOptions::default(),
            MAX_SPECIAL_CHARACTER_MARKERS,
            || true,
        )
        .unwrap();
        assert_eq!(found.count, MAX_SPECIAL_CHARACTER_MARKERS);
        assert!(found.count_limited);
        assert!(!found.source_limited);
        assert!(found.status().contains("至少")); // Early stopping cannot turn 128 into an exact/global total.
        assert_eq!(document.generation(), 0);
    }

    #[test]
    fn cancelled_viewport_does_not_publish_a_partial_zero_result() {
        let (_directory, document) = document("中文😀目标");
        let result = visible_literal_matches(
            &document,
            "不存在",
            0..document.utf16_len(),
            MarkdownLiteralFindOptions::default(),
            MAX_SPECIAL_CHARACTER_MARKERS,
            || false,
        );
        assert!(matches!(
            result,
            Err(MarkdownDocumentBufferError::FindInterrupted)
        ));
        assert_eq!(document.generation(), 0); // Revocation is cancellation, not a completed zero-match search.
    }

    #[test]
    fn same_scan_delivers_original_unicode_ranges_for_visible_glyph_geometry() {
        let (_directory, document) = document("前 x😀 x 后");
        let before = document.selection();
        let latin_ranges = [2..3, 6..7];
        let emoji_range = 3..5;
        for (query, ranges) in [
            ("x", latin_ranges.as_slice()),
            ("😀", std::slice::from_ref(&emoji_range)),
        ] {
            let found = visible_literal_matches(
                &document,
                query,
                0..document.utf16_len(),
                MarkdownLiteralFindOptions::default(),
                MAX_SPECIAL_CHARACTER_MARKERS,
                || true,
            )
            .unwrap();
            // Geometry consumes Core's original UTF-16 coordinates; an emoji is not one unit.
            assert_eq!(found.ranges.as_slice(), ranges);
            assert_eq!(found.count, ranges.len());
        }
        assert_eq!(document.selection(), before);
        assert_eq!(document.generation(), 0);
    }
}
