//! Owns one canonical document and its explicitly retained AppKit/TextKit graph.
//!
//! Construction attaches the public graph in dependency order. Drop stops callbacks and removes
//! container/content/storage edges before releasing the document, caches and native owners.

use super::*;

/// Owns the sole active native document and its TextKit storage adapter.
///
/// Dropping this value synchronously releases its canonical piece tree, edit segments,
/// structural history and private source mapping. Its native storage claim also ends here;
/// transient platform ownership of the now-weak adapter is verified by the deallocation cohort.
pub(crate) struct NativeMarkdownTextModel {
    pub(super) document: SharedMarkdownDocument,
    pub(super) text_storage: Retained<PieceBackedMarkdownTextStorage>,
}

impl NativeMarkdownTextModel {
    /// Establish the unique strong canonical owner before constructing its weak virtual-storage adapter.
    pub(crate) fn new(document: MarkdownDocumentBuffer, mtm: MainThreadMarker) -> Self {
        let document = Rc::new(RefCell::new(document));
        let text_storage = PieceBackedMarkdownTextStorage::new(Rc::clone(&document), mtm);
        Self {
            document,
            text_storage,
        }
    }

    /// Borrow the shared handle without transferring a second persistent document ownership root.
    pub(crate) fn document(&self) -> &SharedMarkdownDocument {
        &self.document
    }

    /// Expose the native storage supertype for its public TextKit observer relationship.
    pub(crate) fn text_storage(&self) -> &NSTextStorage {
        &self.text_storage
    }
}

/// Native TextKit 2 editor ownership root.
///
/// NSTextView accesses the document only through PieceBackedMarkdownTextStorage.
/// NSScrollView owns viewport/scroll presentation. No setString:, setAttributedString:
/// or equivalent whole-document Foundation mirror is used.
pub(crate) struct NativeMarkdownEditorSurface {
    pub(super) scroll_view: Retained<NSScrollView>,
    pub(super) gutter: Retained<NativeMarkdownGutter>,
    pub(super) text_view: Retained<PieceBackedMarkdownTextView>,
    pub(super) text_content_storage: Retained<objc2_app_kit::NSTextContentStorage>,
    // The editor explicitly owns the complete manager/container network independently of
    // transient AppKit attachment callbacks and their separate actual-deallocation signal.
    pub(super) text_layout_manager: Retained<NSTextLayoutManager>,
    pub(super) text_container: Retained<NSTextContainer>,
    pub(super) model: NativeMarkdownTextModel,
}

