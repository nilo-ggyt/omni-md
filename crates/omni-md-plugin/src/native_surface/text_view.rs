//! Adapts native responder/input and visible table interaction to the canonical storage contract.
//!
//! The editor owner retains this view. Its storage/layout backlinks and queued tasks are weak;
//! input depth, marked ranges and semantic cell coordinates are finite foreground metadata.

use super::*;
use objc2_app_kit::{NSTextContentManagerDelegate, NSTextContentStorageDelegate};

/// Stores finite input/layout metadata and one lazily owned cell field, with weak graph backlinks.
pub(super) struct PieceBackedTextViewIvars {
    // Scalar evidence shares this editor's storage lifetime only in explicitly instrumented builds.
    #[cfg(feature = "native-input-trace")]
    pub(super) input_trace: SharedNativeInputTrace,
    pub(super) source_decorations: RefCell<Vec<NativeMarkdownDecorationGeometry>>,
    pub(super) source_decoration_generation: Cell<u64>,
    pub(super) source_decorations_dirty: Cell<bool>,
    pub(super) gutter: OnceCell<Weak<NativeMarkdownGutter>>,
    pub(super) folds: RefCell<Vec<omni_markdown_core::markdown_folding::MarkdownFoldRegion>>,
    pub(super) fold_generation: Cell<u64>,
    pub(super) text_storage: Weak<PieceBackedMarkdownTextStorage>,
    // The surface remains the unique TextKit graph owner. This weak layout-manager link
    // prevents a competing view-owned root from surviving session teardown.
    pub(super) text_layout_manager: OnceCell<Weak<NSTextLayoutManager>>,
    pub(super) text_container_recovery_depth: Cell<usize>,
    // Replacing a content manager may synchronously invoke setTextContainer:. During
    // teardown, refuse to restore relationships that AppKit has just disconnected.
    pub(super) textkit_graph_teardown: Cell<bool>,
    // TextKit 2's inherited NSTextView implementation can retain a stale
    // marked range when its NSString is backed by a virtual piece tree. Keep
    // only the current composition span (never its text) at the input-client
    // boundary so the next IME update replaces the provisional span in place.
    pub(super) marked_text_range: Cell<Option<NSRange>>,
    pub(super) unmarking_text: Cell<bool>,
    pub(super) textkit_input_depth: Cell<usize>,
    pub(super) visible_table_layout_cache: RefCell<Option<NativeMarkdownVisibleTableLayoutCache>>,
    pub(super) table_layout_update_scheduled: Cell<bool>,
    pub(super) table_layout_rebuilding: Cell<bool>,
    pub(super) cell_editor: OnceCell<Retained<NSTextField>>,
    pub(super) active_table_cell: RefCell<Option<NativeMarkdownTableCellEditorState>>,
    pub(super) pending_table_cell_activation: Cell<Option<NativeMarkdownTableCellCoordinate>>,
}

