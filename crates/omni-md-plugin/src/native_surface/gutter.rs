//! Owns viewport-sized line-number/fold controls without owning document text or editor lifetime.
//!
//! Default paged production uses the independent `paged` module. The explicitly gated TextKit reference
//! implementation below prepares bounded geometry in its existing coalesced transaction; it is not a production fallback.
//! Reference painting reads that snapshot only; fold state retains canonical intervals and generation authority.

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use super::*;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use objc2_app_kit::{
    NSAttributedStringNSStringDrawing, NSRulerOrientation, NSRulerView,
    NSTextLayoutFragmentEnumerationOptions,
};
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
use omni_markdown_core::markdown_folding::{markdown_fold_region, markdown_line_may_fold};

#[cfg(feature = "native-paged-production")]
mod labels;
#[cfg(feature = "native-paged-production")]
pub(super) mod paged;
#[cfg(feature = "native-raster-surface-probe")]
mod raster;

/// A finite viewport cap bounds ruler metadata even with unusually small or overlapping line heights.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
const MAX_GUTTER_LINES: usize = 256;
/// Explicit nested folds are presentation metadata, bounded independently of document size.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
const MAX_ACTIVE_FOLDS: usize =
    omni_markdown_core::markdown_folding::anchor_mapping::MAX_MARKDOWN_FOLD_ANCHORS;

/// One logical source line has one number, irrespective of how many wrapped visual rows it occupies.
#[derive(Clone, PartialEq)]
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
struct GutterLine {
    number: usize,
    document_y: f64,
    height: f64,
    may_fold: bool,
    folded: bool,
}

/// Caches only this verified fragment's identity and bounded geometry; Weak does not extend TextKit or document lifetime.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
struct GutterFragmentProof {
    fragment: Weak<objc2_app_kit::NSTextLayoutFragment>,
    source: std::ops::Range<usize>,
    frame: NSRect,
}

/// Core generation alone cannot authorize line numbers: scrolling and Find may replace Native geometry without editing text.
/// Each ruler retains at most 256 current-viewport fragment records, without text copies, Native strong owners, or another layout index.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
struct GutterGeometryProof {
    generation: u64,
    geometry_epoch: u64,
    semantic_epoch: Option<u64>,
    font_epoch: u64,
    fold_revision: u64,
    visible: NSRect,
    origin: NSPoint,
    viewport_source: std::ops::Range<usize>,
    manager: Option<Weak<NSTextLayoutManager>>,
    content: Option<Weak<objc2_app_kit::NSTextContentManager>>,
    fragments: Vec<GutterFragmentProof>,
}

#[cfg(feature = "native-raster-surface-probe")]
thread_local! {
    // Limits failure diagnostics to sixteen main-thread observations, retaining no view, text, Native object, or successor scheduling.
    static GUTTER_STALE_DIAGNOSTIC_COUNT: Cell<u8> = const { Cell::new(0) };
}

/// Reuses fixed scalars already read by validation; None denotes an unvisited short-circuit branch, without extra diagnostic getters.
#[derive(Clone, Copy, Debug, Default)]
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
struct GutterProofDiagnostic {
    epochs: Option<(u64, u64, Option<u64>, u64, u64)>,
    viewport: Option<(NSRect, NSPoint)>,
    source: Option<(usize, usize)>,
    fragment_index: Option<usize>,
    expected_source: Option<(usize, usize)>,
    state: Option<usize>,
    frame: Option<NSRect>,
    expected_frame: Option<NSRect>,
    identity_matches: Option<bool>,
}

/// Reports only the original failure; disabling trace changes no proof, counter, or Native call.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
fn report_gutter_proof_failure(
    site: &'static str,
    reason: &'static str,
    operation: &'static str,
    proof: &GutterGeometryProof,
    facts: GutterProofDiagnostic,
) {
    #[cfg(feature = "native-raster-surface-probe")]
    native_markdown_trace! {
        let ordinal = GUTTER_STALE_DIAGNOSTIC_COUNT.with(|count| {
            if count.get() == 16 { return None; } // Exhausting diagnostics stops logging only, without manufacturing success or retries.
            let next = count.get() + 1;
            count.set(next);
            Some(next)
        });
        if let Some(ordinal) = ordinal {
            eprintln!("native_raster_surface event=gutter_proof_stale ordinal={ordinal} site={site} reason={reason} operation={operation} expected_epochs_g_geometry_semantic_font_fold={:?} expected_viewport={:?} expected_origin={:?} expected_source={}..{} observed={facts:?} formal=false",
                (proof.generation, proof.geometry_epoch, proof.semantic_epoch, proof.font_epoch, proof.fold_revision), proof.visible, proof.origin, proof.viewport_source.start, proof.viewport_source.end);
        }
    }
    #[cfg(not(feature = "native-raster-surface-probe"))]
    let _ = (site, reason, operation, proof, facts); // Ordinary builds add no diagnostic counter, output, or Native observation.
}

#[cfg(feature = "native-raster-surface-probe")]
/// Bounded facts from one original label loop contain geometry/counts only, without text, label strings, or another row array.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
struct GutterDrawFacts {
    dirty: NSRect,
    viewport: NSRect,
    first_line: Option<(usize, f64, f64, f64)>,
    outside_viewport: usize,
    outside_damage: usize,
}