impl NativeMarkdownEditorSurface {
    /// Assemble a virtual-storage TextKit 2 graph and its scroll/ruler owners in dependency order.
    pub(crate) fn new_with_mode(
        document: MarkdownDocumentBuffer,
        frame: NSRect,
        presentation_mode: MarkdownPresentationMode,
        mtm: MainThreadMarker,
    ) -> Self {
        let model = NativeMarkdownTextModel::new(document, mtm);
        model.text_storage.set_presentation_mode(presentation_mode);
        // The surface constructs one complete TextKit 2 graph before exposing it to NSTextView.
        // Replacing the storage in a view-created default graph left native edit/layout bookkeeping
        // inconsistent on macOS 27: even one character edit warned and kept its old storage alive.
        let text_content_storage = objc2_app_kit::NSTextContentStorage::new();
        let text_layout_manager = NSTextLayoutManager::new();
        let text_container = NSTextContainer::initWithSize(
            NSTextContainer::alloc(),
            NSSize::new(frame.size.width, f64::MAX),
        );
        // Establish primary layout and container ownership before installing the canonical adapter.
        // These nonnull native constructors leave no fallible graph lookup after text-view allocation.
        text_content_storage.addTextLayoutManager(&text_layout_manager);
        text_content_storage.setPrimaryTextLayoutManager(Some(&text_layout_manager));
        text_layout_manager.setTextContainer(Some(&text_container));
        let text_storage_observer =
            ProtocolObject::<dyn NSTextStorageObserving>::from_ref(&*text_content_storage);
        text_storage_observer.setTextStorage(Some(model.text_storage()));
        // The designated view initializer receives the already attached container, preserving the
        // same backing store throughout native initialization, editing and subsequent teardown.
        let text_view = PieceBackedMarkdownTextView::new(
            &model.text_storage,
            &text_container,
            &text_layout_manager,
            mtm,
        );

        let document_frame = NSRect::new(NSPoint::new(0.0, 0.0), frame.size);
        configure_markdown_text_view(&text_view, document_frame);
        text_view.setDelegate(Some(ProtocolObject::from_ref(&*text_view)));
        // SAFETY: content storage's delegate is weak, and the surface retains this main-thread view.
        unsafe { text_content_storage.setDelegate(Some(ProtocolObject::from_ref(&*text_view))) };

        let scroll_view = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
        scroll_view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        scroll_view.setHasVerticalScroller(true);
        scroll_view.setHasHorizontalScroller(false);
        scroll_view.setDrawsBackground(true);
        scroll_view.setDocumentView(Some(&text_view));
        // The surface/scroll own the ruler; its editor backlink remains weak and viewport metadata finite.
        let gutter = NativeMarkdownGutter::new(&scroll_view, &text_view, mtm);
        let _ = text_view.ivars().gutter.set(Weak::from_retained(&gutter));
        scroll_view.setVerticalRulerView(Some(&gutter));
        scroll_view.setHasVerticalRuler(true);
        scroll_view.setRulersVisible(presentation_mode == MarkdownPresentationMode::Source);
        text_view.schedule_visible_table_layout_update();

        Self {
            scroll_view,
            gutter,
            text_view,
            text_content_storage,
            text_layout_manager,
            text_container,
            model,
        }
    }

    /// Borrow the retained viewport owner for mounting or release-barrier observation.
    pub(crate) fn scroll_view(&self) -> &NSScrollView {
        &self.scroll_view
    }

    /// Borrow the inherited editor interface without exposing independent native lifetime authority.
    pub(crate) fn text_view(&self) -> &NSTextView {
        &self.text_view
    }

    /// Reach the sole canonical model for bounded queries and revision-aware persistence.
    pub(crate) fn document(&self) -> &SharedMarkdownDocument {
        self.model.document()
    }

    /// Seal input before accepted window close, preventing late native callbacks from creating unsaved edits.
    pub(super) fn stop_accepting_input(&self) {
        let accepted = self.model.text_storage.ivars().accepts_edits.replace(false);
        self.text_view.ivars().textkit_graph_teardown.set(true);
        if !accepted {
            // Accepted close/handoff may seal more than once; native input policy is changed only once.
            return;
        }
        // Native setter callbacks now see the sealed canonical/graph gates before changing responder policy.
        self.text_view.setEditable(false);
        self.text_view.setSelectable(false);
    }

    /// Changes derived source/preview presentation without changing canonical Markdown bytes.
    pub(crate) fn set_presentation_mode(
        &self,
        mode: MarkdownPresentationMode,
    ) -> Result<(), String> {
        // A deliberate mode switch ends native provisional composition before changing its derived style projection.
        self.text_view.commit_composition_before_release()?;
        self.text_view.invalidate_visible_table_layouts(true)?;
        self.text_view.unfold_source();
        self.model.text_storage.set_presentation_mode(mode);
        // The preserved editor exposes line/fold gutters only in literal source presentation.
        self.scroll_view
            .setRulersVisible(mode == MarkdownPresentationMode::Source);
        self.text_view.schedule_visible_table_layout_update();
        Ok(())
    }

    /// Inserts one small source table with valid block spacing through the canonical text-input path.
    pub(crate) fn insert_default_markdown_table(&self) {
        self.text_view.insert_default_markdown_table();
    }
}