define_class!(
    // SAFETY: NSTextView retains responsibility for TextKit input, layout and drawing.
    // Overrides route history into the bounded Rust model; weak links avoid a
    // text-view/content-storage/text-storage ownership cycle.
    #[unsafe(super = NSTextView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = PieceBackedTextViewIvars]
    pub(super) struct PieceBackedMarkdownTextView;

    // SAFETY: this NSTextView subclass inherits initialized NSObject identity on the main thread.
    unsafe impl NSObjectProtocol for PieceBackedMarkdownTextView {}

    // SAFETY: this NSTextView subclass receives text callbacks only while its owning surface retains the live main-thread view.
    unsafe impl NSTextDelegate for PieceBackedMarkdownTextView {}

    // SAFETY: this NSTextView subclass routes selection notifications after validating their originating live view.
    unsafe impl NSTextViewDelegate for PieceBackedMarkdownTextView {
        #[unsafe(method(textViewDidChangeSelection:))]
        /// Accepts only this editor's native selection notification before updating canonical selection metadata.
        fn text_view_did_change_selection(&self, notification: &NSNotification) {
            let Some(notification_text_view) = notification
                .object()
                .and_then(|object| object.downcast::<NSTextView>().ok())
            else {
                // A notification without an actual text-view sender has no document selection authority.
                return;
            };
            if !std::ptr::eq::<NSTextView>(&*notification_text_view, self.as_super()) {
                // Notifications from another native field/editor must not rewrite this document's selection.
                return;
            }
            let Some(text_storage) = self.ivars().text_storage.load() else {
                // Late selection delivery after storage teardown is an inert notification.
                return;
            };
            let selection = self.super_selected_range();
            let end = selection.location.saturating_add(selection.length);
            let intersects_hidden = self.ivars().folds.borrow().iter().any(|fold| {
                // Selecting or navigating into hidden canonical text reveals it before editing/accessibility proceeds.
                selection.location < fold.hidden_utf16_range.end && end > fold.hidden_utf16_range.start
            });
            if intersects_hidden {
                // Native find, select-all and accessibility selection never leave active text invisibly editable.
                self.unfold_source();
            }
            text_storage.update_active_selection(selection);
            self.invalidate_source_gutter_highlight();
            // The previous/current source-line backgrounds and any old bracket outline both need repaint.
            self.setNeedsDisplay(true);
            // Caret-dependent cues share the existing weak/coalesced layout transaction.
            self.ivars().source_decorations_dirty.set(true);
            self.schedule_visible_table_layout_update();
        }
    }

    // SAFETY: the owning content storage holds a weak delegate; callbacks run on the same main thread.
    unsafe impl NSTextContentManagerDelegate for PieceBackedMarkdownTextView {
        #[unsafe(method(textContentManager:shouldEnumerateTextElement:options:))]
        /// Omit only paragraphs within current-generation view-local folds, retaining canonical source offsets.
        fn should_enumerate_text_element(
            &self,
            manager: &objc2_app_kit::NSTextContentManager,
            element: &objc2_app_kit::NSTextElement,
            _options: objc2_app_kit::NSTextContentManagerEnumerationOptions,
        ) -> objc2::runtime::Bool {
            let Some(storage) = self.ivars().text_storage.load() else {
                // A disconnected graph exposes no active fold authority.
                return objc2::runtime::Bool::YES;
            };
            let generation = storage
                .document()
                .map(|document| document.borrow().generation());
            if generation != Some(self.ivars().fold_generation.get())
                || self.ivars().folds.borrow().is_empty()
            {
                // Any edit instantly invalidates old coordinates, even before the queued ruler refresh.
                return objc2::runtime::Bool::YES;
            }
            let Some(range) = element.elementRange() else {
                // Non-ranged structural elements must remain enumerable for TextKit's own graph traversal.
                return objc2::runtime::Bool::YES;
            };
            let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(manager);
            let offset = manager
                .offsetFromLocation_toLocation(&provider.documentRange().location(), &range.location());
            let Ok(offset) = usize::try_from(offset) else {
                // An untranslatable location cannot safely be treated as hidden source.
                return objc2::runtime::Bool::YES;
            };
            (!self
                .ivars()
                .folds
                .borrow()
                .iter()
                .any(|fold| fold.hidden_utf16_range.contains(&offset)))
            .into()
        }
    }

    // SAFETY: this storage-specific delegate inherits the same weak, main-thread content-manager contract.
    unsafe impl NSTextContentStorageDelegate for PieceBackedMarkdownTextView {}

    // SAFETY: this NSTextView subclass serves only its retained table-cell field and returns finite main-thread command results.
    unsafe impl NSControlTextEditingDelegate for PieceBackedMarkdownTextView {
        #[unsafe(method(control:textView:doCommandBySelector:))]
        /// Handles spreadsheet navigation for the one reusable table-cell field.
        ///
        /// # Safety
        /// AppKit must provide live main-thread control/text-view borrows and a valid Objective-C selector.
        unsafe fn control_text_view_do_command_by_selector(
            &self,
            _control: &NSControl,
            _text_view: &NSTextView,
            command_selector: Sel,
        ) -> bool {
            if command_selector == sel!(insertTab:) {
                // Tab commits the current draft before moving horizontally to the next parsed cell.
                self.navigate_active_table_cell(NativeMarkdownTableNavigation::Next);
                true
            } else if command_selector == sel!(insertBacktab:) {
                // Shift-Tab follows the same commit path in the reverse semantic direction.
                self.navigate_active_table_cell(NativeMarkdownTableNavigation::Previous);
                true
            } else if command_selector == sel!(insertNewline:)
                || command_selector == sel!(insertLineBreak:)
            {
                // Enter preserves the column while moving only to an existing next row.
                self.navigate_active_table_cell(NativeMarkdownTableNavigation::NextRow);
                true
            } else if command_selector == sel!(cancelOperation:) {
                // The cell field owns this cancellation; retain the original source and document window.
                self.cancel_active_table_cell();
                true
            } else {
                // Unclaimed field-editor commands retain standard native text behavior.
                false
            }
        }
    }

    // SAFETY: this NSTextView subclass uses the same main-thread table-field delegate lifetime as its owning text view.
    unsafe impl NSTextFieldDelegate for PieceBackedMarkdownTextView {}

    impl PieceBackedMarkdownTextView {
        #[cfg(feature = "native-input-trace")]
        #[unsafe(method(keyDown:))]
        /// Associate only callbacks executing on this real keyDown stack, forwarding native dispatch once.
        fn traced_key_down(&self, event: &NSEvent) {
            let entry = native_input_trace_now();
            let timestamp_bits = event.timestamp().to_bits();
            let (document, marked) = self.input_trace_observation();
            let scope = TraceScope::event(
                &self.ivars().input_trace,
                entry,
                timestamp_bits,
                document,
                marked,
            );
            // SAFETY: the original live AppKit event is forwarded unchanged to the matching main-thread
            // superclass method exactly once. Standard input-manager interpretation remains native.
            unsafe {
                let _: () = msg_send![super(self), keyDown: event];
            }
            if let Some(scope) = scope {
                // Return ends this actual event's causal scope; asynchronous IME delivery gets no guessed event.
                let (document, marked) = self.input_trace_observation();
                scope.finish(native_input_trace_now(), document, marked);
            }
        }

        #[unsafe(method(updateDragTypeRegistration))]
        /// Registers only the plain-source payload types this adapter can import, with no rich-text fallback.
        fn update_drag_type_registration(&self) {
            // This plain-source subclass owns file-to-Markdown imports. The inherited
            // rich-text policy would unregister them and enumerate a compatibility layout graph.
            if self.ivars().textkit_graph_teardown.get() || !self.isEditable() {
                // Closing/read-only views accept no drops and may not recreate layout relationships.
                self.unregisterDraggedTypes();
            } else {
                // Register only the native plain-text/file payloads handled by this adapter.
                // SAFETY: AppKit exports immutable process-lifetime type identifiers.
                let types = unsafe {
                    [
                        objc2_app_kit::NSPasteboardTypeFileURL,
                        NSPasteboardTypeString,
                    ]
                };
                self.registerForDraggedTypes(&NSArray::from_slice(&types));
            }
        }

        #[unsafe(method(paste:))]
        /// Routes explicit file payloads through confined import and ordinary text through native editing.
        fn paste(&self, sender: Option<&AnyObject>) {
            // File URLs are imported through the confined vault before inserting Markdown links.
            let files = native_markdown_pasteboard_file_paths(&NSPasteboard::generalPasteboard());
            if files.is_empty() {
                // Ordinary text keeps native selection, IME and canonical storage behavior.
                // SAFETY: the superclass receives the borrowed sender synchronously on the main thread.
                unsafe {
                    let _: () = msg_send![super(self), paste: sender];
                }
            } else {
                // Queue file work outside the native paste callback; only a weak view identity escapes.
                schedule_native_markdown_attachment_paths(self, files);
            }
        }

        #[unsafe(method(draggingEntered:))]
        /// Advertises copy-only file import while preserving ordinary native text dragging.
        fn dragging_entered(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            // File drags advertise a copy, keeping the external source file untouched.
            if !native_markdown_pasteboard_file_paths(&sender.draggingPasteboard()).is_empty() {
                // External files remain unchanged; only an explicit confined asset copy is offered.
                NSDragOperation::Copy
            } else {
                // Non-file text drags retain the inherited native editing contract.
                // SAFETY: AppKit owns the drag-info object throughout this main-thread callback.
                unsafe { msg_send![super(self), draggingEntered: sender] }
            }
        }

        #[unsafe(method(performDragOperation:))]
        /// Anchors file insertion to the visible physical drop point before scheduling confined import.
        fn perform_drag_operation(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            let files = native_markdown_pasteboard_file_paths(&sender.draggingPasteboard());
            if files.is_empty() {
                // Non-file payloads are handled once by the inherited text view.
                // SAFETY: AppKit supplies live drag information and the superclass handles it synchronously.
                unsafe { msg_send![super(self), performDragOperation: sender] }
            } else {
                // Commit existing provisional edits before resolving the physical drop location.
                if !self.commit_cell_before_command() {
                    // Rejected native field input forbids importing an attachment at a different caret.
                    false
                } else {
                    if !self.commit_composition_before_command() {
                        // A refused native commit leaves the original composition editable and imports nothing.
                        return false.into();
                    }
                    let point = self.convertPoint_fromView(sender.draggingLocation(), None);
                    if let Some(offset) = self.insertion_offset_at_view_point(point) {
                        // The queued import preserves this exact generation/caret anchor or fails without a link.
                        self.setSelectedRange(NSRange::new(offset, 0));
                        schedule_native_markdown_attachment_paths(self, files);
                        true
                    } else {
                        // Missing transient TextKit geometry rejects the drop without copying any external file.
                        false
                    }
                }
            }
        }

        #[unsafe(method(setTextContainer:))]
        /// Preserves the existing weak layout-manager relation during native container callbacks.
        ///
        /// # Safety
        /// AppKit must supply a live optional NSTextContainer on this view's allocating main thread.
        unsafe fn set_text_container(&self, text_container: Option<&NSTextContainer>) {
            let graph_teardown = self.ivars().textkit_graph_teardown.get();
            native_markdown_trace! {
                eprintln!(
                    "[probe] setTextContainer before present={} container={:?} container_manager={} manager={} content={} teardown={}",
                    text_container.is_some(),
                    text_container.map(|value| value as *const NSTextContainer),
                    text_container.is_some_and(|value| value.textLayoutManager().is_some()),
                    self.textLayoutManager().is_some(),
                    self.textContentStorage().is_some(),
                    graph_teardown,
                );
            }
            // SAFETY: AppKit owns the optional container through this main-thread callback;
            // forward the exact public setter contract once before repairing the weak manager link.
            unsafe {
                let _: () = msg_send![super(self), setTextContainer: text_container];
            }
            if !graph_teardown
                && !self.ivars().textkit_graph_teardown.get()
                && self.ivars().text_container_recovery_depth.get() == 0
                && let Some(text_container) = text_container
                && let Some(text_layout_manager) =
                    self.ivars().text_layout_manager.get().and_then(Weak::load)
            {
                // AppKit temporarily detaches the container from both view and manager while resizing.
                // A callback-local retain restores the manager link without creating a second
                // long-lived ownership root alongside the surface.
                self.ivars().text_container_recovery_depth.set(1);
                text_layout_manager.setTextContainer(Some(text_container));
                self.ivars().text_container_recovery_depth.set(0);
            }
            native_markdown_trace! {
                eprintln!(
                    "[probe] setTextContainer after manager={} content={} teardown={}",
                    self.textLayoutManager().is_some(),
                    self.textContentStorage().is_some(),
                    self.ivars().textkit_graph_teardown.get(),
                );
            }
        }

        #[unsafe(method(replaceTextContainer:))]
        /// Forwards an AppKit-requested container replacement without creating a competing layout owner.
        fn replace_text_container(&self, text_container: &NSTextContainer) {
            native_markdown_trace! {
                eprintln!(
                    "[probe] replaceTextContainer before manager={} content={}",
                    self.textLayoutManager().is_some(),
                    self.textContentStorage().is_some(),
                );
            }
            // SAFETY: forward the live borrowed container to the matching superclass method on the main thread.
            unsafe {
                let _: () = msg_send![super(self), replaceTextContainer: text_container];
            }
            native_markdown_trace! {
                eprintln!(
                    "[probe] replaceTextContainer after manager={} content={}",
                    self.textLayoutManager().is_some(),
                    self.textContentStorage().is_some(),
                );
            }
        }

        #[unsafe(method(viewDidMoveToWindow))]
        /// Completes native window association before scheduling any derived viewport layout.
        fn view_did_move_to_window(&self) {
            native_markdown_trace! {
                eprintln!(
                    "[probe] view did move window before manager={} window={}",
                    self.textLayoutManager().is_some(),
                    self.window().is_some()
                );
            }
            // Complete superclass window association before scheduling bounded table layout.
            // Layout runs later on the main queue, outside this callback and the drawing stack.
            // SAFETY: this main-thread override invokes its matching no-argument superclass callback once.
            unsafe {
                let _: () = msg_send![super(self), viewDidMoveToWindow];
            }
            self.schedule_visible_table_layout_update();
            native_markdown_trace! {
                eprintln!(
                    "[probe] view did move window after manager={} window={}",
                    self.textLayoutManager().is_some(),
                    self.window().is_some()
                );
            }
        }

        #[unsafe(method(setFrameSize:))]
        /// Preserves native sizing and invalidates only bounded viewport geometry.
        fn set_frame_size(&self, new_size: NSSize) {
            native_markdown_trace! {
                eprintln!(
                    "[probe] set frame size before {:?} manager={}",
                    new_size,
                    self.textLayoutManager().is_some()
                );
            }
            // SAFETY: NSSize has the declared Cocoa ABI and the live main-thread view receives its matching superclass method.
            unsafe {
                let _: () = msg_send![super(self), setFrameSize: new_size];
            }
            // Resize invalidates viewport geometry even when source bytes are unchanged.
            self.schedule_visible_table_layout_update();
            native_markdown_trace! {
                eprintln!(
                    "[probe] set frame size after {:?} manager={}",
                    new_size,
                    self.textLayoutManager().is_some()
                );
            }
        }

        #[unsafe(method(viewDidEndLiveResize))]
        /// Queues one coalesced geometry refresh after native live resizing finishes.
        fn view_did_end_live_resize(&self) {
            native_markdown_trace! {
                eprintln!("[probe] view end resize manager={}", self.textLayoutManager().is_some());
            }
            // SAFETY: the live main-thread view forwards its matching no-argument superclass callback once.
            unsafe {
                let _: () = msg_send![super(self), viewDidEndLiveResize];
            }
            self.schedule_visible_table_layout_update();
        }

        #[unsafe(method(scrollWheel:))]
        /// Preserves native scrolling before scheduling the new visible table projection.
        fn scroll_wheel(&self, event: &NSEvent) {
            native_markdown_trace! {
                eprintln!("[probe] scroll wheel manager={}", self.textLayoutManager().is_some());
            }
            // SAFETY: AppKit owns the event for this callback; the live superclass receives it synchronously on the main thread.
            unsafe {
                let _: () = msg_send![super(self), scrollWheel: event];
            }
            self.schedule_visible_table_layout_update();
        }

        #[unsafe(method(drawViewBackgroundInRect:))]
        /// Add cached active-line geometry after native background paint and before native source glyph drawing.
        fn draw_view_background_in_rect(&self, dirty: NSRect) {
            // SAFETY: forward the matching public NSTextView background hook once on the main thread.
            unsafe {
                let _: () = msg_send![super(self), drawViewBackgroundInRect: dirty];
            }
            self.draw_source_active_line(dirty);
        }

        #[unsafe(method(drawRect:))]
        /// Completes native text drawing before painting already-prepared bounded table overlays.
        fn draw_rect(&self, dirty_rect: NSRect) {
            native_markdown_trace! {
                eprintln!(
                    "[probe] draw rect begin manager={} cache={}",
                    self.textLayoutManager().is_some(),
                    self.ivars().visible_table_layout_cache.borrow().is_some()
                );
            }

            // SAFETY: NSTextView completes normal layout/drawing before read-only table overlays.
            // The overlay never becomes a second mutable text model.
            unsafe {
                let _: () = msg_send![super(self), drawRect: dirty_rect];
            }
            self.draw_visible_markdown_tables(dirty_rect);
            self.draw_source_decorations(dirty_rect);
            // A previous request may have encountered an outer TextKit editing transaction.
            // Drawing only queues a coalesced successor; it never lays out tables on this stack.
            self.schedule_visible_table_layout_update();
            native_markdown_trace! {
                eprintln!("[probe] draw rect end manager={}", self.textLayoutManager().is_some());
            }
        }

        #[unsafe(method(mouseDown:))]
        /// Selects a visible table cell or preserves native text selection after committing an earlier cell draft.
        fn mouse_down(&self, event: &NSEvent) {
            // Hit testing consumes only the current immutable visible-cell projection.
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            if let Some(cell) = self.visible_table_cell_at_point(point) {
                // A visible table hit owns editing focus rather than source-text drag selection.
                self.activate_table_cell(cell);
                return;
            }
            if !self.commit_cell_before_command() {
                // Preserve the pending cell draft rather than advancing this native command.
                return;
            }
            // SAFETY: non-table clicks forward exactly once to NSTextView, preserving native
            // selection, dragging and input-method behavior.
            unsafe {
                let _: () = msg_send![super(self), mouseDown: event];
            }
        }

        #[unsafe(method(hasMarkedText))]
        /// Reports native transient composition during input and canonical settled composition between callbacks.
        fn has_marked_text(&self) -> bool {
            if self.ivars().textkit_input_depth.get() != 0 {
                // SAFETY: while forwarding an input operation, ask NSTextView
                // for its own transient state so its implementation sees the
                // same state it established internally.
                return unsafe { msg_send![super(self), hasMarkedText] };
            }
            self.ivars()
                .marked_text_range
                .get()
                .is_some_and(|range| range.length != 0)
        }

        #[unsafe(method(markedRange))]
        /// Keeps the current IME span scalar-safe without exposing a second composition text model.
        fn marked_range(&self) -> NSRange {
            if self.ivars().textkit_input_depth.get() != 0 {
                // SAFETY: see `has_marked_text`; this avoids feeding the
                // host's internal TextKit transaction back into our semantic
                // range adapter.
                return unsafe { msg_send![super(self), markedRange] };
            }
            self.ivars()
                .marked_text_range
                .get()
                .unwrap_or_else(|| NSRange::new(NSNotFound as usize, 0))
        }

        #[unsafe(method(selectedRange))]
        /// Uses native transient selection only while a guarded input operation owns its reconciliation.
        fn selected_range(&self) -> NSRange {
            if self.ivars().textkit_input_depth.get() != 0 {
                // Native insertion reconciles a transient selection before canonical metadata settles.
                return self.super_selected_range();
            }
            self.semantic_selection_range()
                .unwrap_or_else(|| self.super_selected_range())
        }

        #[unsafe(method(insertText:replacementRange:))]
        /// Forwards a normalized canonical replacement through standard native input and balanced style guards.
        ///
        /// # Safety
        /// The input object must be a live NSString or NSAttributedString accepted by NSTextInputClient;
        /// AppKit must invoke this callback on the owning main thread.
        unsafe fn insert_text_replacement_range(&self, string: &AnyObject, replacement_range: NSRange) {
            #[cfg(feature = "native-input-trace")]
            let trace_scope = self.begin_input_trace_selector(NativeInputTraceSelector::InsertText);
            if self.ivars().textkit_graph_teardown.get() {
                // Accepted close seals native composition/selection work as well as canonical edits;
                // forwarding late input could otherwise create new system ownership after the final save.
                #[cfg(feature = "native-input-trace")]
                {
                    self.reject_input_trace(NativeInputTraceRejection::Closing);
                    self.finish_input_trace_selector(trace_scope);
                }
                return;
            }
            // TextKit 2 can send `NSNotFound` when the input method expects the
            // text client to use its current selection. A piece-backed string
            // has no materialized selection state of its own, so provide the
            // semantic range explicitly at this boundary. We deliberately do
            // not call `setSelectedRange` afterwards: that would synchronously
            // re-enter TextKit layout and can produce an input/selection loop.
            let had_marked_text = self
                .ivars()
                .marked_text_range
                .get()
                .is_some_and(|range| range.length != 0);
            let effective_range = self.input_replacement_range(replacement_range, had_marked_text);
            self.begin_textkit_input_operation();
            // SAFETY: NSTextInputClient supplies a live NSString/attributed string; the normalized scalar range is forwarded synchronously on the main thread.
            let _: () = unsafe {
                msg_send![super(self), insertText: string, replacementRange: effective_range]
            };
            self.end_textkit_input_operation();
            self.clear_pending_selection();
            // `insertText` commits an active composition. A normal insertion
            // also invalidates any stale local span, so the next composition
            // starts from the semantic Rust selection.
            self.ivars().marked_text_range.set(None);
            if let Some(text_storage) = self.ivars().text_storage.load() {
                // End semantic deferral after native input has committed its provisional text.
                text_storage.finish_composition();
            }
            #[cfg(feature = "native-input-trace")]
            self.finish_input_trace_selector(trace_scope);
            self.schedule_visible_table_layout_update();
        }

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        /// Updates one provisional IME span using native text input and canonical UTF-16 coordinates.
        ///
        /// # Safety
        /// The input must be a live supported native string; selected_range refers to that string and
        /// replacement_range uses the NSTextInputClient explicit-range or NSNotFound convention.
        unsafe fn set_marked_text_selected_range_replacement_range(
            &self,
            string: &AnyObject,
            selected_range: NSRange,
            replacement_range: NSRange,
        ) {
            #[cfg(feature = "native-input-trace")]
            let trace_scope = self.begin_input_trace_selector(NativeInputTraceSelector::SetMarkedText);
            if self.ivars().textkit_graph_teardown.get() {
                // Native-only provisional state must not restart after the saved foreground input gate closes.
                #[cfg(feature = "native-input-trace")]
                {
                    self.reject_input_trace(NativeInputTraceRejection::Closing);
                    self.finish_input_trace_selector(trace_scope);
                }
                return;
            }
            // During an IME composition AppKit commonly supplies
            // `NSNotFound` for the replacement range on the first update and
            // on subsequent updates supplies the currently marked range. Use
            // the marked range first so a Chinese composition replaces its
            // previous provisional text rather than appending to it.
            let effective_range = self.normalized_replacement_range(replacement_range);
            let replacement_length = string_utf16_length(string);
            if let Some(text_storage) = self.ivars().text_storage.load() {
                // Provisional candidates keep stable base attributes until the input method commits.
                text_storage.begin_composition();
            }
            self.begin_textkit_input_operation();
            // SAFETY: the live input string and relative selection satisfy NSTextInputClient; normalized replacement coordinates remain in the canonical document.
            let _: () = unsafe {
                msg_send![
                    super(self),
                    setMarkedText: string,
                    selectedRange: selected_range,
                    replacementRange: effective_range
                ]
            };
            self.end_textkit_input_operation();
            self.clear_pending_selection();
            let Some(marked_end) = effective_range.location.checked_add(replacement_length) else {
                // An overflowing marked span cannot become valid canonical selection metadata.
                #[cfg(feature = "native-input-trace")]
                {
                    self.reject_input_trace(NativeInputTraceRejection::CoordinateOverflow);
                    self.finish_input_trace_selector(trace_scope);
                }
                return;
            };
            let document_length = self.ivars().text_storage.load().and_then(|storage| {
                storage
                    .document()
                    .map(|document| document.borrow().utf16_len())
            });
            if document_length.is_some_and(|length| marked_end <= length) {
                // Only an accepted in-document candidate may replace the adapter's settled marked span.
                let marked_range = NSRange::new(effective_range.location, replacement_length);
                self.ivars().marked_text_range.set(Some(marked_range));
                if let Some(text_storage) = self.ivars().text_storage.load() {
                    // NSTextInputClient reports selection relative to marked text. The canonical piece
                    // tree stores absolute UTF-16 selection, independent of synchronous layout timing.
                    text_storage.set_composition_selection(marked_range, selected_range);
                }
            }
            #[cfg(feature = "native-input-trace")]
            self.finish_input_trace_selector(trace_scope);
        }

        #[unsafe(method(unmarkText))]
        /// Clears both native and semantic composition state without recursively committing the same span.
        ///
        /// # Safety
        /// The retained view, native storage and input transaction must be live on the AppKit main thread.
        unsafe fn unmark_text(&self) {
            #[cfg(feature = "native-input-trace")]
            let trace_scope = self.begin_input_trace_selector(NativeInputTraceSelector::UnmarkText);
            if self.ivars().textkit_graph_teardown.get() {
                // Closing was authorized only after composition cleared; stale selectors cannot accept more edits.
                #[cfg(feature = "native-input-trace")]
                {
                    self.reject_input_trace(NativeInputTraceRejection::Closing);
                    self.finish_input_trace_selector(trace_scope);
                }
                return;
            }
            // NSTextView must clear its own marked-text bookkeeping as well as our semantic
            // range. Forward under the existing native-input guard so reentrant getters observe
            // superclass state while the standard commit operation is in progress.
            if self.ivars().unmarking_text.replace(true) {
                // A nested superclass callback belongs to the current unmark operation.
                #[cfg(feature = "native-input-trace")]
                self.finish_input_trace_selector(trace_scope);
                return;
            }
            self.begin_textkit_input_operation();
            // SAFETY: unmarkText is the matching main-thread NSTextInputClient superclass method;
            // the retained native graph and canonical document remain live for this synchronous commit.
            unsafe {
                let _: () = msg_send![super(self), unmarkText];
            }
            self.end_textkit_input_operation();
            self.ivars().marked_text_range.set(None);
            if let Some(text_storage) = self.ivars().text_storage.load() {
                // End semantic deferral after native input has committed its provisional text.
                text_storage.finish_composition();
            }
            self.ivars().unmarking_text.set(false);
            #[cfg(feature = "native-input-trace")]
            self.finish_input_trace_selector(trace_scope);
            self.schedule_visible_table_layout_update();
        }

        #[unsafe(method(insertNewline:))]
        /// Route settled Return through canonical Markdown continuation and ordinary indentation semantics.
        fn insert_newline(&self, sender: Option<&AnyObject>) {
            if ProtocolObject::<dyn NSTextInputClient>::from_ref(self.as_super()).hasMarkedText()
                || !self.run_canonical_editing_command(MarkdownEditingCommand::InsertNewline)
            {
                // Native composition gets first refusal; an explicitly unhandled context uses normal AppKit input.
                // SAFETY: forward the matching selector and borrowed sender synchronously on the main thread.
                unsafe {
                    let _: () = msg_send![super(self), insertNewline: sender];
                }
            }
        }

        #[unsafe(method(deleteBackward:))]
        /// Remove Markdown block markup first, preserving native grapheme deletion when no command applies.
        fn delete_backward(&self, sender: Option<&AnyObject>) {
            if ProtocolObject::<dyn NSTextInputClient>::from_ref(self.as_super()).hasMarkedText()
                || !self.run_canonical_editing_command(MarkdownEditingCommand::DeleteMarkupBackward)
            {
                // Input methods and ordinary Unicode deletion retain their inherited native behavior.
                // SAFETY: the live main-thread superclass receives its original borrowed sender exactly once.
                unsafe {
                    let _: () = msg_send![super(self), deleteBackward: sender];
                }
            }
        }

        #[unsafe(method(insertTab:))]
        /// Indent selected source lines with the preserved two-space command; preview keeps native routing.
        fn insert_tab(&self, sender: Option<&AnyObject>) {
            let source =
                self.ivars().text_storage.load().is_some_and(|storage| {
                    storage.presentation_mode() == MarkdownPresentationMode::Source
                });
            if ProtocolObject::<dyn NSTextInputClient>::from_ref(self.as_super()).hasMarkedText()
                || !source
                || !self.run_canonical_editing_command(MarkdownEditingCommand::IndentMore)
            {
                // Non-source/native composition input is handled once by the original responder implementation.
                // SAFETY: forward the matching input selector on the live main-thread text view.
                unsafe {
                    let _: () = msg_send![super(self), insertTab: sender];
                }
            }
        }

        #[unsafe(method(insertBacktab:))]
        /// Outdent source lines without turning a handled no-op into native focus traversal.
        fn insert_backtab(&self, sender: Option<&AnyObject>) {
            let source =
                self.ivars().text_storage.load().is_some_and(|storage| {
                    storage.presentation_mode() == MarkdownPresentationMode::Source
                });
            if ProtocolObject::<dyn NSTextInputClient>::from_ref(self.as_super()).hasMarkedText()
                || !source
                || !self.run_canonical_editing_command(MarkdownEditingCommand::IndentLess)
            {
                // An unclaimed native context keeps its standard Shift-Tab behavior.
                // SAFETY: the live superclass borrows the original sender for this synchronous selector.
                unsafe {
                    let _: () = msg_send![super(self), insertBacktab: sender];
                }
            }
        }

        #[unsafe(method(undo:))]
        /// Routes native Undo to the sole bounded structural piece-tree history.
        fn undo(&self, _sender: Option<&AnyObject>) {
            self.restore_history(MarkdownHistoryDirection::Undo);
        }

        #[unsafe(method(redo:))]
        /// Routes native Redo to structural history without enabling NSUndoManager storage.
        fn redo(&self, _sender: Option<&AnyObject>) {
            self.restore_history(MarkdownHistoryDirection::Redo);
        }

        #[unsafe(method(cancelOperation:))]
        /// Leaves selector-only cancellation with AppKit so input-source actions cannot dismiss the plugin.
        fn cancel_operation(&self, sender: Option<&AnyObject>) {
            // Input methods can issue cancelOperation: without a physical Escape event. Native
            // composition owns those selectors; window-level physical key routing owns dismissal.
            // SAFETY: the live main-thread superclass receives the original borrowed sender once.
            unsafe {
                let _: () = msg_send![super(self), cancelOperation: sender];
            }
        }

        #[unsafe(method(performKeyEquivalent:))]
        /// Claims exact Command-W while leaving other native key equivalents unchanged.
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            if is_exact_markdown_command_w_event(event) {
                // Exact Command-W follows the same save-before-close identity-bound action as the panel.
                self.enqueue_markdown_dismissal();
                true
            } else {
                // SAFETY: key equivalents not owned by Omni forward exactly once to NSTextView.
                unsafe { msg_send![super(self), performKeyEquivalent: event] }
            }
        }
    }
);