/// Only the scroll surface owns the ruler; the backlink never prolongs its editor or canonical model.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(super) struct GutterIvars {
    text_view: Weak<PieceBackedMarkdownTextView>,
    lines: RefCell<Vec<GutterLine>>,
    // Lines and proof change in one publication segment without Native calls; failed refresh revokes the proof rather than renewing old rows.
    geometry: RefCell<Option<Rc<GutterGeometryProof>>>,
    attributes: RefCell<Option<Rc<Retained<MarkdownTextAttributes>>>>,
    // Fixes the flag before super initialization; uses the CPU-layer path only when the actual document Raster exists.
    #[cfg(feature = "native-raster-surface-probe")]
    raster_enabled: bool,
    #[cfg(feature = "native-raster-surface-probe")]
    raster: RefCell<Option<Rc<raster::GutterRaster>>>,
}

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
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

        #[cfg(feature = "native-raster-surface-probe")]
        #[unsafe(method(wantsUpdateLayer))]
        /// Uses actual CALayer.contents only with the same original document Raster; ordinary reference drawing stays with NSRulerView.
        fn wants_update_layer(&self) -> bool {
            if self.ivars().raster_enabled {
                true
            } else {
                // SAFETY: The public NSView getter ABI matches this override, whose receiver already has fully initialized ivars.
                unsafe { msg_send![super(self), wantsUpdateLayer] }
            }
        }

        #[cfg(feature = "native-raster-surface-probe")]
        #[unsafe(method(updateLayer))]
        /// An actual AppKit damage event triggers one bounded CPU paint; cancellation/failure neither certifies completion nor schedules retries.
        fn update_layer(&self) {
            if !self.ivars().raster_enabled {
                // SAFETY: Without the auxiliary canvas, forwards the public NSView lifecycle method unchanged.
                unsafe { let _: () = msg_send![super(self), updateLayer]; }
                return;
            }
            self.paint_raster_layer("updateLayer");
        }

        #[cfg(feature = "native-raster-surface-probe")]
        #[unsafe(method(drawRect:))]
        /// The layer entry avoids a window accelerated-CG display list; original drawRect runs only within the CPU context.
        fn draw_rect(&self, dirty: NSRect) {
            if self.ivars().raster_enabled {
                self.paint_raster_layer("drawRect");
            } else {
                // SAFETY: The ordinary reference path preserves NSRulerView's public drawing ABI and original damage range.
                unsafe { let _: () = msg_send![super(self), drawRect: dirty]; }
            }
        }

        #[cfg(feature = "native-raster-surface-probe")]
        #[unsafe(method(setFrame:))]
        /// Actual geometry events revoke the document's shared geometry intent before the native setter.
        fn set_frame_for_raster(&self, frame: NSRect) {
            self.invalidate_raster_geometry();
            // SAFETY: The original NSView setter receives the same NSRect ABI without parameter or layout-policy changes.
            unsafe { let _: () = msg_send![super(self), setFrame: frame]; }
        }

        #[cfg(feature = "native-raster-surface-probe")]
        #[unsafe(method(setBounds:))]
        /// Actual scroll bounds changes revoke publication authority for old CPU images.
        fn set_bounds_for_raster(&self, bounds: NSRect) {
            self.invalidate_raster_geometry();
            // SAFETY: Matches the public NSView setter ABI and forwards original geometry unchanged.
            unsafe { let _: () = msg_send![super(self), setBounds: bounds]; }
        }

        #[cfg(feature = "native-raster-surface-probe")]
        #[unsafe(method(viewDidChangeBackingProperties))]
        /// A screen-scale change revokes the intent; old pixel resolution cannot become new backing.
        fn backing_changed_for_raster(&self) {
            self.invalidate_raster_geometry();
            // SAFETY: The public NSView notification takes no arguments and adds no observer or hidden view.
            unsafe { let _: () = msg_send![super(self), viewDidChangeBackingProperties]; }
        }

        #[cfg(feature = "native-raster-surface-probe")]
        #[unsafe(method(viewDidMoveToWindow))]
        /// Moving the same ruler to another window revokes its intent instead of assigning successor identity to in-flight paint.
        fn window_changed_for_raster(&self) {
            self.invalidate_raster_geometry();
            // SAFETY: Preserves the public NSView lifecycle order and ABI.
            unsafe { let _: () = msg_send![super(self), viewDidMoveToWindow]; }
        }

        #[unsafe(method(drawHashMarksAndLabelsInRect:))]
        /// Paints cached line numbers only inside the same live Source viewport, without parsing text or triggering layout.
        fn draw_hash_marks_and_labels(&self, dirty: NSRect) {
            #[cfg(feature = "native-raster-surface-probe")]
            if self.ivars().raster_enabled && !self.raster_draw_is_active() {
                return; // Only this actual CPU super drawRect may consume labels, without bypassing it through the window GPU context.
            }
            let Some(Some(view)) = self.paint_native_step(|| self.ivars().text_view.load()) else { return; };
            let _payload_use = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
            if view.ivars().textkit_graph_teardown.get() { return; }
            let Ok(capture) = view.ivars().folds.capture_selection() else { return; };
            let Some(proof) = self.ivars().geometry.borrow().clone() else { return; };
            if !self.cached_geometry_is_current(&view, &capture, &proof, "draw_begin") { return; }
            let Some(attributes) = self.ivars().attributes.borrow().clone() else { return; };
            let Some((viewport, generation)) = self.source_viewport(&view) else { return; };
            let Some(damage) = native_markdown_paint_rect(viewport, dirty, viewport) else { return; };
            let Some(active_line) = self.paint_native_step(|| view.selected_source_line()) else { return; };
            // Clones the existing bounded cache before reentrant Native coordinate conversion and label drawing, with no cache borrow held.
            let lines = self.ivars().lines.borrow().clone();
            #[cfg(feature = "native-markdown-runtime-probe")]
            crate::native_surface::probe_call_diagnostic::memory_point("gutter_draw_begin");
            #[cfg(feature = "native-raster-surface-probe")]
            let mut facts = GutterDrawFacts {
                dirty, viewport,
                first_line: None, outside_viewport: 0, outside_damage: 0,
            };
            let Some(_graphics) = self.paint_native_step(GutterGraphicsState::save) else { return; };
            if self.paint_native_step(|| NSBezierPath::clipRect(damage)).is_none() { return; }
            for line in lines {
                // Converts original document baselines rather than moving a long logical line's number to the viewport edge.
                let Some(point) = self.paint_native_step(|| self.convertPoint_fromView(
                    NSPoint::new(0.0, line.document_y), Some(&view))) else { return; };
                if self.source_viewport(&view) != Some((viewport, generation)) { return; }
                #[cfg(feature = "native-raster-surface-probe")]
                if facts.first_line.is_none() {
                    facts.first_line = Some((line.number, line.document_y, point.y, line.height));
                }
                let Some(rect) = visible_gutter_line(point.y, line.height, viewport) else {
                    #[cfg(feature = "native-raster-surface-probe")]
                    { facts.outside_viewport += 1; }
                    continue; // Offscreen logical lines do not move to the viewport top; their original fold-hit coordinates remain authoritative.
                };
                let Some(paint) = native_markdown_paint_rect(rect, dirty, viewport) else {
                    #[cfg(feature = "native-raster-surface-probe")]
                    { facts.outside_damage += 1; }
                    continue; // Ordinary damage drawing retains the existing local-update semantics.
                };
                if active_line == Some(line.number) {
                    // Uses the same actual active-line intersection; RAII balances graphics state on every revoked exit.
                    let Some(_highlight) = self.paint_native_step(GutterGraphicsState::save) else { return; };
                    let Some(color) = self.paint_native_step(|| NSColor::colorWithSRGBRed_green_blue_alpha(0.35, 0.48, 0.72, 0.12)) else { return; };
                    if self.paint_native_step(|| color.setFill()).is_none()
                        || self.paint_native_step(|| NSBezierPath::fillRect(paint)).is_none() { return; }
                }
                let marker = if line.folded {
                    "▶" // A folded line shows the existing expansion marker; clicks still require Core fold authority.
                } else if line.may_fold {
                    "▾" // A visual hint alone grants no authority to hide text.
                } else {
                    " " // An ordinary line retains its number only.
                };
                let Some(string) = self.paint_native_step(|| NSString::from_str(&format!("{} {marker}", line.number + 1))) else { return; };
                // SAFETY: The original dictionary contains live fonts/colors and immutable keys only; bounded temporary labels return synchronously.
                let Some(label) = self.paint_native_step(|| unsafe {
                    NSAttributedString::initWithString_attributes(NSAttributedString::alloc(), &string, Some(&attributes))
                }) else { return; };
                if self.source_viewport(&view) != Some((viewport, generation)) { return; }
                if self.paint_native_step(|| label.drawAtPoint(NSPoint::new(4.0, point.y))).is_none() { return; }
                #[cfg(feature = "native-raster-surface-probe")]
                if let Some(raster) = self.ivars().raster.borrow().as_ref() {
                    raster.record_drawn_label(); // Records a draw only after the actual label call returns under the original intent.
                }
            }
            // Rechecks the fragment batch once after the complete loop, avoiding quadratic rescans per label or Native primitive.
            if !self.cached_geometry_is_current(&view, &capture, &proof, "draw_end") { return; }
            drop(_graphics); // Restores graphics state before recording the original draw-end boundary.
            #[cfg(feature = "native-raster-surface-probe")]
            self.complete_raster_draw(facts);
            #[cfg(feature = "native-markdown-runtime-probe")]
            crate::native_surface::probe_call_diagnostic::memory_point("gutter_draw_end");
        }

        #[unsafe(method(mouseDown:))]
        /// Hits cached logical lines only in the same Source visible area; occluded regions have no fold target.
        fn mouse_down(&self, event: &NSEvent) {
            let Some(view) = self.ivars().text_view.load() else {
                // The original weak client may have ended before AppKit delivers a queued event.
                return;
            };
            // Hit-testing retains geometry across AppKit calls; the full operation prevents accepted payload release.
            let _payload_use = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
            if view.ivars().textkit_graph_teardown.get() {
                // A late click cannot parse old fold regions or commit a draft.
                return;
            }
            let Ok(capture) = view.ivars().folds.capture_selection() else { return; };
            let Some(proof) = self.ivars().geometry.borrow().clone() else { return; };
            if !self.cached_geometry_is_current(&view, &capture, &proof, "mouse_begin") { return; }
            let Some((viewport, generation)) = self.source_viewport(&view) else {
                return; // Preview, closed, or unmounted rulers cannot consume an old fold event.
            };
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            if !gutter_contains_point(viewport, point) {
                return; // Shares painting's document clip, leaving no invisible hit regions in Find/header areas.
            }
            let lines = self.ivars().lines.borrow().clone();
            let selected = lines.into_iter().find(|line| {
                // Preserves a long logical line's original hit height, clipping only actually invisible portions.
                let top = self
                    .convertPoint_fromView(NSPoint::new(0.0, line.document_y), Some(&view))
                    .y;
                visible_gutter_line(top, line.height, viewport)
                    .is_some_and(|rect| gutter_contains_point(rect, point))
            });
            if self.source_viewport(&view) != Some((viewport, generation))
                || !self.cached_geometry_is_current(&view, &capture, &proof, "mouse_end") {
                // Conversion reentry into layout, a new document generation, or close prevents the old point from targeting successor geometry.
                return;
            }
            if let Some(line) = selected {
                // Core rechecks generation and actual syntax after normal draft commit; the click cannot bypass the existing fold transaction.
                if let Err(error) = view.toggle_source_fold(line.number) {
                    // Capacity, commit, or syntax refusal preserves original text and existing folds.
                    present_native_markdown_error(&error);
                }
            }
        }
    }
);

/// Balances the original synchronous graphics stack even after revocation, without leaking clip state to another control.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
struct GutterGraphicsState;
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
impl GutterGraphicsState {
    fn save() -> Self {
        NSGraphicsContext::saveGraphicsState_class();
        Self
    }
}
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
impl Drop for GutterGraphicsState {
    fn drop(&mut self) {
        NSGraphicsContext::restoreGraphicsState_class();
    }
}

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
impl NativeMarkdownGutter {
    #[cfg(feature = "native-raster-surface-probe")]
    /// Both public Native entries share an ordinary Rust boundary, rather than directly calling selector-bearing override implementations.
    fn paint_raster_layer(&self, entry: &'static str) {
        let raster = self.ivars().raster.borrow().clone();
        if let Some(raster) = raster
            && let Err(error) = raster.paint(self, entry)
        {
            raster.report_failure(&error);
        } // An uninstalled construction lease or accepted teardown grants no access to an old Native graph.
    }