impl Drop for NativeMarkdownEditorSurface {
    fn drop(&mut self) {
        // From this point, synchronous callbacks may only disconnect the graph; they must
        // not restore the layout manager through setTextContainer:.
        self.stop_accepting_input();
        // Clear fold and ruler state before removing either side of the weak native client relation.
        self.text_view.ivars().folds.borrow_mut().clear();
        self.text_view
            .ivars()
            .source_decorations
            .borrow_mut()
            .clear();
        self.text_view.ivars().source_decorations_dirty.set(false);
        self.gutter.clear();
        self.scroll_view.setRulersVisible(false);
        self.scroll_view.setVerticalRulerView(None);
        // Cancel pending layout and visible-table state before disconnecting TextKit. Queued
        // work holds only a weak view and cannot resurrect the destroyed editing session.
        self.text_view
            .ivars()
            .table_layout_update_scheduled
            .set(false);
        self.text_view.ivars().table_layout_rebuilding.set(false);
        self.text_view
            .ivars()
            .visible_table_layout_cache
            .borrow_mut()
            .take();
        self.text_view
            .ivars()
            .pending_table_cell_activation
            .set(None);
        self.text_view.ivars().active_table_cell.borrow_mut().take();
        if let Some(editor) = self.text_view.ivars().cell_editor.get() {
            // Only a lazily created field has delegate/field-editor edges; teardown never creates one merely to remove it.
            editor.abortEditing();
            editor.setHidden(true);
            editor.setStringValue(ns_string!(""));
            // The reusable field must sever its borrowed delegate and field-editor relations before
            // the parent view releases its final owner; hidden controls still belong to this session.
            // SAFETY: the surface and main-thread text view still retain the live field during detach.
            unsafe {
                editor.setDelegate(None);
                editor.setTarget(None);
                editor.setAction(None);
            }
            editor.removeFromSuperview();
        }
        // Destruction accepts no text edits. Save/responder preflight committed composition before this
        // ownership boundary; invoking unmarkText here could create unsaved source after the last save.
        // Finish Foundation's lazy attribute bookkeeping while the canonical document
        // is still alive. fixAttributesInRange: is a finite no-op, so this creates no mirror.
        let document_length = self.model.document.borrow().utf16_len();
        self.model
            .text_storage
            .ensureAttributesAreFixedInRange(NSRange::new(0, document_length));
        // NSTextView registers drag types during initialization. Unregister symmetrically
        // before graph release so deferred system transactions stop retaining the view.
        self.text_view.unregisterDraggedTypes();
        // A disconnected old responder must reject further user edits while native ownership unwinds.
        self.text_view.setEditable(false);
        self.text_view.setSelectable(false);
        // Stop native find ownership before dismantling its original responder target. Drag cleanup
        // is an AppKit override callback, not a teardown primitive to invoke outside a drag operation.
        self.text_view.setUsesFindBar(false);
        self.scroll_view.setDocumentView(None);
        self.text_view.setDelegate(None);
        // TextKit 2 may expose a compatibility NSLayoutManager/storage relationship. Clear
        // public delegates, viewport controller and layout queue before disconnecting owners
        // so delayed layout cannot attach the old session again.
        // SAFETY: the surface owns both main-thread objects; clearing delegate and queue ends callbacks before graph ownership is released.
        unsafe {
            self.text_content_storage.setDelegate(None);
            self.text_layout_manager.setLayoutQueue(None);
        }
        self.text_layout_manager.setDelegate(None);
        // The surface owns this exact container even if AppKit temporarily detached it from the manager.
        self.text_container.setTextView(None);
        let text_viewport_controller = self.text_layout_manager.textViewportLayoutController();
        text_viewport_controller.setDelegate(None);
        // Do not query NSTextView.layoutManager here: that compatibility accessor can
        // create/switch to a TextKit 1 graph precisely while TextKit 2 is being released.
        // Existing compatibility managers, if any, are detached through storage enumeration.

        let text_storage_observer =
            ProtocolObject::<dyn NSTextStorageObserving>::from_ref(&*self.text_content_storage);
        let legacy_layout_manager_count = self.model.text_storage.layoutManagers().count();
        native_markdown_trace! {
            eprintln!(
                "[probe] drop graph before view_manager={} view_content={} storage={} manager={} container={} legacy_managers={}",
                self.text_view.textLayoutManager().is_some(),
                self.text_view.textContentStorage().is_some(),
                text_storage_observer.textStorage().is_some(),
                self.text_layout_manager.textContentManager().is_some(),
                self.text_layout_manager.textContainer().is_some(),
                legacy_layout_manager_count,
            );
        }
        if let Some(text_layout_manager) = self.text_view.textLayoutManager()
            && let Some(text_container) = text_layout_manager.textContainer()
        {
            // The manager strongly owns its text container. Clear the container's borrowed view
            // link before removing the layout target to release the viewport graph at teardown.
            text_container.setTextView(None);
            native_markdown_trace! {
                eprintln!(
                    "[probe] drop after container view=nil manager={} content={}",
                    self.text_view.textLayoutManager().is_some(),
                    self.text_view.textContentStorage().is_some(),
                );
            }
            text_layout_manager.setTextContainer(None);
            native_markdown_trace! {
                eprintln!(
                    "[probe] drop after manager container=nil manager={} content={}",
                    self.text_view.textLayoutManager().is_some(),
                    self.text_view.textContentStorage().is_some(),
                );
            }
        }
        // NSTextContentStorage ignores attributedString while textStorage is installed.
        // Detach that observer before replacing the fallback with a valid immutable empty value.
        text_storage_observer.setTextStorage(None);
        // Apple's attributedString contract forbids setting nil even though the imported signature
        // is nullable. A real empty value releases fallback text without violating that native invariant.
        let empty = NSAttributedString::initWithString(NSAttributedString::alloc(), ns_string!(""));
        self.text_content_storage.setAttributedString(Some(&empty));
        // Content storage retains observed storage. Remove both its manager and primary
        // relationship before continuing, preventing teardown callbacks from retaining the
        // piece-backed string and canonical document again.
        self.text_content_storage.setPrimaryTextLayoutManager(None);
        self.text_content_storage
            .removeTextLayoutManager(&self.text_layout_manager);
        // NSTextContainer.setTextView(None) above notifies its view through setTextContainer:.
        // The view setter is an AppKit callback rather than a second direct teardown API.
        let detached_legacy_layout_managers =
            self.model.text_storage.detach_legacy_layout_managers();
        native_markdown_trace! {
            eprintln!(
                "[probe] drop after content storage=nil manager={} content={} detached_legacy_managers={}",
                self.text_view.textLayoutManager().is_some(),
                self.text_view.textContentStorage().is_some(),
                detached_legacy_layout_managers,
            );
        }
        native_markdown_trace! {
            eprintln!(
                "[probe] drop graph after view_manager={} view_content={} storage={} manager={} container={} managers={} primary={}",
                self.text_view.textLayoutManager().is_some(),
                self.text_view.textContentStorage().is_some(),
                text_storage_observer.textStorage().is_some(),
                self.text_layout_manager.textContentManager().is_some(),
                self.text_layout_manager.textContainer().is_some(),
                self.text_content_storage.textLayoutManagers().len(),
                self.text_content_storage
                    .primaryTextLayoutManager()
                    .is_some(),
            );
        }
        // Release derived native attribute dictionaries explicitly after layout detaches.
        // Their paragraph/text-block objects belong to this session, even when AppKit
        // temporarily retains an otherwise disconnected storage shell.
        *self.model.text_storage.ivars().presentation.borrow_mut() =
            MarkdownViewportPresentation::default();
        *self
            .model
            .text_storage
            .ivars()
            .font_run_scratch
            .borrow_mut() = Vec::new();
        self.model
            .text_storage
            .ivars()
            .attribute_cache
            .borrow_mut()
            .entries
            .clear();
        self.model
            .text_storage
            .ivars()
            .font_selection_cache
            .borrow_mut()
            .style_fonts
            .clear();
        self.model
            .text_storage
            .ivars()
            .font_selection_cache
            .borrow_mut()
            .fallback_fonts
            .clear();
    }
}