impl PieceBackedMarkdownTextView {
    /// Execute one shared keyboard transaction after visible cell/composition ownership has settled.
    fn run_canonical_editing_command(&self, command: MarkdownEditingCommand) -> bool {
        if self.ivars().textkit_graph_teardown.get() {
            // Consume late native commands without allowing fallback to change the sealed storage.
            return true;
        }
        if !self.commit_cell_before_command() {
            // A failed visible-cell commit consumes/vetoes the action instead of falling back and losing its draft.
            return true;
        }
        if !self.commit_composition_before_command() {
            // Markup/indentation must not replace a native provisional range while the IME still owns it.
            return true;
        }
        self.unfold_source();
        let Some(storage) = self.ivars().text_storage.load() else {
            // A disconnected old native shell cannot edit a new session through fallback.
            return true;
        };
        let range = self.selectedRange();
        let selection = MarkdownTextSelection {
            anchor_utf16: range.location,
            head_utf16: range.location.saturating_add(range.length),
        };
        match storage.apply_editing_command(command, selection) {
            // No applicable markup rule leaves ordinary native input/grapheme deletion available.
            Ok(MarkdownEditingOutcome::NotHandled) => false,
            // An unindented Shift-Tab is consumed without changing history or native focus.
            Ok(MarkdownEditingOutcome::Unchanged) => true,
            // The complete receipt is authoritative after canonical borrows and storage notifications have ended.
            Ok(MarkdownEditingOutcome::Changed(edit)) => {
                let start = edit
                    .selection_after
                    .anchor_utf16
                    .min(edit.selection_after.head_utf16);
                let end = edit
                    .selection_after
                    .anchor_utf16
                    .max(edit.selection_after.head_utf16);
                self.setSelectedRange(NSRange::new(start, end - start));
                self.schedule_visible_table_layout_update();
                true
            }
            // Rejections keep the exact draft/source/history and stop this action before native fallback can mutate it.
            Err(error) => {
                present_native_markdown_error(&error);
                true
            }
        }
    }