    /// CPU callbacks guard each primitive with the same original intent; ordinary reference drawing retains its original Native calls.
    fn paint_native_step<T>(&self, work: impl FnOnce() -> T) -> Option<T> {
        #[cfg(feature = "native-raster-surface-probe")]
        if self.ivars().raster_enabled {
            let raster = self.ivars().raster.borrow().clone();
            if let Some(raster) = raster
                && raster.active()
            {
                return raster.drawing_step(self, work);
            }
        }
        Some(work()) // Original mouse hits and initial geometry reads create no additional CPU paint transaction.
    }

    #[cfg(feature = "native-raster-surface-probe")]
    fn complete_raster_draw(&self, facts: GutterDrawFacts) {
        let raster = self.ivars().raster.borrow().clone();
        if let Some(raster) = raster {
            raster.mark_draw_complete(self, facts);
        }
    }

    #[cfg(feature = "native-raster-surface-probe")]
    fn raster_draw_is_active(&self) -> bool {
        self.ivars()
            .raster
            .borrow()
            .as_ref()
            .is_some_and(|raster| raster.active())
    }

    #[cfg(feature = "native-raster-surface-probe")]
    fn invalidate_raster_geometry(&self) {
        let raster = self.ivars().raster.borrow().clone();
        if let Some(raster) = raster {
            raster.invalidate_geometry();
        }
    }

    #[cfg(feature = "native-raster-surface-probe")]
    fn draw_native_for_raster(&self, bounds: NSRect) {
        // SAFETY: Calls public NSRulerView drawing, which still dispatches original drawHashMarks inside the established CPU context.
        unsafe {
            let _: () = msg_send![super(self), drawRect: bounds];
        }
    }

