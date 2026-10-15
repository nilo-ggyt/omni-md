//! Owns viewport-sized line-number/fold controls without owning document text or editor lifetime.
//!
//! Geometry is prepared in the existing coalesced TextKit transaction. Painting reads a bounded
//! snapshot only; fold state contains canonical intervals and is invalidated by any text generation.

use super::*;
use objc2_app_kit::{
    NSAttributedStringNSStringDrawing, NSRulerOrientation, NSRulerView,
    NSTextLayoutFragmentEnumerationOptions,
};
use omni_markdown_core::markdown_folding::{markdown_fold_region, markdown_line_may_fold};

/// A finite viewport cap bounds ruler metadata even with unusually small or overlapping line heights.
const MAX_GUTTER_LINES: usize = 256;
/// Explicit nested folds are presentation metadata, bounded independently of document size.
const MAX_ACTIVE_FOLDS: usize = 256;

/// One logical source line has one number, irrespective of how many wrapped visual rows it occupies.
#[derive(Clone)]
struct GutterLine {
    number: usize,
    document_y: f64,
    height: f64,
    may_fold: bool,
    folded: bool,
}

/// Only the scroll surface owns the ruler; the backlink never prolongs its editor or canonical model.
pub(super) struct GutterIvars {
    text_view: Weak<PieceBackedMarkdownTextView>,
    lines: RefCell<Vec<GutterLine>>,
    attributes: Retained<MarkdownTextAttributes>,
}

define_class!(
    // SAFETY: NSRulerView initialization and drawing stay on the main thread; all Rust ivars are
    // initialized before AppKit calls overrides, and the client backlink is weak.
    #[unsafe(super = NSRulerView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = GutterIvars]
    pub(super) struct NativeMarkdownGutter;

    // SAFETY: this initialized NSRulerView subclass inherits main-thread NSObject identity.
    unsafe impl NSObjectProtocol for NativeMarkdownGutter {}

    impl NativeMarkdownGutter {
        #[unsafe(method(isFlipped))]
        /// Use the editor's downward source-line coordinate convention for both paint and hit testing.
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawHashMarksAndLabelsInRect:))]
        /// Paint a bounded geometry snapshot; no source parsing or TextKit layout occurs during drawing.
        fn draw_hash_marks_and_labels(&self, dirty: NSRect) {
            let Some(view) = self.ivars().text_view.load() else {
                // A late ruler draw after editor teardown has no source or native client authority.
                return;
            };
            let active_line = view.selected_source_line();
            // End the state borrow before drawing can invoke native appearance/layout callbacks.
            let lines = self.ivars().lines.borrow().clone();
            for line in lines {
                // Convert each retained source baseline through the public view coordinate system.
                let point = self.convertPoint_fromView(NSPoint::new(0.0, line.document_y), Some(&view));
                let rect = NSRect::new(
                    NSPoint::new(0.0, point.y),
                    NSSize::new(self.bounds().size.width, line.height.max(16.0)),
                );
                if !NSIntersectsRect(rect, dirty) {
                    // Off-damage labels reuse their existing backing store without native draw work.
                    continue;
                }
                if active_line == Some(line.number) {
                    // Active source-line emphasis is painted from copied finite geometry before its label.
                    NSGraphicsContext::saveGraphicsState_class();
                    NSColor::colorWithSRGBRed_green_blue_alpha(0.35, 0.48, 0.72, 0.12).setFill();
                    NSBezierPath::fillRect(rect);
                    NSGraphicsContext::restoreGraphicsState_class();
                }
                let marker = if line.folded {
                    // A collapsed block advertises the explicit expand action.
                    "▶"
                } else if line.may_fold {
                    // Hints are checked by the authoritative core query before any content is hidden.
                    "▾"
                } else {
                    // Single-line source has no fold affordance.
                    " "
                };
                let string = NSString::from_str(&format!("{} {marker}", line.number + 1));
                // SAFETY: this dictionary contains valid AppKit font/color values for immutable keys;
                // the temporary attributed label is retained for the complete synchronous draw.
                let label = unsafe {
                    NSAttributedString::initWithString_attributes(
                        NSAttributedString::alloc(),
                        &string,
                        Some(&self.ivars().attributes),
                    )
                };
                label.drawAtPoint(NSPoint::new(4.0, point.y));
            }
        }

        #[unsafe(method(mouseDown:))]
        /// Toggle only the source line hit in the current bounded ruler snapshot.
        fn mouse_down(&self, event: &NSEvent) {
            let Some(view) = self.ivars().text_view.load() else {
                // The weak client may disappear while AppKit finishes a queued mouse event.
                return;
            };
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let lines = self.ivars().lines.borrow().clone();
            let selected = lines.into_iter().find(|line| {
                // Hit testing uses the same converted source geometry as label painting.
                let top = self
                    .convertPoint_fromView(NSPoint::new(0.0, line.document_y), Some(&view))
                    .y;
                point.y >= top && point.y < top + line.height.max(16.0)
            });
            if let Some(line) = selected {
                // The toggle validates current canonical generation and commits any visible draft first.
                if let Err(error) = view.toggle_source_fold(line.number) {
                    // Capacity/commit/query failures preserve both source text and the existing fold graph.
                    present_native_markdown_error(&error);
                }
            }
        }
    }
);