    /// Resolves a physical drop through TextKit 2 selection navigation within the visible container.
    pub(super) fn insertion_offset_at_view_point(&self, point: NSPoint) -> Option<usize> {
        let document_length = self.document_length();
        if document_length == 0 {
            // An empty canonical document has one valid insertion boundary and needs no fragment lookup.
            return Some(0);
        }
        let manager = self.textLayoutManager()?;
        let content = manager.textContentManager()?;
        if content.hasEditingTransaction() {
            // Pointer hit testing must not enumerate text elements inside an active native editing group.
            return None;
        }
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
        let start = provider.documentRange().location();
        let origin = self.textContainerOrigin();
        let mut bounds = self.visibleRect();
        bounds.origin.x -= origin.x;
        bounds.origin.y -= origin.y;
        // TextKit consumes container coordinates; no deprecated layoutManager/point-index accessor
        // may switch this view into TextKit 1 or build a complete document-sized native text mirror.
        let selections = manager.textSelectionNavigation()
            .textSelectionsInteractingAtPoint_inContainerAtLocation_anchors_modifiers_selecting_bounds(
                NSPoint::new(point.x - origin.x, point.y - origin.y), &start, &NSArray::new(),
                objc2_app_kit::NSTextSelectionNavigationModifier::empty(), false, bounds,
            );
        let range = selections.firstObject()?.textRanges().firstObject()?;
        let offset =
            usize::try_from(provider.offsetFromLocation_toLocation(&start, &range.location()))
                .ok()?;
        // Revalidate the native coordinate against the sole canonical scalar boundary before mutation.
        let storage = self.ivars().text_storage.load()?;
        let document = storage.document()?;
        let valid = document.borrow().byte_offset_for_utf16(offset).is_ok();
        valid.then_some(offset)
    }