    /// Pure Rust identity checks may bracket each Native primitive without rescanning fragments or upgrading document ownership.
    fn proof_is_current(&self, proof: &Rc<GutterGeometryProof>) -> bool {
        self.ivars()
            .geometry
            .borrow()
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, proof))
    }

    /// Checks the original batch at paint/hit admission and complete draw exit; the Raster intent still revokes geometry during work.
    /// Brackets every Weak/Native query with original selection/proof identity checks, without RefCell borrows across calls.
    fn cached_geometry_is_current(
        &self,
        view: &PieceBackedMarkdownTextView,
        capture: &NativeMarkdownSelectionCapture,
        proof: &Rc<GutterGeometryProof>,
        site: &'static str,
    ) -> bool {
        let failure = Cell::new(None);
        let operation = Cell::new("begin");
        let facts = Cell::new(GutterProofDiagnostic::default());
        // Records the first false from original short-circuit evaluation only; later cleanup/checks cannot replace that failure reason.
        let predicate = |name: &'static str, value: bool| {
            if !value && failure.get().is_none() {
                failure.set(Some(name));
            }
            value
        };
        // The macro labels original calls only, preserving their guard, getter, and .ok()? short-circuit behavior.
        macro_rules! read {
            ($guard:expr, $name:literal, $work:expr) => {{
                operation.set($name);
                native_markdown_table_intent_step($guard, $work).ok()?
            }};
        }
        let result = (|| -> Option<()> {
            let original = || {
                predicate("selection_capture", capture.current())
                    && predicate("proof_identity", self.proof_is_current(proof))
                    && predicate("graph_open", !view.ivars().textkit_graph_teardown.get())
            };
            let storage = read!(&original, "view.storage_weak", || view
                .ivars()
                .text_storage
                .load())?;
            let current = || {
                original()
                    && predicate(
                        "source_mode",
                        storage.presentation_mode() == MarkdownPresentationMode::Source,
                    )
                    && predicate(
                        "geometry_epoch",
                        storage.ivars().table_geometry_epoch.get() == proof.geometry_epoch,
                    )
                    && predicate(
                        "semantic_epoch",
                        storage.ivars().presentation_semantic_epoch.get() == proof.semantic_epoch,
                    )
                    && predicate(
                        "font_epoch",
                        storage.ivars().font_run_scratch.epoch() == proof.font_epoch,
                    )
                    && predicate(
                        "fold_revision",
                        view.ivars().folds.revision() == proof.fold_revision,
                    )
            };
            operation.set("canonical.document");
            let document = storage.document()?;
            let generation = document.borrow().generation();
            drop(document); // Diagnostics reuse the generation already read under the original borrow, without another document upgrade.
            let mut observed = facts.get();
            observed.epochs = Some((
                generation,
                storage.ivars().table_geometry_epoch.get(),
                storage.ivars().presentation_semantic_epoch.get(),
                storage.ivars().font_run_scratch.epoch(),
                view.ivars().folds.revision(),
            ));
            facts.set(observed); // Other epochs are existing pure Cell reads, with no additional Native diagnostic getter.
            if !current() || !predicate("generation", generation == proof.generation) {
                return None;
            }
            let actual_viewport = read!(&current, "view.bounded_paint_viewport", || view
                .bounded_paint_viewport());
            let mut observed = facts.get();
            observed.viewport = actual_viewport;
            facts.set(observed);
            if !predicate(
                "document_viewport",
                actual_viewport == Some((proof.visible, proof.origin)),
            ) {
                return None; // Compares the actual document clip; a ruler's fixed zero origin is not Source evidence.
            }
            let Some(manager_weak) = proof.manager.as_ref() else {
                operation.set("empty_document.shape");
                let document = storage.document()?;
                return (predicate("empty_viewport_source", proof.viewport_source.is_empty())
                    && predicate("empty_document_length", document.borrow().utf16_len() == 0)
                    && predicate("empty_fragment_inventory", proof.fragments.is_empty())
                    && current())
                .then_some(());
            };
            let manager = read!(&current, "proof.manager_weak", || manager_weak.load())?;
            let content = read!(&current, "proof.content_weak", || proof
                .content
                .as_ref()?
                .load())?;
            let actual_manager = read!(&current, "view.textLayoutManager", || view
                .textLayoutManager())?;
            let actual_content = read!(&current, "manager.textContentManager", || manager
                .textContentManager())?;
            if !predicate(
                "manager_identity",
                std::ptr::eq(&*actual_manager, &*manager),
            ) || !predicate(
                "content_identity",
                std::ptr::eq(&*actual_content, &*content),
            ) {
                return None;
            }
            let viewport = read!(&current, "manager.viewportController", || manager
                .textViewportLayoutController());
            let viewport_manager = read!(&current, "viewport.textLayoutManager", || viewport
                .textLayoutManager())?;
            if !predicate(
                "viewport_manager_identity",
                std::ptr::eq(&*viewport_manager, &*manager),
            ) {
                return None;
            }
            let range = read!(&current, "viewport.viewportRange", || viewport
                .viewportRange())?;
            let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
            let document_range = read!(&current, "provider.documentRange", || provider
                .documentRange());
            let start = read!(&current, "documentRange.location", || document_range
                .location());
            let source_range = |range: &NSTextRange| -> Option<std::ops::Range<usize>> {
                let location = read!(&current, "range.location", || range.location());
                let end = read!(&current, "range.endLocation", || range.endLocation());
                let from = usize::try_from(read!(&current, "content.startOffset", || content
                    .offsetFromLocation_toLocation(&start, &location)))
                .ok()?;
                let to = usize::try_from(read!(&current, "content.endOffset", || content
                    .offsetFromLocation_toLocation(&start, &end)))
                .ok()?;
                predicate("source_range_order", from <= to).then_some(from..to)
            };
            let actual_source = source_range(&range)?;
            let mut observed = facts.get();
            observed.source = Some((actual_source.start, actual_source.end));
            facts.set(observed);
            if !predicate(
                "viewport_source_range",
                actual_source == proof.viewport_source,
            ) {
                return None;
            }
            for (index, expected) in proof.fragments.iter().enumerate() {
                let mut observed = facts.get();
                observed.fragment_index = Some(index);
                observed.expected_source = Some((expected.source.start, expected.source.end));
                observed.expected_frame = Some(expected.frame);
                observed.source = None;
                observed.state = None;
                observed.frame = None;
                observed.identity_matches = None;
                facts.set(observed); // None preserves fields skipped by original short-circuiting, without reading a frame merely for diagnostics.
                let fragment = read!(&current, "fragment.weak", || expected.fragment.load())?;
                let state = read!(&current, "fragment.state", || fragment.state());
                let mut observed = facts.get();
                observed.state = Some(state.0);
                facts.set(observed);
                if !predicate(
                    "fragment_state",
                    state == objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable,
                ) {
                    return None;
                }
                let frame = read!(&current, "fragment.layoutFragmentFrame", || fragment
                    .layoutFragmentFrame());
                let mut observed = facts.get();
                observed.frame = Some(frame);
                facts.set(observed);
                if !predicate("fragment_frame", frame == expected.frame) {
                    return None;
                }
                let fragment_manager = read!(&current, "fragment.textLayoutManager", || fragment
                    .textLayoutManager())?;
                if !predicate(
                    "fragment_manager_identity",
                    std::ptr::eq(&*fragment_manager, &*manager),
                ) {
                    return None;
                }
                let fragment_range = read!(&current, "fragment.rangeInElement", || fragment
                    .rangeInElement());
                let actual_source = source_range(&fragment_range)?;
                let mut observed = facts.get();
                observed.source = Some((actual_source.start, actual_source.end));
                facts.set(observed);
                if !predicate("fragment_source_range", actual_source == expected.source) {
                    return None;
                }
                let location = read!(&current, "fragmentRange.location", || fragment_range
                    .location());
                let actual = read!(&current, "manager.fragmentForLocation", || manager
                    .textLayoutFragmentForLocation(&location))?;
                let identity_matches = std::ptr::eq(&*actual, &*fragment);
                let mut observed = facts.get();
                observed.identity_matches = Some(identity_matches);
                facts.set(observed);
                if !predicate("fragment_lookup_identity", identity_matches) {
                    return None;
                }
            }
            current().then_some(()) // Preserves original decisions and Native order; diagnostics neither advance layout nor authorize success.
        })();
        // Rechecks the original intent after Native temporaries return, reusing actual failure without extra observation calls.
        let valid = result.is_some()
            && predicate("selection_after_claim_drop", capture.current())
            && predicate("proof_after_claim_drop", self.proof_is_current(proof));
        if !valid {
            report_gutter_proof_failure(
                site,
                failure.get().unwrap_or("native_result_unavailable"),
                operation.get(),
                proof,
                facts.get(),
            );
        }
        valid
    }

    /// Paint and fold hits share the actual visible area, without cached Find height or manual header offsets.
    fn source_viewport(&self, view: &PieceBackedMarkdownTextView) -> Option<(NSRect, u64)> {
        if view.ivars().textkit_graph_teardown.get() {
            return None; // An accepted empty shell does not upgrade the canonical Weak.
        }
        let proof = self.ivars().geometry.borrow().clone()?;
        let generation = {
            let storage = self.paint_native_step(|| view.ivars().text_storage.load())??;
            if storage.presentation_mode() != MarkdownPresentationMode::Source
                || storage.ivars().table_geometry_epoch.get() != proof.geometry_epoch
                || storage.ivars().presentation_semantic_epoch.get() != proof.semantic_epoch
                || storage.ivars().font_run_scratch.epoch() != proof.font_epoch
                || view.ivars().folds.revision() != proof.fold_revision
            {
                return None; // Late hidden-ruler drawing/events cannot borrow Preview to restore Source authority.
            }
            let document = storage.document()?;
            document.borrow().generation()
        }; // Canonical borrowing and temporary strong ownership end before coordinate conversion.
        let (text_visible, origin) = self.paint_native_step(|| view.bounded_paint_viewport())??;
        if generation != proof.generation
            || text_visible != proof.visible
            || origin != proof.origin
            || !self.proof_is_current(&proof)
        {
            return None; // A new scroll/Find viewport in the same text generation cannot renew old cache paint/fold-hit authority.
        }
        // The ruler owns its horizontal strip; converts only document clip edges while actual hierarchy accounts for Find/resize offsets.
        let top = self
            .paint_native_step(|| {
                self.convertPoint_fromView(NSPoint::new(0.0, text_visible.origin.y), Some(view))
            })?
            .y;
        let bottom = self
            .paint_native_step(|| {
                self.convertPoint_fromView(
                    NSPoint::new(0.0, text_visible.origin.y + text_visible.size.height),
                    Some(view),
                )
            })?
            .y;
        let bounds = self.paint_native_step(|| self.bounds())?;
        let vertical_clip = NSRect::new(
            NSPoint::new(bounds.origin.x, top.min(bottom)),
            NSSize::new(bounds.size.width, (bottom - top).abs()),
        );
        let visible = self.paint_native_step(|| self.visibleRect())?;
        let viewport = native_markdown_paint_rect(visible, bounds, vertical_clip)?;
        // Rechecks generation and Source after reentrant getters; invalid/empty intersections never use whole-document bounds instead.
        self.paint_native_step(|| view.gutter_generation_is_current(generation))?
            .then_some((viewport, generation))
    }

    /// One scroll surface mounts one reference ruler with bounded fonts/labels and a weak editor; auxiliary-layer construction failure is explicit.
    pub(super) fn new(
        scroll: &NSScrollView,
        view: &PieceBackedMarkdownTextView,
        ledger: &NativeMarkdownOwnedLedger,
        mtm: MainThreadMarker,
    ) -> Result<Retained<Self>, String> {
        #[cfg(feature = "native-raster-surface-probe")]
        let raster_owner = view.ivars().raster_surface.borrow().clone();
        #[cfg(not(feature = "native-raster-surface-probe"))]
        let _ = ledger; // Ordinary reference builds create no auxiliary layer or second ledger.
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
            geometry: RefCell::new(None),
            // Drawing clones Rust Rc only; Native retain/release never overlaps a RefCell borrow.
            attributes: RefCell::new(Some(Rc::new(attributes))),
            #[cfg(feature = "native-raster-surface-probe")]
            raster_enabled: raster_owner.is_some(),
            #[cfg(feature = "native-raster-surface-probe")]
            raster: RefCell::new(None),
        });
        // SAFETY: the NSRulerView designated initializer receives the live main-thread scroll owner
        // after every Rust ivar is initialized; the declared selector matches its Objective-C ABI.
        let ruler: Retained<Self> = unsafe {
            msg_send![super(allocated), initWithScrollView: Some(scroll), orientation: NSRulerOrientation::VerticalRuler]
        };
        // Explicitly clips ruler bounds under modern AppKit; the separate document clip still constrains painting and clicks.
        ruler.setClipsToBounds(true);
        ruler.setRuleThickness(74.0);
        ruler.setClientView(Some(view));
        ruler.setAccessibilityLabel(Some(ns_string!("源码行号与折叠控件")));
        #[cfg(feature = "native-raster-surface-probe")]
        if let Some(owner) = raster_owner {
            let raster = raster::GutterRaster::install(&ruler, view, &owner, ledger)?;
            *ruler.ivars().raster.borrow_mut() = Some(raster);
        }
        Ok(ruler)
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    /// Check bounded visible metadata without exposing or copying its source-line records.
    pub(super) fn verify_visible_labels_for_probe(&self) -> Result<(), String> {
        let count = self.ivars().lines.borrow().len();
        if count == 0 || count > MAX_GUTTER_LINES {
            // Missing labels or an exceeded viewport cap invalidates the actual native gutter contract.
            return Err(format!("源码行号的可见标签数量无效：{count}"));
        }
        Ok(())
    }

    /// Read current cached paragraph geometry for one logical source line, without native layout work.
    fn line_frame(
        &self,
        view: &PieceBackedMarkdownTextView,
        line: usize,
        width: f64,
    ) -> Option<NSRect> {
        let capture = view.ivars().folds.capture_selection().ok()?;
        let proof = self.ivars().geometry.borrow().clone()?;
        if !self.cached_geometry_is_current(view, &capture, &proof, "active_line_background") {
            return None; // Document active-line background cannot consume old gutter geometry from another viewport either.
        }
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

    /// Checks each payload slot to prove the ruler shell retains no geometry capacity or attribute ownership.
    pub(super) fn payload_is_empty(&self) -> bool {
        let metadata_empty = self.ivars().lines.borrow().capacity() == 0
            && self.ivars().geometry.borrow().is_none()
            && self.ivars().attributes.borrow().is_none();
        // Ends every temporary RefCell borrow before the clientView getter, allowing reentrant close observation.
        #[cfg(feature = "native-raster-surface-probe")]
        let metadata_empty = metadata_empty && self.ivars().raster.borrow().is_none();
        metadata_empty && self.clientView().is_none()
    }

    /// Drop bounded geometry immediately when detaching; the scroll view separately releases its ruler.
    pub(super) fn clear(&self) {
        #[cfg(feature = "native-raster-surface-probe")]
        {
            let raster = self.ivars().raster.borrow_mut().take();
            if let Some(raster) = raster {
                // Takes the sole slot before reentrant Native calls, restoring original relationships and returning auxiliary metadata.
                if let Err(error) = raster.restore() {
                    raster.report_failure(&error);
                }
                drop(raster);
            }
        }
        let lines = self.ivars().lines.replace(Vec::new());
        let geometry = self.ivars().geometry.borrow_mut().take();
        let attributes = self.ivars().attributes.borrow_mut().take();
        // Releases capacity and Native values after all borrows end, before reentrant client detachment.
        drop((lines, geometry, attributes));
        self.setClientView(None);
    }
}