impl NativeMarkdownGutter {
    /// Attach one ruler to one scroll surface, retaining only fonts, labels and a weak editor link.
    pub(super) fn new(
        scroll: &NSScrollView,
        view: &PieceBackedMarkdownTextView,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let font = NSFont::monospacedDigitSystemFontOfSize_weight(11.0, unsafe {
            // SAFETY: AppKit's regular font weight is an immutable exported scalar.
            NSFontWeightRegular
        });
        let color = NSColor::secondaryLabelColor();
        // SAFETY: AppKit exports immutable process-lifetime attribute key objects.
        let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
        let values: [&AnyObject; 2] = [&font, &color];
        let attributes = MarkdownTextAttributes::from_slices(&keys, &values);
        let allocated = Self::alloc(mtm).set_ivars(GutterIvars {
            text_view: Weak::from(view),
            lines: RefCell::new(Vec::new()),
            attributes,
        });
        // SAFETY: the NSRulerView designated initializer receives the live main-thread scroll owner
        // after every Rust ivar is initialized; the declared selector matches its Objective-C ABI.
        let ruler: Retained<Self> = unsafe {
            msg_send![super(allocated), initWithScrollView: Some(scroll), orientation: NSRulerOrientation::VerticalRuler]
        };
        ruler.setRuleThickness(74.0);
        ruler.setClientView(Some(view));
        ruler.setAccessibilityLabel(Some(ns_string!("Source line numbers and folding controls")));
        ruler
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    /// Check bounded visible metadata without exposing or copying its source-line records.
    pub(super) fn verify_visible_labels_for_probe(&self) -> Result<(), String> {
        let count = self.ivars().lines.borrow().len();
        if count == 0 || count > MAX_GUTTER_LINES {
            // Missing labels or an exceeded viewport cap invalidates the actual native gutter contract.
            return Err(format!(
                "the native source gutter has an invalid visible label count: {count}"
            ));
        }
        Ok(())
    }

    /// Read current cached paragraph geometry for one logical source line, without native layout work.
    fn line_frame(&self, line: usize, width: f64) -> Option<NSRect> {
        self.ivars()
            .lines
            .borrow()
            .iter()
            .find(|entry| entry.number == line)
            .map(|entry| {
                // The returned rectangle is copied metadata, so all cache borrows end before painting.
                NSRect::new(
                    NSPoint::new(0.0, entry.document_y),
                    NSSize::new(width, entry.height.max(20.0)),
                )
            })
    }

    /// Drop bounded geometry immediately when detaching; the scroll view separately releases its ruler.
    pub(super) fn clear(&self) {
        self.ivars().lines.borrow_mut().clear();
        self.setClientView(None);
    }
}

impl PieceBackedMarkdownTextView {
    /// Resolve the settled native caret's logical line using canonical tree aggregates.
    fn selected_source_line(&self) -> Option<usize> {
        let storage = self.ivars().text_storage.load()?;
        if storage.presentation_mode() != MarkdownPresentationMode::Source {
            // Inline preview owns no active source-line/gutter highlight.
            return None;
        }
        let selection = self.selectedRange();
        let document = storage.document()?;
        let document = document.borrow();
        document
            .byte_offset_for_utf16(selection.location)
            .and_then(|byte| document.line_for_byte(byte))
            .ok()
    }