/// Configures bounded plain-source editing, native input and accessibility without an independent undo/checking model.
pub(super) fn configure_markdown_text_view(text_view: &NSTextView, frame: NSRect) {
    // Scroll owns vertical overflow; fixed horizontal tracking wraps source without a document-sized native buffer.
    text_view.setFrame(frame);
    text_view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    text_view.setMinSize(NSSize::new(0.0, frame.size.height));
    text_view.setMaxSize(NSSize::new(f64::MAX, f64::MAX));
    text_view.setVerticallyResizable(true);
    text_view.setHorizontallyResizable(false);
    text_view.setTextContainerInset(NSSize::new(36.0, 32.0));
    // Native accessibility reads the same canonical virtual string as editing.
    text_view.setAccessibilityLabel(Some(ns_string!("Markdown editor")));
    text_view.setAccessibilityHelp(Some(ns_string!(
        "Edit Markdown. Use the Document menu for formatting, find and replace, and file actions."
    )));
    text_view.setEditable(true);
    text_view.setSelectable(true);
    text_view.setRichText(false);
    text_view.setImportsGraphics(false);
    // Support plain-text drags plus explicit confined file imports; rich-text graphics
    // may not create a second mutable attributed document behind the piece tree.
    // SAFETY: AppKit exports immutable process-lifetime pasteboard type identifiers.
    let drag_types = unsafe {
        [
            objc2_app_kit::NSPasteboardTypeFileURL,
            NSPasteboardTypeString,
        ]
    };
    text_view.registerForDraggedTypes(&NSArray::from_slice(&drag_types));
    text_view.setDrawsBackground(true);
    text_view.setBackgroundColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.075, 0.084, 0.11, 1.0,
    ));
    text_view.setInsertionPointColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.45, 0.66, 1.0, 1.0,
    )));
    // NSUndoManager would retain a duplicate history. Use only the Rust piece tree's
    // 128 structurally shared revisions and notify TextKit through exact responder actions.
    text_view.setAllowsUndo(false);
    // Native find UI stays enabled in every supported product and acceptance configuration.
    text_view.setUsesFindBar(true);
    text_view.setSmartInsertDeleteEnabled(false);
    text_view.setAutomaticQuoteSubstitutionEnabled(false);
    text_view.setAutomaticDashSubstitutionEnabled(false);
    text_view.setAutomaticTextReplacementEnabled(false);
    text_view.setAutomaticSpellingCorrectionEnabled(false);
    text_view.setAutomaticLinkDetectionEnabled(false);
    text_view.setAutomaticDataDetectionEnabled(false);
    text_view.setAutomaticTextCompletionEnabled(false);
    text_view.setContinuousSpellCheckingEnabled(false);
    text_view.setGrammarCheckingEnabled(false);
    // The switches above do not clear NSTextView's default spelling bit. Markdown owns
    // its semantics, so disable unused NSTextCheckingController work after each edit.
    text_view.setEnabledTextCheckingTypes(0);
    // SAFETY: initUsingTextLayoutManager(true) creates the owned container; these public
    // layout constraints are changed only while its text view is alive.
    if let Some(text_container) = unsafe { text_view.textContainer() } {
        // A live container follows viewport width while allowing lazy vertical growth.
        text_container.setWidthTracksTextView(true);
        text_container.setHeightTracksTextView(false);
    }
    if let Some(text_layout_manager) = text_view.textLayoutManager() {
        // Retain TextKit's protection against suspicious native layout requests for giant/untrusted source.
        text_layout_manager.setLimitsLayoutForSuspiciousContents(true);
    }
}