/// Clips only offscreen portions of the original logical line; labels keep their original baseline rather than moving to the viewport top.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
fn visible_gutter_line(top: f64, height: f64, viewport: NSRect) -> Option<NSRect> {
    if !top.is_finite() || !height.is_finite() {
        return None; // Unverified Native line geometry grants no paint or fold-hit authority.
    }
    let line = NSRect::new(
        NSPoint::new(viewport.origin.x, top),
        NSSize::new(viewport.size.width, height.max(16.0)),
    );
    native_markdown_paint_rect(line, viewport, viewport)
}

/// Uses the same half-open visible region so edge events cannot fall into neighboring Find/header controls.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
fn gutter_contains_point(rect: NSRect, point: NSPoint) -> bool {
    point.x.is_finite()
        && point.y.is_finite()
        && point.x >= rect.origin.x
        && point.x < rect.origin.x + rect.size.width
        && point.y >= rect.origin.y
        && point.y < rect.origin.y + rect.size.height
}

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
impl PieceBackedMarkdownTextView {
    /// Revalidates derived data against the same live canonical generation after TextKit/coordinate calls.
    pub(super) fn projection_generation_is_current(&self, expected: u64) -> bool {
        let sealed = self.ivars().textkit_graph_teardown.get();
        let current = if sealed {
            // A sealed graph does not even upgrade canonical Weak, avoiding any business-lifetime extension.
            None
        } else {
            self.ivars()
                .text_storage
                .load()
                .and_then(|storage| storage.document())
                .map(|document| document.borrow().generation())
        };
        projection_can_publish(sealed, expected, current)
    }