    /// Paint a cached active source line before normal text drawing, with no element enumeration.
    pub(super) fn draw_source_active_line(&self, dirty: NSRect) {
        let Some(line) = self.selected_source_line() else {
            // Preview/disconnected input has no source-line background to draw.
            return;
        };
        let Some(ruler) = self.ivars().gutter.get().and_then(Weak::load) else {
            // Native storage-only adapters have no prepared source-line geometry.
            return;
        };
        let Some(frame) = ruler.line_frame(line, self.bounds().size.width) else {
            // Offscreen/unlaid caret geometry is deferred; drawing never forces whole-document layout.
            return;
        };
        if NSIntersectsRect(frame, dirty) {
            // The subtle native source highlight uses the same one-line range as the gutter.
            NSGraphicsContext::saveGraphicsState_class();
            NSColor::colorWithSRGBRed_green_blue_alpha(0.35, 0.48, 0.72, 0.08).setFill();
            NSBezierPath::fillRect(frame);
            NSGraphicsContext::restoreGraphicsState_class();
        }
    }

    /// Mark only existing ruler backing as damaged after a settled selection change.
    pub(super) fn invalidate_source_gutter_highlight(&self) {
        if let Some(ruler) = self.ivars().gutter.get().and_then(Weak::load) {
            // Selection changes reuse cached label geometry and create no timer or extra layout request.
            ruler.setNeedsDisplay(true);
        }
    }

    /// Discard all view-local folds before input, selection into hidden text, or a source generation change.
    pub(super) fn unfold_source(&self) {
        if self.ivars().folds.borrow_mut().is_empty() {
            // The normal unfolded editor needs no layout invalidation or native transaction.
            return;
        }
        self.ivars().folds.borrow_mut().clear();
        self.invalidate_fold_layout();
    }

    /// Invalidate only derived layout; canonical bytes, selection history and saved baseline are untouched.
    fn invalidate_fold_layout(&self) {
        self.ivars().visible_table_layout_cache.borrow_mut().take();
        if let Some(manager) = self.textLayoutManager()
            && let Some(content) = manager.textContentManager()
        {
            // End metadata borrows before AppKit may synchronously enumerate the updated fold delegate.
            let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
            manager.invalidateLayoutForRange(&provider.documentRange());
        }
        self.setNeedsDisplay(true);
        self.schedule_visible_table_layout_update();
    }

    /// Resolve a single explicit user fold while preserving the canonical source and full text coordinates.
    pub(super) fn toggle_source_fold(&self, line: usize) -> Result<(), String> {
        self.commit_active_table_cell()?;
        self.commit_composition_before_release()?;
        if self
            .ivars()
            .text_storage
            .load()
            .is_none_or(|storage| storage.presentation_mode() != MarkdownPresentationMode::Source)
        {
            // Folding belongs to literal source mode; preview retains its normal inline/table presentation.
            return Err("Switch to Source mode to fold Markdown blocks".into());
        }
        let document = self
            .ivars()
            .text_storage
            .load()
            .and_then(|storage| storage.document())
            .ok_or("The editor is closed")?;
        let generation = document.borrow().generation();
        if self.ivars().fold_generation.get() != generation {
            // An intervening canonical edit invalidates every saved source coordinate.
            self.ivars().folds.borrow_mut().clear();
            self.ivars().fold_generation.set(generation);
        }
        let existing = self
            .ivars()
            .folds
            .borrow()
            .iter()
            .position(|fold| fold.header_line == line);
        if let Some(index) = existing {
            // Expanding removes only this fold; nested independent fold choices remain view-local.
            self.ivars().folds.borrow_mut().remove(index);
        } else {
            // Query before changing presentation: a rejected/nonfoldable source line stays fully visible.
            let region = markdown_fold_region(&document.borrow(), line)
                .map_err(|error| error.to_string())?;
            let Some(region) = region else {
                // A cheap gutter hint never authorizes hiding a context-dependent continuation line.
                return Ok(());
            };
            if self.ivars().folds.borrow().len() >= MAX_ACTIVE_FOLDS {
                // The explicit metadata cap cannot silently replace an earlier user fold choice.
                return Err("Expand a folded block before adding more folds".into());
            }
            let selected = self.selectedRange();
            if selected.location > region.hidden_utf16_range.start
                && selected.location < region.hidden_utf16_range.end
            {
                // A newly hidden active caret moves to the visible header end before paragraph omission.
                self.setSelectedRange(NSRange::new(region.hidden_utf16_range.start, 0));
            }
            self.ivars().folds.borrow_mut().push(region);
        }
        self.invalidate_fold_layout();
        Ok(())
    }