    /// Reads canonical UTF-16 length through weak storage without retaining text across native calls.
    pub(super) fn document_length(&self) -> usize {
        self.ivars()
            .text_storage
            .load()
            .and_then(|storage| {
                storage
                    .document()
                    .map(|document| document.borrow().utf16_len())
            })
            .unwrap_or(0)
    }

    /// Resolves explicit or NSNotFound input ranges using marked composition before the settled selection.
    pub(super) fn input_replacement_range(
        &self,
        requested: NSRange,
        has_marked_text: bool,
    ) -> NSRange {
        let document_length = self.document_length();
        if is_valid_markdown_replacement_range(requested, document_length) {
            // A valid explicit native range takes priority over either provisional or settled selection.
            return requested;
        }

        if has_marked_text {
            // Candidate replacement defaults to the entire provisional composition when AppKit sends NSNotFound.
            self.ivars()
                .marked_text_range
                .get()
                .filter(|range| is_valid_markdown_replacement_range(*range, document_length))
        } else {
            // Ordinary typing has no provisional range; canonical/native selection supplies the fallback below.
            None
        }
        .or_else(|| self.semantic_selection_range())
        .unwrap_or_else(|| self.super_selected_range())
    }

    /// Starts balanced native getter/style guards before forwarding a standard NSTextInputClient operation.
    pub(super) fn begin_textkit_input_operation(&self) {
        if self.ivars().textkit_input_depth.get() == 0 {
            // Editing always expands view-only folds before native input can change hidden source coordinates.
            self.unfold_source();
        }
        // The storage shares this scope so selection-dependent style invalidation cannot reenter
        // native element enumeration while NSTextView is still inserting or committing marked text.
        if let Some(storage) = self.ivars().text_storage.load() {
            // Share the outer input boundary with storage so derived attribute notifications wait for native insertion.
            storage.begin_native_input();
        }
        let depth = self.ivars().textkit_input_depth.get();
        self.ivars()
            .textkit_input_depth
            .set(depth.saturating_add(1));
    }