    /// The ruler belongs to Source only; switching to Preview during Native callbacks revokes old geometry publication.
    fn gutter_generation_is_current(&self, expected: u64) -> bool {
        self.projection_generation_is_current(expected)
            && self.ivars().text_storage.load().is_some_and(|storage| {
                storage.presentation_mode() == MarkdownPresentationMode::Source
            })
    }

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
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if self.ivars().textkit_graph_teardown.get() {
            // This separate drawing entry also cannot acquire text or attributes after acceptance.
            return;
        }
        let Some(line) = self.selected_source_line() else {
            // Preview/disconnected input has no source-line background to draw.
            return;
        };
        let Some(ruler) = self.ivars().gutter.get().and_then(Weak::load) else {
            // Native storage-only adapters have no prepared source-line geometry.
            return;
        };
        let Some((visible, _)) = self.bounded_paint_viewport() else {
            return; // An initially hidden/unmounted view draws nothing; actual mount/draw events resume through existing scheduling.
        };
        let Some(frame) = ruler.line_frame(self, line, self.bounds().size.width) else {
            // Offscreen/unlaid caret geometry is deferred; drawing never forces whole-document layout.
            return;
        };
        if let Some(paint) = native_markdown_paint_rect(frame, dirty, visible) {
            // The subtle native source highlight uses the same one-line range as the gutter.
            NSGraphicsContext::saveGraphicsState_class();
            NSColor::colorWithSRGBRed_green_blue_alpha(0.35, 0.48, 0.72, 0.08).setFill();
            NSBezierPath::fillRect(paint);
            NSGraphicsContext::restoreGraphicsState_class();
        }
    }

    /// Mark only existing ruler backing as damaged after a settled selection change.
    pub(super) fn invalidate_source_gutter_highlight(&self) {
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if self.ivars().textkit_graph_teardown.get() {
            // Late selection notifications cannot request drawing on an empty shell.
            return;
        }
        if let Some(ruler) = self.ivars().gutter.get().and_then(Weak::load) {
            // Selection changes reuse cached label geometry and create no timer or extra layout request.
            ruler.setNeedsDisplay(true);
        }
    }

    /// Explicit expand-all, mode change, and teardown retain their clearing policy; ordinary edits map through actual ChangeSet.
    pub(super) fn unfold_source(&self) {
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if self.ivars().textkit_graph_teardown.get() {
            // Old input entries cannot repopulate fold state already taken and dropped.
            return;
        }
        if self.ivars().folds.borrow_mut().is_empty() {
            // The normal unfolded editor needs no layout invalidation or native transaction.
            return;
        }
        self.ivars().folds.borrow_mut().clear();
        self.ivars().folds.advance(); // Explicit unfolding revokes authority; invalidation does not advance the commit intent again.
        self.invalidate_fold_layout();
    }

    /// Invalidate only derived layout; canonical bytes, selection history and saved baseline are untouched.
    fn invalidate_fold_layout(&self) {
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if self.ivars().textkit_graph_teardown.get() || !self.ivars().folds.owner_is_current() {
            // Native layout requires the same live owner and cannot ask a detached shell to restore derived state.
            return;
        }
        let Ok(capture) = self.ivars().folds.capture_selection() else {
            return;
        };
        // After confirming the owner/capture, this invalidation owns current damage; a prior finish take makes another take a no-op.
        // Takes damage before the first Native release/call only; later damage created by reentry must remain on return.
        self.ivars().folds.take_layout_dirty();
        self.advance_block_fold_epoch(); // Fold changes within one text generation also revoke old derived semantics.
        let _ = (|| -> Result<(), NativeMarkdownFoldError> {
            // Native cache release and getters may reenter; advances only under the fold intent captured at entry.
            selection_native_step(&capture, || self.invalidate_visible_table_layout_cache())?;
            let Some(manager) = selection_native_step(&capture, || self.textLayoutManager())?
            else {
                return Ok(());
            };
            let Some(content) = selection_native_step(&capture, || manager.textContentManager())?
            else {
                return Ok(());
            };
            let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
            let range = selection_native_step(&capture, || provider.documentRange())?;
            selection_native_step(&capture, || manager.invalidateLayoutForRange(&range))?;
            selection_native_step(&capture, || self.setNeedsDisplay(true))?;
            selection_native_step(&capture, || self.schedule_visible_table_layout_update())?;
            Ok(())
        })(); // After revocation, new state owns drawing; no intent recapture or retry is scheduled.
    }

    /// Committed edits first update pure fold coordinates; derived Native invalidation runs once after document borrowing ends.
    pub(super) fn finish_fold_layout_update(&self) {
        if self.ivars().folds.take_layout_dirty() {
            self.invalidate_fold_layout();
        }
    }

    /// Resolve a single explicit user fold while preserving the canonical source and full text coordinates.
    pub(super) fn toggle_source_fold(&self, line: usize) -> Result<(), String> {
        if !self.ivars().folds.owner_is_current() {
            return Err("笔记折叠权限已变更".into());
        }
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if self.ivars().textkit_graph_teardown.get() {
            // Probes and other direct callers obey the same sealed boundary.
            return Err("笔记折叠控件已关闭".into());
        }
        self.commit_active_table_cell()?;
        self.commit_composition_before_release()?;
        if self
            .ivars()
            .text_storage
            .load()
            .is_none_or(|storage| storage.presentation_mode() != MarkdownPresentationMode::Source)
        {
            // Folding belongs to literal source mode; preview retains its normal inline/table presentation.
            return Err("请切换到源码模式后折叠文档块".into());
        }
        let document = self
            .ivars()
            .text_storage
            .load()
            .and_then(|storage| storage.document())
            .ok_or("编辑器已关闭")?;
        let generation = document.borrow().generation();
        if self.ivars().folds.generation.get() != generation {
            // An intervening canonical edit invalidates every saved source coordinate.
            self.ivars().folds.synchronize_generation(generation);
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
            self.ivars().folds.advance();
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
                return Err("请先展开折叠块，再添加新的折叠".into());
            }
            let selected = self
                .primary_text_selection()
                .map_err(|error| error.to_string())?;
            if !selected.belongs_to(&document) || !selected.current_in(&document.borrow()) {
                return Err("折叠前文本选择或笔记已变更".into());
            }
            let move_to_header = (selected.selection.head_utf16 > region.hidden_utf16_range.start
                && selected.selection.head_utf16 < region.hidden_utf16_range.end)
                .then_some(region.hidden_utf16_range.start);
            // Publishes this bounded choice in Rust before the Native setter; reentry cannot append the old region afterward.
            self.ivars().folds.borrow_mut().push(region);
            self.ivars().folds.advance();
            if let Some(header) = move_to_header {
                self.set_semantic_native_selection(MarkdownTextSelection::caret(header));
            }
        }
        self.invalidate_fold_layout();
        Ok(())
    }

    /// Rebuild at most one viewport of logical line labels from existing TextKit 2 fragment geometry.
    pub(super) fn refresh_source_gutter(&self) {
        self.finish_fold_layout_update();
        // All callers, including direct probes, cover the full canonical Rc and enumeration-block lifetime.
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if self.ivars().textkit_graph_teardown.get() {
            // Late refresh cannot reconstruct Vec capacity already released by cleanup.
            return;
        }
        let Some(ruler) = self.ivars().gutter.get().and_then(Weak::load) else {
            // Isolated storage/probe adapters may intentionally have no scroll-owned ruler.
            return;
        };
        // Refresh revokes the previous batch proof immediately; failed same-generation viewport preparation cannot renew old rows for paint or hits.
        // Takes Weak metadata out before dropping it; subsequent Native calls carry no slot borrow.
        let previous_geometry = ruler.ivars().geometry.borrow_mut().take();
        drop(previous_geometry);
        if self
            .ivars()
            .text_storage
            .load()
            .is_none_or(|storage| storage.presentation_mode() != MarkdownPresentationMode::Source)
        {
            // Preview has no ruler labels or fold controls, matching its uncluttered baseline presentation.
            let previous = ruler.ivars().lines.replace(Vec::new());
            drop(previous);
            return;
        }
        let Some(storage) = self.ivars().text_storage.load() else {
            return;
        };
        let Some(document) = storage.document() else {
            // A disconnected document has neither Source authority nor permission to retain hittable old labels.
            let previous = ruler.ivars().lines.replace(Vec::new());
            drop(previous);
            return;
        };
        let (document_length, generation) = {
            let document = document.borrow();
            (document.utf16_len(), document.generation())
        };
        if self.ivars().folds.generation.get() != generation {
            // Only an unknown generation outside the observer flow unfolds; committed mappings retain their matching-generation authority.
            self.ivars().folds.synchronize_generation(generation);
        }
        let Ok(capture) = self.ivars().folds.capture_selection() else {
            return; // Refresh consumes the original foreground note intent, without acquiring a newer owner after Native callbacks.
        };
        let geometry_epoch = storage.ivars().table_geometry_epoch.get();
        let semantic_epoch = storage.ivars().presentation_semantic_epoch.get();
        let font_epoch = storage.ivars().font_run_scratch.epoch();
        let fold_revision = self.ivars().folds.revision();
        let current = || {
            capture.current()
                && !self.ivars().textkit_graph_teardown.get()
                && storage.presentation_mode() == MarkdownPresentationMode::Source
                && storage.ivars().table_geometry_epoch.get() == geometry_epoch
                && storage.ivars().presentation_semantic_epoch.get() == semantic_epoch
                && storage.ivars().font_run_scratch.epoch() == font_epoch
                && self.ivars().folds.revision() == fold_revision
        };
        let is_empty = document_length == 0;
        if is_empty {
            // An empty note still has one first line, without fragment requests or fabricated Source coverage.
            let Ok(Some((visible, origin))) =
                native_markdown_table_intent_step(&current, || self.bounded_paint_viewport())
            else {
                return;
            };
            let prepared = vec![GutterLine {
                number: 0,
                document_y: origin.y,
                height: 20.0,
                may_fold: false,
                folded: false,
            }];
            let proof = Rc::new(GutterGeometryProof {
                generation,
                geometry_epoch,
                semantic_epoch,
                font_epoch,
                fold_revision,
                visible,
                origin,
                viewport_source: 0..0,
                manager: None,
                content: None,
                fragments: Vec::new(),
            });
            if !publish_gutter_lines(&ruler.ivars().lines, prepared, current()) {
                return;
            }
            // Publication makes no Native call, so reentry cannot observe new lines paired with an old proof.
            *ruler.ivars().geometry.borrow_mut() = Some(proof);
            ruler.setNeedsDisplay(true);
            return;
        }
        let prepared = (|| -> Option<(Vec<GutterLine>, Rc<GutterGeometryProof>)> {
            // Each Native getter checks the same original intent before and after; document borrows span neither Native calls nor synchronous enumeration.
            let manager =
                native_markdown_table_intent_step(&current, || self.textLayoutManager()).ok()??;
            let registered = native_markdown_table_intent_step(&current, || {
                self.ivars().text_layout_manager.get().and_then(Weak::load)
            })
            .ok()??;
            if !std::ptr::eq(&*manager, &*registered) {
                return None; // Another manager with equal length/coordinates cannot become this ruler's source.
            }
            let content =
                native_markdown_table_intent_step(&current, || manager.textContentManager())
                    .ok()??;
            let actual_content =
                native_markdown_table_intent_step(&current, || self.textContentStorage())
                    .ok()??;
            if !std::ptr::eq::<objc2_app_kit::NSTextContentManager>(
                &*content,
                actual_content.as_super(),
            ) {
                return None; // Requires actual original view/content/storage identities instead of inferring relationships from coordinates.
            }
            let actual_storage =
                native_markdown_table_intent_step(&current, || actual_content.textStorage())
                    .ok()??;
            if !std::ptr::eq::<NSTextStorage>(&*actual_storage, storage.as_super()) {
                return None;
            }
            let (visible, origin) =
                native_markdown_table_intent_step(&current, || self.bounded_paint_viewport())
                    .ok()??;
            let viewport = native_markdown_table_intent_step(&current, || {
                manager.textViewportLayoutController()
            })
            .ok()?;
            let viewport_manager =
                native_markdown_table_intent_step(&current, || viewport.textLayoutManager())
                    .ok()??;
            if !std::ptr::eq(&*viewport_manager, &*manager) {
                return None; // The original controller's actual range belongs only to its actual manager.
            }
            let range =
                native_markdown_table_intent_step(&current, || viewport.viewportRange()).ok()??;
            let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
            let document_range =
                native_markdown_table_intent_step(&current, || provider.documentRange()).ok()?;
            let document_start =
                native_markdown_table_intent_step(&current, || document_range.location()).ok()?;
            let source_range = |range: &NSTextRange| -> Option<std::ops::Range<usize>> {
                let start =
                    native_markdown_table_intent_step(&current, || range.location()).ok()?;
                let end =
                    native_markdown_table_intent_step(&current, || range.endLocation()).ok()?;
                let from = usize::try_from(
                    native_markdown_table_intent_step(&current, || {
                        content.offsetFromLocation_toLocation(&document_start, &start)
                    })
                    .ok()?,
                )
                .ok()?;
                let to = usize::try_from(
                    native_markdown_table_intent_step(&current, || {
                        content.offsetFromLocation_toLocation(&document_start, &end)
                    })
                    .ok()?,
                )
                .ok()?;
                if from > to || to > document_length {
                    return None; // Foreign/out-of-range locations cannot be clamped into valid canonical ranges.
                }
                for (location, offset) in [(&start, from), (&end, to)] {
                    let offset = isize::try_from(offset).ok()?;
                    let reconstructed = native_markdown_table_intent_step(&current, || {
                        content.locationFromLocation_withOffset(&document_start, offset)
                    })
                    .ok()??;
                    if native_markdown_table_intent_step(&current, || {
                        content.offsetFromLocation_toLocation(location, &reconstructed)
                    })
                    .ok()?
                        != 0
                    {
                        return None; // The original provider must reconstruct endpoints exactly; integer bounds alone do not prove Source identity.
                    }
                }
                Some(from..to)
            };
            if source_range(&document_range)? != (0..document_length) {
                return None;
            }
            let viewport_source = source_range(&range)?;
            let terminal_viewport =
                viewport_source.is_empty() && viewport_source.end == document_length;
            if viewport_source.is_empty() && !terminal_viewport {
                return None; // An unlaid internal empty viewport has no current document coverage; do not guess the preceding fragment.
            }
            // A point query may return an old LayoutAvailable outflow fragment; only the current viewport.location
            // Source lookup establishes this batch's first fragment, without ensureLayout or reading offscreen text from zero.
            let seed = native_markdown_table_intent_step(&current, || range.location()).ok()?;
            let first = native_markdown_table_intent_step(&current, || {
                manager.textLayoutFragmentForLocation(&seed)
            })
            .ok()??;
            let lines = RefCell::new(Vec::<GutterLine>::new());
            let fragments = RefCell::new(Vec::<GutterFragmentProof>::new());
            let visited = Cell::new(0_usize);
            let covered = Cell::new(None);
            let previous_bottom = Cell::new(None);
            let failed = Cell::new(false);
            let block =
                block2::RcBlock::new(|pointer: NonNull<objc2_app_kit::NSTextLayoutFragment>| {
                    if !current() || visited.get() >= MAX_GUTTER_LINES {
                        failed.set(true);
                        return objc2::runtime::Bool::NO; // The budget counts fragments, so repeated fragments of one logical line cannot bypass it.
                    }
                    let first_callback = visited.get() == 0;
                    visited.set(visited.get() + 1);
                    let next = (|| -> Option<bool> {
                        // SAFETY: Public synchronous enumeration lends a live fragment during its callback only; no raw pointer enters the cache.
                        let fragment = unsafe { pointer.as_ref() };
                        if first_callback && !std::ptr::eq(fragment, &*first) {
                            return None; // If enumeration returns a different stale first fragment, skipping it cannot certify complete coverage.
                        }
                        if native_markdown_table_intent_step(&current, || fragment.state()).ok()?
                            != objc2_app_kit::NSTextLayoutFragmentState::LayoutAvailable
                        {
                            return None; // Estimated/unknown state cannot publish a frame, and drawing never performs layout.
                        }
                        let fragment_manager = native_markdown_table_intent_step(&current, || {
                            fragment.textLayoutManager()
                        })
                        .ok()??;
                        let element =
                            native_markdown_table_intent_step(&current, || fragment.textElement())
                                .ok()??;
                        let element_content = native_markdown_table_intent_step(&current, || {
                            element.textContentManager()
                        })
                        .ok()??;
                        if !std::ptr::eq(&*fragment_manager, &*manager)
                            || !std::ptr::eq(&*element_content, &*content)
                        {
                            return None; // Every fragment must still belong to the original manager/content; separate Native layouts cannot be mixed.
                        }
                        let fragment_range = native_markdown_table_intent_step(&current, || {
                            fragment.rangeInElement()
                        })
                        .ok()?;
                        let source = source_range(&fragment_range)?;
                        let location = native_markdown_table_intent_step(&current, || {
                            fragment_range.location()
                        })
                        .ok()?;
                        let actual = native_markdown_table_intent_step(&current, || {
                            manager.textLayoutFragmentForLocation(&location)
                        })
                        .ok()??;
                        if !std::ptr::eq(&*actual, fragment) {
                            return None; // An old outflow fragment replaced by its manager has no label authority even with LayoutAvailable state.
                        }
                        let frame = native_markdown_table_intent_step(&current, || {
                            fragment.layoutFragmentFrame()
                        })
                        .ok()?;
                        let (frontier, bottom) = if terminal_viewport {
                            // EOF may have an actually laid-out terminal empty line, or public lookup may return the actual final fragment;
                            // accepts only that one fragment ending at real document length, without backward search or a synthesized baseline.
                            if !first_callback || source.end != document_length {
                                return None;
                            }
                            (source.end, gutter_frame_bottom(frame, None)?)
                        } else {
                            gutter_viewport_fragment_frontier_with_hidden(
                                &viewport_source,
                                covered.get(),
                                &source,
                                previous_bottom.get(),
                                frame,
                                &|gap| {
                                    self.ivars().folds.hidden_range_is_covered(
                                        gap,
                                        generation,
                                        fold_revision,
                                        &current,
                                    )
                                },
                            )?
                        };
                        // Proves continuous Source and monotone geometry before logical-line metadata; offscreen and folded zero-height fragments advance coverage only.
                        let top = frame.origin.y + origin.y;
                        let bottom_in_document = bottom + origin.y;
                        if !top.is_finite() || !bottom_in_document.is_finite() {
                            return None;
                        }
                        if frame.size.height > 0.0
                            && top < visible.origin.y + visible.size.height
                            && bottom_in_document > visible.origin.y
                        {
                            let metadata = {
                                let document = document.borrow();
                                document
                                    .byte_offset_for_utf16(source.start)
                                    .and_then(|byte| document.line_for_byte(byte))
                                    .map(|number| {
                                        (
                                            number,
                                            markdown_line_may_fold(&document, number)
                                                .unwrap_or(false),
                                        )
                                    })
                            }
                            .ok()?;
                            let (number, hint) = metadata;
                            if lines
                                .borrow()
                                .last()
                                .is_none_or(|line| line.number != number)
                            {
                                let folded = self
                                    .ivars()
                                    .folds
                                    .borrow()
                                    .iter()
                                    .any(|fold| fold.header_line == number);
                                lines.borrow_mut().push(GutterLine {
                                    number,
                                    document_y: top,
                                    height: frame.size.height,
                                    may_fold: hint,
                                    folded,
                                });
                            } // Soft-wrap fragments share the original logical number, without moving it to the viewport top.
                        }
                        let fragment_weak =
                            native_markdown_table_intent_step(&current, || Weak::from(fragment))
                                .ok()?;
                        fragments.borrow_mut().push(GutterFragmentProof {
                            fragment: fragment_weak,
                            source,
                            frame,
                        });
                        covered.set(Some(frontier));
                        previous_bottom.set(Some(bottom));
                        Some(frontier < viewport_source.end) // Stops at complete actual viewport coverage without consuming adjacent outflow.
                    })();
                    if next.is_none() || !current() {
                        failed.set(true);
                        return objc2::runtime::Bool::NO;
                    }
                    next.unwrap_or(false).into()
                });
            let enumerated = native_markdown_table_intent_step(&current, || {
                manager.enumerateTextLayoutFragmentsFromLocation_options_usingBlock(
                    Some(&seed),
                    NSTextLayoutFragmentEnumerationOptions::empty(),
                    &block,
                )
            })
            .ok()?;
            drop(enumerated);
            drop(block); // Rechecks the full batch only after synchronous enumeration and callback borrows have ended.
            let covered_to_end = covered.get().is_some_and(|end| {
                end >= viewport_source.end
                    || self.ivars().folds.hidden_range_is_covered(
                        &(end..viewport_source.end),
                        generation,
                        fold_revision,
                        &current,
                    )
            });
            if failed.get() || !covered_to_end {
                return None; // Publishes no truncated prefix and never infers Source coverage from frame height.
            }
            let final_range =
                native_markdown_table_intent_step(&current, || viewport.viewportRange()).ok()??;
            let final_manager =
                native_markdown_table_intent_step(&current, || self.textLayoutManager()).ok()??;
            let final_content =
                native_markdown_table_intent_step(&current, || manager.textContentManager())
                    .ok()??;
            if source_range(&final_range)? != viewport_source
                || !std::ptr::eq(&*final_manager, &*manager)
                || !std::ptr::eq(&*final_content, &*content)
                || native_markdown_table_intent_step(&current, || self.bounded_paint_viewport())
                    .ok()?
                    != Some((visible, origin))
            {
                return None; // Find/scroll/mode reentry changing the original viewport or graph prevents mixed-frame publication.
            }
            let manager_weak =
                native_markdown_table_intent_step(&current, || Weak::from_retained(&manager))
                    .ok()?;
            let content_weak =
                native_markdown_table_intent_step(&current, || Weak::from_retained(&content))
                    .ok()?;
            let proof = Rc::new(GutterGeometryProof {
                generation,
                geometry_epoch,
                semantic_epoch,
                font_epoch,
                fold_revision,
                visible,
                origin,
                viewport_source,
                manager: Some(manager_weak),
                content: Some(content_weak),
                fragments: fragments.into_inner(),
            });
            Some((lines.into_inner(), proof))
        })();
        let Some((prepared, proof)) = prepared else {
            return; // Preserves the failure policy: incomplete geometry cannot masquerade as visible labels.
        };
        if !publish_gutter_lines(
            &ruler.ivars().lines,
            prepared,
            current() && self.gutter_generation_is_current(generation),
        ) {
            // Publishes after the final Native callback; close or canonical changes discard this candidate allocation.
            return;
        }
        *ruler.ivars().geometry.borrow_mut() = Some(proof);
        ruler.setNeedsDisplay(true);
    }
}