    /// Rebuild at most one viewport of logical line labels from existing TextKit 2 fragment geometry.
    pub(super) fn refresh_source_gutter(&self) {
        let Some(ruler) = self.ivars().gutter.get().and_then(Weak::load) else {
            // Isolated storage/probe adapters may intentionally have no scroll-owned ruler.
            return;
        };
        if self
            .ivars()
            .text_storage
            .load()
            .is_none_or(|storage| storage.presentation_mode() != MarkdownPresentationMode::Source)
        {
            // Preview has no ruler labels or fold controls, matching its uncluttered baseline presentation.
            ruler.ivars().lines.borrow_mut().clear();
            return;
        }
        let Some(document) = self
            .ivars()
            .text_storage
            .load()
            .and_then(|storage| storage.document())
        else {
            // A disconnected editor must discard its last source-coordinate labels.
            ruler.ivars().lines.borrow_mut().clear();
            return;
        };
        let is_empty = document.borrow().utf16_len() == 0;
        if is_empty {
            // An empty new note still has source line one; it needs no TextKit fragment enumeration.
            *ruler.ivars().lines.borrow_mut() = vec![GutterLine {
                number: 0,
                document_y: self.textContainerOrigin().y,
                height: 20.0,
                may_fold: false,
                folded: false,
            }];
            ruler.setNeedsDisplay(true);
            return;
        }
        let generation = document.borrow().generation();
        if self.ivars().fold_generation.get() != generation {
            // Edited ranges are never remapped speculatively: any canonical edit expands existing folds.
            self.ivars().folds.borrow_mut().clear();
            self.ivars().fold_generation.set(generation);
        }
        let Some(manager) = self.textLayoutManager() else {
            // Missing public TextKit geometry defers labels until a future native layout transaction.
            return;
        };
        let Some(content) = manager.textContentManager() else {
            // A detached manager cannot translate fragment locations to canonical source offsets.
            return;
        };
        let visible = self.visibleRect();
        let origin = self.textContainerOrigin();
        let Some(first) = manager.textLayoutFragmentForPosition(NSPoint::new(
            0.0,
            (visible.origin.y - origin.y).max(0.0),
        )) else {
            // There are no laid-out visible fragments yet; painting must not force whole-document layout.
            return;
        };
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
        let start = provider.documentRange().location();
        let lines = RefCell::new(Vec::<GutterLine>::new());
        let block =
            block2::RcBlock::new(|fragment: NonNull<objc2_app_kit::NSTextLayoutFragment>| {
                // SAFETY: TextKit lends each live fragment only for this synchronous enumeration callback.
                let fragment = unsafe { fragment.as_ref() };
                let frame = fragment.layoutFragmentFrame();
                if lines.borrow().len() >= MAX_GUTTER_LINES
                    || frame.origin.y + origin.y > visible.origin.y + visible.size.height
                {
                    // Stop at the viewport bottom or fixed metadata cap; no offscreen document walk follows.
                    return objc2::runtime::Bool::NO;
                }
                let offset = content
                    .offsetFromLocation_toLocation(&start, &fragment.rangeInElement().location());
                if let Ok(offset) = usize::try_from(offset) {
                    // Only nonnegative canonical positions can become labels; native sentinels contribute no row.
                    let metadata = {
                        // Native location queries have completed before borrowing the canonical piece tree.
                        let document = document.borrow();
                        document
                            .byte_offset_for_utf16(offset)
                            .and_then(|byte| document.line_for_byte(byte))
                            .map(|number| {
                                // The cheap finite hint grants no hiding authority; the explicit fold query rechecks grammar.
                                let hint =
                                    markdown_line_may_fold(&document, number).unwrap_or(false);
                                (number, hint)
                            })
                    };
                    if let Ok((number, hint)) = metadata
                        && lines
                            .borrow()
                            .last()
                            .is_none_or(|line| line.number != number)
                    {
                        // Wrapped fragments share one source label; the first fragment supplies its baseline.
                        let folded = self
                            .ivars()
                            .folds
                            .borrow()
                            .iter()
                            .any(|fold| fold.header_line == number);
                        lines.borrow_mut().push(GutterLine {
                            number,
                            document_y: frame.origin.y + origin.y,
                            height: frame.size.height,
                            may_fold: hint,
                            folded,
                        });
                    }
                }
                objc2::runtime::Bool::YES
            });
        manager.enumerateTextLayoutFragmentsFromLocation_options_usingBlock(
            Some(&first.rangeInElement().location()),
            NSTextLayoutFragmentEnumerationOptions::empty(),
            &block,
        );
        drop(block);
        *ruler.ivars().lines.borrow_mut() = lines.into_inner();
        ruler.setNeedsDisplay(true);
    }
}