    /// Ends native input guards and flushes coalesced derived attributes after superclass input returns.
    pub(super) fn end_textkit_input_operation(&self) {
        let depth = self.ivars().textkit_input_depth.get();
        self.ivars()
            .textkit_input_depth
            .set(depth.saturating_sub(1));
        // End the native view's guard before style notifications can synchronously reenter getters.
        if let Some(storage) = self.ivars().text_storage.load() {
            // Only the final balanced outer input return may publish coalesced presentation invalidation.
            storage.end_native_input();
        }
    }

    /// Releases transient native reconciliation metadata after one complete storage/input operation.
    pub(super) fn clear_pending_selection(&self) {
        if let Some(text_storage) = self.ivars().text_storage.load() {
            // The pending native reconciliation range belongs to this completed input transaction only.
            text_storage.ivars().pending_textkit_selection.take();
        }
    }

    /// Reads NSTextView native selection during its own synchronous input transaction.
    pub(super) fn super_selected_range(&self) -> NSRange {
        // SAFETY: `selectedRange` is an NSTextView primitive and the superclass
        // implementation remains available while this subclass is alive.
        unsafe { msg_send![super(self), selectedRange] }
    }

    /// Reads the canonical settled selection when no native input transaction owns a transient range.
    pub(super) fn semantic_selection_range(&self) -> Option<NSRange> {
        let text_storage = self.ivars().text_storage.load()?;
        let document = text_storage.document()?;
        let document = document.borrow();
        let selection = document.selection();
        let start = selection.anchor_utf16.min(selection.head_utf16);
        let end = selection.anchor_utf16.max(selection.head_utf16);
        let range = NSRange::new(start, end - start);
        is_valid_markdown_replacement_range(range, document.utf16_len()).then_some(range)
    }