/// The next fragment of the same actual viewport must advance continuous Source with no backward or overlapping layout frame.
/// Checks bounded scalars only; zero-height folds may advance coverage, while actual paint intersections determine hidden/offscreen labels.
#[cfg(test)]
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
fn gutter_viewport_fragment_frontier(
    viewport: &std::ops::Range<usize>,
    covered: Option<usize>,
    source: &std::ops::Range<usize>,
    previous_bottom: Option<f64>,
    frame: NSRect,
) -> Option<(usize, f64)> {
    gutter_viewport_fragment_frontier_with_hidden(
        viewport,
        covered,
        source,
        previous_bottom,
        frame,
        &|_| false,
    )
}

/// Native may omit explicitly folded fragments; only hidden-range union coverage under the original intent permits crossing a Source gap.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
fn gutter_viewport_fragment_frontier_with_hidden(
    viewport: &std::ops::Range<usize>,
    covered: Option<usize>,
    source: &std::ops::Range<usize>,
    previous_bottom: Option<f64>,
    frame: NSRect,
    hidden: &impl Fn(&std::ops::Range<usize>) -> bool,
) -> Option<(usize, f64)> {
    if viewport.start >= viewport.end || source.start >= source.end {
        return None; // Ordinary viewports reject empty ranges; callers independently validate actual EOF against original document length.
    }
    let bottom = gutter_frame_bottom(frame, previous_bottom)?;
    if frame.size.height == 0.0 && !hidden(source) {
        return None; // Zero height alone cannot consume Source; only an actual explicit fold permits omitting that complete range.
    }
    let continuous = match covered {
        None => {
            (source.start <= viewport.start && viewport.start < source.end)
                || (source.start > viewport.start && hidden(&(viewport.start..source.start)))
        }
        Some(end) => {
            end < viewport.end
                && (source.start == end || (source.start > end && hidden(&(end..source.start))))
        }
    };
    continuous.then_some((source.end, bottom)) // Ordinary Source remains exactly adjacent; only a proven hidden gap is exempt.
}

/// Ordinary and actual EOF fragments share bounded frame validation, without repairing invalid layout through moved y, sorting, or estimated height.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
fn gutter_frame_bottom(frame: NSRect, previous_bottom: Option<f64>) -> Option<f64> {
    if ![
        frame.origin.x,
        frame.origin.y,
        frame.size.width,
        frame.size.height,
    ]
    .into_iter()
    .all(f64::is_finite)
        || frame.size.width < 0.0
        || frame.size.height < 0.0
    {
        return None; // Invalid or overflowing geometry proves no coverage; old coordinates cannot identify a visible logical line.
    }
    let bottom = frame.origin.y + frame.size.height;
    if !bottom.is_finite() || previous_bottom.is_some_and(|previous| frame.origin.y < previous) {
        return None; // Correct Source order with backward y still indicates mixed layout; coordinate sorting cannot hide overlap.
    }
    Some(bottom)
}

/// Publication requires the live matching-generation projection; window visibility and queued work are not authority.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
fn projection_can_publish(sealed: bool, expected: u64, current: Option<u64>) -> bool {
    !sealed && current == Some(expected)
}

/// The final bounded-metadata commit point; rejection drops the complete prepared Vec outside state borrowing.
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
fn publish_gutter_lines(
    target: &RefCell<Vec<GutterLine>>,
    prepared: Vec<GutterLine>,
    current: bool,
) -> bool {
    if !current || prepared.len() > MAX_GUTTER_LINES {
        // Neither truncates excess/stale candidates nor overwrites a zero-capacity slot after accepted cleanup.
        return false;
    }
    let previous = target.replace(prepared);
    drop(previous);
    true
}

#[cfg(test)]
mod publication_tests {
    use super::*;

    #[test]
    fn actual_viewport_excludes_old_outflow_and_rejects_backwards_geometry() {
        let viewport = 11_763..13_520;
        let frame = |y, height| NSRect::new(NSPoint::new(0.0, y), NSSize::new(600.0, height));
        // The regression's old point query returned three Source ranges before the viewport; LayoutAvailable alone grants no coverage.
        for (source, y, height) in [
            (11_504..11_560, 6_443.0, 17.0),
            (11_560..11_707, 6_460.0, 51.0),
            (11_707..11_763, 6_511.0, 17.0),
        ] {
            assert!(
                gutter_viewport_fragment_frontier(&viewport, None, &source, None, frame(y, height))
                    .is_none()
            );
        }
        let first = gutter_viewport_fragment_frontier(
            &viewport,
            None,
            &(11_763..11_819),
            None,
            frame(6_456.0, 17.0),
        )
        .unwrap();
        assert_eq!(first, (11_819, 6_473.0));
        assert!(
            gutter_viewport_fragment_frontier(
                &viewport,
                Some(first.0),
                &(11_819..11_875),
                Some(first.1),
                frame(6_460.0, 17.0)
            )
            .is_none()
        );
        assert_eq!(
            gutter_viewport_fragment_frontier(
                &viewport,
                Some(first.0),
                &(11_819..11_875),
                Some(first.1),
                frame(6_473.0, 17.0)
            ),
            Some((11_875, 6_490.0))
        );
    }

    #[test]
    fn viewport_coverage_keeps_wrapped_first_and_folded_zero_height_but_never_bridges_holes() {
        let viewport = 100..300;
        let frame = |y, height| NSRect::new(NSPoint::new(0.0, y), NSSize::new(0.0, height));
        // The first fragment may straddle viewport start; folded Source remains present, and zero height suppresses labels without deleting canonical coverage.
        let first =
            gutter_viewport_fragment_frontier(&viewport, None, &(80..160), None, frame(0.0, 34.0))
                .unwrap();
        let folded = gutter_viewport_fragment_frontier_with_hidden(
            &viewport,
            Some(first.0),
            &(160..240),
            Some(first.1),
            frame(34.0, 0.0),
            &|gap| *gap == (160..240),
        )
        .unwrap();
        assert_eq!(
            gutter_viewport_fragment_frontier(
                &viewport,
                Some(folded.0),
                &(240..320),
                Some(folded.1),
                frame(34.0, 17.0)
            ),
            Some((320, 51.0))
        );
        for source in [159..240, 161..240, 160..160] {
            assert!(
                gutter_viewport_fragment_frontier(
                    &viewport,
                    Some(first.0),
                    &source,
                    Some(first.1),
                    frame(34.0, 17.0)
                )
                .is_none()
            );
        }
        assert!(
            gutter_viewport_fragment_frontier(
                &viewport,
                Some(320),
                &(320..340),
                Some(51.0),
                frame(51.0, 17.0)
            )
            .is_none()
        );
        assert!(
            gutter_viewport_fragment_frontier(
                &(300..300),
                None,
                &(300..301),
                None,
                frame(0.0, 17.0)
            )
            .is_none()
        );
        for bad in [
            frame(f64::NAN, 17.0),
            frame(0.0, -1.0),
            frame(f64::MAX, f64::MAX),
        ] {
            assert!(
                gutter_viewport_fragment_frontier(&viewport, None, &(80..160), None, bad).is_none()
            );
        }
    }

    #[test]
    fn native_omitted_fold_requires_exact_hidden_gap_and_keeps_frame_order() {
        let frame = |y, height| NSRect::new(NSPoint::new(0.0, y), NSSize::new(780.0, height));
        let advance = |source: std::ops::Range<usize>,
                       y,
                       hidden: &dyn Fn(&std::ops::Range<usize>) -> bool| {
            gutter_viewport_fragment_frontier_with_hidden(
                &(0..490),
                Some(51),
                &source,
                Some(85.0),
                frame(y, 17.0),
                &|gap| hidden(gap),
            )
        };
        assert_eq!(
            advance(126..127, 85.0, &|gap| *gap == (51..126)),
            Some((127, 102.0))
        );
        assert!(advance(126..127, 85.0, &|_| false).is_none());
        assert!(advance(127..128, 85.0, &|gap| *gap == (51..126)).is_none());
        assert!(advance(126..127, 84.0, &|gap| *gap == (51..126)).is_none());
        assert!(
            gutter_viewport_fragment_frontier(
                &(0..490),
                Some(51),
                &(51..126),
                Some(85.0),
                frame(85.0, 0.0),
            )
            .is_none()
        ); // Zero height alone cannot prove hidden Source.
    }

    #[test]
    fn partially_visible_long_line_keeps_only_its_real_visible_fold_target() {
        // A long line extends beyond both document edges near Find; using its full height would admit clicks in occluded regions.
        let viewport = NSRect::new(NSPoint::new(0.0, 100.0), NSSize::new(74.0, 60.0));
        let visible = visible_gutter_line(40.0, 200.0, viewport).unwrap();
        assert_eq!(visible, viewport);
        assert!(!gutter_contains_point(visible, NSPoint::new(10.0, 80.0)));
        assert!(gutter_contains_point(visible, NSPoint::new(10.0, 120.0)));
        assert!(!gutter_contains_point(visible, NSPoint::new(10.0, 170.0)));
        // Fully offscreen lines have no hit rectangle; Native dirtyRect cannot replace the visible region.
        assert!(visible_gutter_line(10.0, 20.0, viewport).is_none());
    }

    #[test]
    fn resized_viewport_edges_and_invalid_geometry_cannot_expand_fold_hits() {
        let before = NSRect::new(NSPoint::new(0.0, 20.0), NSSize::new(74.0, 160.0));
        let after = NSRect::new(NSPoint::new(0.0, 80.0), NSSize::new(74.0, 100.0));
        let old_line = visible_gutter_line(60.0, 40.0, before).unwrap();
        let new_line = visible_gutter_line(60.0, 40.0, after).unwrap();
        // The narrowed Find/resize region immediately rejects old hit areas while preserving the same logical line's still-visible portion.
        assert!(gutter_contains_point(old_line, NSPoint::new(10.0, 70.0)));
        assert!(!gutter_contains_point(new_line, NSPoint::new(10.0, 70.0)));
        assert!(gutter_contains_point(new_line, NSPoint::new(0.0, 80.0)));
        assert!(!gutter_contains_point(new_line, NSPoint::new(74.0, 80.0)));
        assert!(!gutter_contains_point(new_line, NSPoint::new(0.0, 100.0)));
        assert!(visible_gutter_line(f64::NAN, 20.0, after).is_none());
        assert!(visible_gutter_line(80.0, f64::INFINITY, after).is_none());
        assert!(!gutter_contains_point(
            new_line,
            NSPoint::new(f64::NAN, 90.0)
        ));
    }
}