    /// Normalizes sentinel and stale replacement ranges against the current canonical document.
    pub(super) fn normalized_replacement_range(&self, requested: NSRange) -> NSRange {
        let Some(text_storage) = self.ivars().text_storage.load() else {
            return requested;
        };
        let Some(document) = text_storage.document() else {
            return requested;
        };
        let document_length = document.borrow().utf16_len();

        if is_valid_markdown_replacement_range(requested, document_length) {
            // An explicit in-document native replacement outranks composition/selection fallbacks;
            // preserving it lets the input method replace its intended scalar-aligned source interval.
            return requested;
        }

        let input_client = ProtocolObject::<dyn NSTextInputClient>::from_ref(self.as_super());
        let marked_range = input_client.markedRange();
        if is_valid_markdown_replacement_range(marked_range, document_length) {
            // A later IME candidate replaces the full provisional scalar span rather than appending.
            return marked_range;
        }

        // The Rust selection is updated from `textViewDidChangeSelection:` and
        // remains authoritative even when TextKit has not yet reconciled its
        // piece-backed NSString. It is the fallback for the first IME
        // transaction and for programmatic input.
        let document_selection = document.borrow().selection();
        let selection_start = document_selection
            .anchor_utf16
            .min(document_selection.head_utf16);
        let selection_end = document_selection
            .anchor_utf16
            .max(document_selection.head_utf16);
        let semantic_range = NSRange::new(selection_start, selection_end - selection_start);
        if is_valid_markdown_replacement_range(semantic_range, document_length) {
            // Settled canonical history/selection supplies the first input-method replacement anchor.
            return semantic_range;
        }

        let selected_range = self.super_selected_range();
        if is_valid_markdown_replacement_range(selected_range, document_length) {
            // Native selection is the final valid fallback when no canonical composition remains.
            selected_range
        } else {
            // A stale native selection cannot target outside the document; fall back to a safe EOF caret.
            NSRange::new(document_length, 0)
        }
    }

    /// Initialize weak storage/layout backlinks and empty foreground metadata before native construction.
    pub(super) fn new(
        text_storage: &Retained<PieceBackedMarkdownTextStorage>,
        text_container: &NSTextContainer,
        text_layout_manager: &Retained<NSTextLayoutManager>,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let text_view = Self::alloc(mtm).set_ivars(PieceBackedTextViewIvars {
            #[cfg(feature = "native-input-trace")]
            input_trace: Rc::clone(&text_storage.ivars().input_trace),
            text_storage: Weak::from_retained(text_storage),
            source_decorations: RefCell::new(Vec::new()),
            source_decoration_generation: Cell::new(0),
            source_decorations_dirty: Cell::new(true),
            gutter: OnceCell::new(),
            folds: RefCell::new(Vec::new()),
            fold_generation: Cell::new(0),
            marked_text_range: Cell::new(None),
            unmarking_text: Cell::new(false),
            text_layout_manager: OnceCell::new(),
            text_container_recovery_depth: Cell::new(0),
            textkit_graph_teardown: Cell::new(false),
            textkit_input_depth: Cell::new(0),
            visible_table_layout_cache: RefCell::new(None),
            table_layout_update_scheduled: Cell::new(false),
            table_layout_rebuilding: Cell::new(false),
            cell_editor: OnceCell::new(),
            active_table_cell: RefCell::new(None),
            pending_table_cell_activation: Cell::new(None),
        });
        // SAFETY: all Rust ivars are initialized, and the surface already owns the live container,
        // primary TextKit 2 manager and content store. This is NSTextView's designated initializer;
        // it borrows that exact graph on the main thread instead of constructing a competing network.
        let text_view: Retained<Self> = unsafe {
            msg_send![super(text_view), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)), textContainer: text_container]
        };
        // Preserve only a weak recovery backlink; the enclosing editor retains this exact manager.
        let _ = text_view
            .ivars()
            .text_layout_manager
            .set(Weak::from_retained(text_layout_manager));
        text_view
    }

    /// Commit visible input before applying one canonical memento and its receipt selection.
    pub(super) fn restore_history(&self, direction: MarkdownHistoryDirection) {
        if !self.commit_cell_before_command() {
            // An uncommittable visible cell draft vetoes history navigation instead of disappearing.
            return;
        }
        if !self.commit_composition_before_command() {
            // Native composition failure vetoes structural history without accepting an unsaved later mutation.
            return;
        }
        self.unfold_source();
        let Some(text_storage) = self.ivars().text_storage.load() else {
            // A disconnected weak storage cannot mutate or navigate a successor document.
            return;
        };
        let Some(edit) = text_storage.restore_history(direction) else {
            // Empty undo/redo history contributes no native selection or geometry transaction.
            return;
        };
        let selection_start = edit
            .selection_after
            .anchor_utf16
            .min(edit.selection_after.head_utf16);
        let selection_end = edit
            .selection_after
            .anchor_utf16
            .max(edit.selection_after.head_utf16);
        self.setSelectedRange(NSRange::new(
            selection_start,
            selection_end - selection_start,
        ));
        self.schedule_visible_table_layout_update();
    }

    /// Queues dismissal using this exact editor identity without extending its lifetime.
    pub(super) fn enqueue_markdown_dismissal(&self) {
        let weak = MainThreadBound::new(Weak::from(self), self.mtm());
        enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("Markdown dismissal runs on the main queue");
            let is_current = weak.get(mtm).load().is_some_and(|view| {
                // A retained old shell has no authority over a newly mounted note or session.
                ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
                    active
                        .as_ref()
                        .and_then(|surface| surface.editor.as_ref())
                        .is_some_and(|editor| std::ptr::eq(&*editor.text_view, &*view))
                })
            });
            if !is_current {
                // The originating editor closed or changed before this queued command executed.
                return;
            }
            if let Err(error) = request_native_markdown_window_dismiss() {
                // A save conflict leaves the same active draft editable and reports the failure.
                eprintln!("Markdown dismissal failed: {error}");
            }
        });
    }

    /// Read the superclass state directly, avoiding a semantic adapter masking a native provisional span.
    fn native_marked_state(&self) -> (bool, NSRange) {
        // SAFETY: the matching public NSTextInputClient getters are synchronous on this live main-thread superclass.
        unsafe {
            (
                msg_send![super(self), hasMarkedText],
                msg_send![super(self), markedRange],
            )
        }
    }

    #[cfg(feature = "native-markdown-runtime-probe")]
    /// Expose the same actual superclass composition observation used by production preflight.
    pub(super) fn native_marked_state_for_probe(&self) -> (bool, NSRange) {
        self.native_marked_state()
    }

    /// Commit both native and semantic composition, rejecting a remaining provisional span before save/teardown.
    pub(super) fn commit_composition_before_release(&self) -> Result<(), String> {
        let input_client = ProtocolObject::<dyn NSTextInputClient>::from_ref(self.as_super());
        if input_client.hasMarkedText() || self.native_marked_state().0 {
            // The standard input hook commits either visible adapter state or an outstanding native-only span.
            input_client.unmarkText();
        }
        let native = self.native_marked_state();
        if input_client.hasMarkedText()
            || native.0
            || (native.1.location != NSNotFound as usize && native.1.length != 0)
        {
            // A native refusal cannot be hidden by clearing only custom metadata or discarding its source owner.
            return Err("The native Markdown composition could not be committed".into());
        }
        if let Some(text_storage) = self.ivars().text_storage.load() {
            // Flush finite derived invalidation only after both native and canonical composition have ended.
            text_storage.finish_composition();
        }
        Ok(())
    }

    /// Convert native composition rejection into a consumed command while keeping its editable state visible.
    pub(super) fn commit_composition_before_command(&self) -> bool {
        match self.commit_composition_before_release() {
            // No provisional text remains; subsequent persistence/history may proceed.
            Ok(()) => true,
            // Native void/bool commands report failure and must not fall through into another edit.
            Err(error) => {
                present_native_markdown_error(&error);
                false
            }
        }
    }
}

/// Rejects NSNotFound, overflow and out-of-document native replacement geometry.
pub(super) fn is_valid_markdown_replacement_range(range: NSRange, document_length: usize) -> bool {
    range.location != NSNotFound as usize
        && range
            .location
            .checked_add(range.length)
            .is_some_and(|end| end <= document_length)
}

/// Reads supported native input length without flattening its string payload.
pub(super) fn string_utf16_length(string: &AnyObject) -> usize {
    // Both NSString and NSAttributedString expose the same UTF-16 `length`
    // primitive used by NSTextInputClient. The dynamic input object is owned by
    // AppKit for the duration of this call, so no string copy is required.
    // SAFETY: NSTextInputClient supplies NSString or NSAttributedString, both implement length returning NSUInteger; the borrow lives through this call.
    unsafe { msg_send![string, length] }
}

/// Recognizes the exact native Command-W equivalent from unmodified event characters and modifiers.
pub(super) fn is_exact_markdown_command_w_event(event: &NSEvent) -> bool {
    let Some(characters) = event.charactersIgnoringModifiers() else {
        // A key event without character identity cannot claim this command equivalent.
        return false;
    };
    autoreleasepool(|pool| {
        // SAFETY: the borrowed string is compared only inside this autorelease pool and cannot escape.
        is_exact_markdown_command_w(unsafe { characters.to_str(pool) }, event.modifierFlags())
    })
}

/// Accepts Command-W only when no additional device-independent modifiers alter its meaning.
pub(super) fn is_exact_markdown_command_w(
    characters_ignoring_modifiers: &str,
    modifiers: NSEventModifierFlags,
) -> bool {
    characters_ignoring_modifiers.eq_ignore_ascii_case("w")
        && modifiers.intersection(NSEventModifierFlags::DeviceIndependentFlagsMask)
            == NSEventModifierFlags::Command
}
