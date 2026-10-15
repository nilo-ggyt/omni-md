//! Exercises real TextKit input and object release on the process main thread.

use super::*;

/// Weak-only release evidence consumed after actual tracked native deallocation, without polling.
///
/// The token owns no native graph, document, map, worker, or timer. Verification runs once on the
/// first queued callback after the final actual deallocation stack, matching product handoff/close.
/// It never polls or permits surviving nodes; the complete graph must be absent when verified.
#[must_use = "native input success requires actual-deallocation completion and strict verification"]
pub struct NativeMarkdownRuntimeProbeCompletion {
    cohort: NativeMarkdownDeallocationCohort,
    panel: Weak<NativeMarkdownPanel>,
    container_view: Weak<NSView>,
    release_barrier: NativeMarkdownEditorReleaseBarrier,
    document: std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    scroll_view: Weak<NSScrollView>,
    gutter: Weak<NativeMarkdownGutter>,
    text_view: Weak<PieceBackedMarkdownTextView>,
    text_storage: Weak<PieceBackedMarkdownTextStorage>,
    text_content_storage: Weak<objc2_app_kit::NSTextContentStorage>,
    text_layout_manager: Weak<NSTextLayoutManager>,
    text_container: Weak<NSTextContainer>,
    table_cell_editor: Option<Weak<NSTextField>>,
}

impl NativeMarkdownRuntimeProbeCompletion {
    /// Queue one strict verification event only after every tracked native role actually deallocates.
    pub fn when_native_objects_released(&self, work: impl FnOnce() + Send + 'static) {
        self.cohort.when_released(work);
    }

    /// Observe bounded role booleans once when the standalone driver's missing-signal deadline fails.
    pub fn failure_diagnostic(&self, _mtm: MainThreadMarker) -> String {
        // Every temporary native upgrade immediately becomes a boolean. This diagnostic cannot
        // accept a survivor, schedule another check or convert missing completion into success.
        format!(
            "cohort=[{}] canonical_released_synchronously={} panel={} root={} document={} scroll={} gutter={} storage={} view={} content={} manager={} container={} cell={}",
            self.cohort.diagnostic(),
            self.release_barrier.canonical_released_synchronously(),
            self.panel.load().is_some(),
            self.container_view.load().is_some(),
            self.document.upgrade().is_some(),
            self.scroll_view.load().is_some(),
            self.gutter.load().is_some(),
            self.text_storage.load().is_some(),
            self.text_view.load().is_some(),
            self.text_content_storage.load().is_some(),
            self.text_layout_manager.load().is_some(),
            self.text_container.load().is_some(),
            self.table_cell_editor
                .as_ref()
                .is_some_and(|cell| cell.load().is_some()),
        )
    }

    /// Require every old weak node/resource to be released before any successor may become accepted.
    pub fn verify_released(self, _mtm: MainThreadMarker) -> Result<(), String> {
        if !self.cohort.has_completed() {
            // Calling the old guessed next-queue observation cannot bypass actual native deallocation authority.
            return Err("the native Markdown probe deallocation cohort has not completed".into());
        }
        // Distinguish actual native completion from synchronous canonical/history/mapping destruction.
        println!(
            "markdown_canonical_released_synchronously={}",
            self.release_barrier.canonical_released_synchronously()
        );
        println!(
            "markdown_native_deallocation_after_us={}",
            self.cohort.elapsed().as_micros()
        );
        println!(
            "markdown_editor_deallocation_after_us={}",
            self.release_barrier.native_release_elapsed().as_micros()
        );
        if self.panel.load().is_some() || self.container_view.load().is_some() {
            // A closed fixture window/content owner is part of the graph, not an allowed harness survivor.
            return Err(
                "the native Markdown probe window/content survived actual completion".into(),
            );
        }
        // The product barrier checks canonical document/storage and scroll ownership first.
        if let Err(error) = self.release_barrier.verify_released() {
            // Keep the strict product failure while exposing every independent weak observation;
            // only booleans escape these temporary loads, with no addresses or document contents.
            return Err(format!(
                "{error}; graph: document={} scroll={} storage={} view={} content={} manager={} container={} cell={}",
                self.document.upgrade().is_some(),
                self.scroll_view.load().is_some(),
                self.text_storage.load().is_some(),
                self.text_view.load().is_some(),
                self.text_content_storage.load().is_some(),
                self.text_layout_manager.load().is_some(),
                self.text_container.load().is_some(),
                self.table_cell_editor.as_ref().is_some_and(|weak| {
                    // A cell absent before first activation cannot be a leaked native editor.
                    weak.load().is_some()
                })
            ));
        }
        if self.document.upgrade().is_some() {
            // A surviving canonical document implies its history/mapping owner escaped surface teardown.
            return Err(
                "the Rust Markdown document survived the native release transaction".to_owned(),
            );
        }
        if self.scroll_view.load().is_some() {
            // An old mounted scroll graph cannot coexist with a successor document surface.
            return Err(
                "the native Markdown scroll view survived the native release transaction"
                    .to_owned(),
            );
        }
        if self.gutter.load().is_some() {
            // The scroll-owned ruler must release its fonts/viewport labels at the same close boundary.
            return Err("the source gutter survived the native release transaction".into());
        }
        if self.text_view.load().is_some() {
            // Pending input/layout references must have ended when the previous native callback returned.
            return Err(format!(
                "the native Markdown text view survived the native release transaction: content={} manager={} container={} cell={}",
                self.text_content_storage.load().is_some(),
                self.text_layout_manager.load().is_some(),
                self.text_container.load().is_some(),
                self.table_cell_editor
                    .as_ref()
                    .is_some_and(|weak| weak.load().is_some())
            ));
        }
        if self.text_storage.load().is_some() {
            // Attributed/native storage must not remain an independent owner after canonical teardown.
            return Err(
                "the native Markdown text storage survived the native release transaction"
                    .to_owned(),
            );
        }
        if self.text_content_storage.load().is_some() {
            // Content-storage ownership must be severed, not hidden by a closed/invisible view.
            return Err(
                "the TextKit 2 content storage survived the native release transaction".to_owned(),
            );
        }
        if self.text_layout_manager.load().is_some() {
            // A retained old layout manager can carry text fragments or queued work across documents.
            return Err(
                "the TextKit 2 layout manager survived the native release transaction".to_owned(),
            );
        }
        if self.text_container.load().is_some() {
            // The old container must release with its manager/view graph before successor acceptance.
            return Err(
                "the TextKit 2 text container survived the native release transaction".to_owned(),
            );
        }
        if self.table_cell_editor.is_some_and(|weak| {
            // Lazy allocation is optional, but any actually-created editor must be fully released.
            weak.load().is_some()
        }) {
            // Hidden reusable controls still count as live ownership and cannot satisfy teardown.
            return Err(
                "the reusable table cell editor survived the native release transaction".to_owned(),
            );
        }
        Ok(())
    }
}

/// Establish that constructing only the real storage/model has no persistent owner before TextKit mounts it.
fn verify_unmounted_native_markdown_model_release(
    path: &Path,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    let document = MarkdownDocumentBuffer::open(path, 2 * 1_024 * 1_024)
        .map_err(|error| format!("failed to open storage countertest fixture: {error}"))?;
    let (cohort, document_weak, storage_weak) = autoreleasepool(|_| {
        let model = NativeMarkdownTextModel::new(document, mtm);
        let document_weak = Rc::downgrade(&model.document);
        let storage_weak = Weak::from_retained(&model.text_storage);
        let mut cohort = NativeMarkdownDeallocationCohort::new(mtm);
        cohort.track(&*model.text_storage, mtm);
        cohort.seal();
        // This countertest creates no text view, manager, input event or native layout transaction.
        drop(model);
        (cohort, document_weak, storage_weak)
    });
    if document_weak.upgrade().is_some() || storage_weak.load().is_some() || !cohort.has_completed()
    {
        // Construction alone must not create a persistent native/storage or canonical ownership cycle.
        return Err(format!(
            "unmounted Markdown model survived: {}",
            cohort.diagnostic()
        ));
    }
    println!("native_unmounted_model_release=passed canonical_and_storage");
    Ok(())
}

/// Owns the mounted synthetic fixture across actual native event transactions.
///
/// Opening, exercising and closing in separate callbacks lets AppKit service initialization and
/// input work as it does for a user session. The fixture retains no production vault or user text.
#[must_use = "the mounted probe must be exercised and then closed before release acceptance"]
pub struct NativeMarkdownRuntimeProbeSession {
    application: Retained<NSApplication>,
    surface: NativeMarkdownEditorSurface,
    panel: Retained<NativeMarkdownPanel>,
    container_view: Retained<NSView>,
    initial_text: String,
    fixture_suffix: String,
}

/// Mounts the actual editor; the caller returns from this callback before exercising its input.
#[cfg(feature = "native-markdown-runtime-probe")]
pub fn run_native_markdown_runtime_probe(
    document_path: &Path,
    mtm: MainThreadMarker,
) -> Result<NativeMarkdownRuntimeProbeSession, String> {
    verify_native_markdown_deallocation_accounting(mtm)?;
    let application = NSApplication::sharedApplication(mtm);
    // A real NSApplication.run harness already completed launch; standalone callers must finish
    // launch once so native main-queue sources share the application's scheduling boundary.
    if !application.isRunning() {
        // Avoid repeating AppKit launch notifications when the probe runs from the real app delegate.
        application.finishLaunching();
    }
    verify_unmounted_native_markdown_model_release(document_path, mtm)?;
    // Open only the caller-owned bounded synthetic document; no production session or vault is consulted.
    let document = MarkdownDocumentBuffer::open(document_path, 2 * 1_024 * 1_024)
        .map_err(|error| format!("failed to open runtime-probe document: {error}"))?;

    // This bounded fixture copy supports independently constructed expectations for subsequent edits.
    let initial_text = std::fs::read_to_string(document_path)
        .map_err(|error| format!("failed to read runtime-probe fixture: {error}"))?;
    let heading_prefix = "# Probe";
    let fixture_suffix = initial_text
        .strip_prefix(heading_prefix)
        .ok_or_else(|| "runtime-probe fixture must start with the Probe heading".to_owned())?
        .to_owned();

    // Mount the same actual TextKit surface as the product, with no alternate minimal/skip path.
    let surface = NativeMarkdownEditorSurface::new_with_mode(
        document,
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(720.0, 520.0)),
        MarkdownPresentationMode::InlinePreview,
        mtm,
    );
    let panel = NativeMarkdownPanel::new(mtm);
    // SAFETY: the probe owns this retained panel through its explicit native close boundary;
    // automatic AppKit release must not consume the Rust owner's independent reference.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setAnimationBehavior(NSWindowAnimationBehavior::None);
    let container_view = panel
        .contentView()
        .ok_or_else(|| "runtime-probe panel has no content view".to_owned())?;
    container_view.addSubview(surface.scroll_view());
    panel.setInitialFirstResponder(Some(surface.text_view()));
    application.activate();
    panel.orderFrontRegardless();
    panel.makeKeyWindow();
    if !panel.makeFirstResponder(Some(surface.text_view())) {
        // Native input exercises require the actual text responder; focus refusal invalidates the test setup.
        return Err("runtime-probe editor did not become first responder".to_owned());
    }
    complete_native_markdown_probe_layout_transaction(
        &application,
        &panel,
        &container_view,
        &surface,
    )?;
    Ok(NativeMarkdownRuntimeProbeSession {
        application,
        surface,
        panel,
        container_view,
        initial_text,
        fixture_suffix,
    })
}

impl NativeMarkdownRuntimeProbeSession {
    /// Runs every input/style/table assertion against the already-mounted product graph.
    pub fn exercise(&self) -> Result<(), String> {
        // Borrow immutable ownership roots only; reentrant AppKit calls never cross a document borrow.
        let Self {
            application,
            surface,
            panel,
            container_view,
            initial_text,
            fixture_suffix,
        } = self;
        let heading_prefix = "# Probe";
        // Check identity through TextKit 2's observer protocol, avoiding legacy layout-manager accessors.
        let text_storage_observer =
            ProtocolObject::<dyn NSTextStorageObserving>::from_ref(&*surface.text_content_storage);
        let installed_storage = text_storage_observer
            .textStorage()
            .ok_or_else(|| "TextKit 2 did not retain the piece-backed text storage".to_owned())?;
        if !std::ptr::eq::<NSTextStorage>(&*installed_storage, surface.model.text_storage()) {
            // A substitute native storage would bypass the canonical piece tree and conceal a duplicate body owner.
            return Err("TextKit 2 installed a different text storage".to_owned());
        }
        drop(installed_storage);

        if surface.text_view().allowsUndo() {
            // Native undo would retain an independent edit history instead of the bounded structural document history.
            return Err("AppKit undo storage remained enabled".to_owned());
        }
        if surface.text_view().enabledTextCheckingTypes() != 0 {
            // Automatic checking could retain text or start unaccounted foreground work outside the declared editor model.
            return Err("AppKit text checking remained enabled".to_owned());
        }
        if surface.scroll_view().documentView().is_none() {
            // A detached scroll hierarchy cannot establish visible layout or native input acceptance.
            return Err("the native Markdown scroll view has no editor document view".to_owned());
        }
        if surface.model.text_storage.presentation_mode() != MarkdownPresentationMode::InlinePreview
        {
            // Initial user-visible presentation must match the product default before semantic style assertions apply.
            return Err("the native Markdown surface did not start in preview mode".to_owned());
        }

        // Exercise view-local fold ownership before style/input checks, then expand the original source.
        surface.set_presentation_mode(MarkdownPresentationMode::Source)?;
        complete_native_markdown_probe_table_layout(&surface.text_view)?;
        surface.text_view.refresh_source_gutter();
        surface.gutter.verify_visible_labels_for_probe()?;
        let fold_generation = surface.document().borrow().generation();
        let fold_length = surface.document().borrow().utf16_len();
        surface.text_view.toggle_source_fold(0)?;
        if surface.text_view.ivars().folds.borrow().is_empty()
            || surface.document().borrow().generation() != fold_generation
            || surface.document().borrow().utf16_len() != fold_length
            || surface.document().borrow().is_dirty()
        {
            // Folding must omit presentation paragraphs without mutating source, history, or saved identity.
            return Err("native source folding changed canonical text or failed to retain its fold interval".into());
        }
        surface.text_view.unfold_source();
        if !surface.text_view.ivars().folds.borrow().is_empty() {
            // Expansion must synchronously release every hidden interval before native editing begins.
            return Err("native source expansion retained a hidden interval".into());
        }
        complete_native_markdown_probe_table_layout(&surface.text_view)?;

        surface.set_presentation_mode(MarkdownPresentationMode::InlinePreview)?;
        // The menu adapter's shared atomic formatting transaction notifies real native storage once.
        let format_edit = surface.model.text_storage.apply_format(
            MarkdownFormatAction::Bold,
            MarkdownTextSelection {
                anchor_utf16: 2,
                head_utf16: 7,
            },
        )?;
        surface.text_view.setSelectedRange(NSRange::new(
            format_edit.selection_after.anchor_utf16,
            format_edit.selection_after.head_utf16 - format_edit.selection_after.anchor_utf16,
        ));
        if surface.document().borrow().utf16_len() != fold_length + 4 {
            // Paired strong delimiters add exactly four code units around the selected fixture label.
            return Err(
                "native format storage notification did not commit the canonical wrapper".into(),
            );
        }
        surface
            .text_view
            .restore_history(MarkdownHistoryDirection::Undo);
        if surface.document().borrow().utf16_len() != fold_length
            || surface.document().borrow().is_dirty()
        {
            // A single undo restores the original saved root before the remaining independent assertions.
            return Err(
                "native formatting did not restore its exact saved root in one undo".into(),
            );
        }
        complete_native_markdown_probe_table_layout(&surface.text_view)?;

        // Heading rendering must add heading semantics without accidentally introducing underline.
        let heading_index = markdown_probe_utf16_offset(
            initial_text,
            initial_text
                .find("Probe")
                .ok_or_else(|| "runtime-probe fixture has no heading text".to_owned())?,
        )?;
        assert_native_markdown_attributes(surface, heading_index, false, "heading")?;
        assert_native_markdown_styles(
            surface,
            heading_index,
            &[MarkdownPresentationStyles::HEADING_1],
            &[MarkdownPresentationStyles::UNDERLINE],
            "heading",
        )?;

        // Table delimiters own table semantics even when their dash syntax resembles a horizontal rule.
        let table_delimiter_index = markdown_probe_utf16_offset(
            initial_text,
            initial_text
                .find(":---")
                .ok_or_else(|| "runtime-probe fixture has no table delimiter".to_owned())?,
        )?;
        assert_native_markdown_attributes(
            surface,
            table_delimiter_index,
            false,
            "table delimiter",
        )?;
        assert_native_markdown_styles(
            surface,
            table_delimiter_index,
            &[
                MarkdownPresentationStyles::TABLE,
                MarkdownPresentationStyles::TABLE_DELIMITER,
            ],
            &[
                MarkdownPresentationStyles::HORIZONTAL_RULE,
                MarkdownPresentationStyles::UNDERLINE,
                MarkdownPresentationStyles::HEADING_1,
                MarkdownPresentationStyles::HEADING_2,
                MarkdownPresentationStyles::HEADING_3,
                MarkdownPresentationStyles::HEADING_4,
                MarkdownPresentationStyles::HEADING_5,
                MarkdownPresentationStyles::HEADING_6,
            ],
            "table delimiter",
        )?;
        assert_native_markdown_table_recovered(surface, table_delimiter_index)?;
        assert_native_markdown_table_layout(surface)?;
        assert_native_markdown_table_input_guards(surface)?;

        // Scroll to the final table and invalidate derived geometry before checking EOF visibility.
        let eof_table_header_index = markdown_probe_utf16_offset(
            initial_text,
            initial_text
                .rfind("| 阶段 | 交付核心物料 | 周期 | 风险预案 |")
                .ok_or_else(|| "runtime-probe fixture has no EOF table".to_owned())?,
        )?;
        let clip_view = surface.scroll_view.contentView();
        let document_height = surface
            .text_view()
            .frame()
            .size
            .height
            .max(surface.text_view().bounds().size.height);
        let viewport_height = clip_view.bounds().size.height;
        clip_view.scrollToPoint(NSPoint::new(
            0.0,
            (document_height - viewport_height).max(0.0),
        ));
        surface.scroll_view.reflectScrolledClipView(&clip_view);
        surface
            .text_view
            .ivars()
            .visible_table_layout_cache
            .borrow_mut()
            .take();
        surface
            .text_view
            .ivars()
            .table_layout_update_scheduled
            .set(false);
        surface.text_view().setNeedsDisplay(true);
        complete_native_markdown_probe_layout_transaction(
            application,
            panel,
            container_view,
            surface,
        )?;
        assert_native_markdown_eof_table_layout(surface, eof_table_header_index)?;

        // Link semantics do not imply underline; only explicit Markdown underline may add that attribute.
        let link_index = markdown_probe_utf16_offset(
            initial_text,
            initial_text
                .find("link")
                .ok_or_else(|| "runtime-probe fixture has no link text".to_owned())?,
        )?;
        assert_native_markdown_attributes(surface, link_index, false, "link")?;
        assert_native_markdown_styles(
            surface,
            link_index,
            &[MarkdownPresentationStyles::LINK],
            &[MarkdownPresentationStyles::UNDERLINE],
            "link",
        )?;

        // The explicit underline fixture distinguishes product style rules from incidental native defaults.
        let underline_index = markdown_probe_utf16_offset(
            initial_text,
            initial_text
                .find("underlined")
                .ok_or_else(|| "runtime-probe fixture has no explicit underline".to_owned())?,
        )?;
        assert_native_markdown_attributes(surface, underline_index, true, "explicit underline")?;
        assert_native_markdown_styles(
            surface,
            underline_index,
            &[MarkdownPresentationStyles::UNDERLINE],
            &[],
            "explicit underline",
        )?;

        // A true rule must remain distinct from both table delimiters and heading syntax.
        let horizontal_rule_byte = initial_text
            .find("\n---\n")
            .map(|offset| offset + 1)
            .ok_or_else(|| "runtime-probe fixture has no horizontal rule".to_owned())?;
        let horizontal_rule_index =
            markdown_probe_utf16_offset(initial_text, horizontal_rule_byte)?;
        assert_native_markdown_attributes(
            surface,
            horizontal_rule_index,
            false,
            "horizontal rule",
        )?;
        assert_native_markdown_styles(
            surface,
            horizontal_rule_index,
            &[MarkdownPresentationStyles::HORIZONTAL_RULE],
            &[
                MarkdownPresentationStyles::UNDERLINE,
                MarkdownPresentationStyles::HEADING_1,
                MarkdownPresentationStyles::HEADING_2,
            ],
            "horizontal rule",
        )?;

        // Source mode preserves every delimiter while using bounded syntax coloring and uniform typography.
        surface.set_presentation_mode(MarkdownPresentationMode::Source)?;
        if surface.model.text_storage.presentation_mode() != MarkdownPresentationMode::Source {
            // Source mode must actually remove preview geometry/concealment without changing canonical text.
            return Err("the native Markdown surface did not enter source mode".to_owned());
        }
        assert_native_markdown_attributes(surface, heading_index, false, "source heading")?;
        assert_native_markdown_styles(
            surface,
            heading_index,
            &[MarkdownPresentationStyles::HEADING_1],
            &[MarkdownPresentationStyles::UNDERLINE],
            "source heading",
        )?;
        // Literal source markers and headings share a readable uniform baseline rather than preview concealment.
        let (marker_attributes, _) = surface.model.text_storage.attributes_at_index(0);
        let (heading_attributes, _) = surface
            .model
            .text_storage
            .attributes_at_index(heading_index);
        // SAFETY: AppKit exports the immutable font key, and this module creates NSFont values for it.
        let font_key = unsafe { NSFontAttributeName };
        let marker_font = marker_attributes
            .objectForKey(font_key)
            .and_then(|font| font.downcast::<NSFont>().ok())
            .ok_or("source marker has no native font")?;
        let heading_font = heading_attributes
            .objectForKey(font_key)
            .and_then(|font| font.downcast::<NSFont>().ok())
            .ok_or("source heading has no native font")?;
        if marker_font.pointSize() != 14.0
            || heading_font.pointSize() != 14.0
            || !surface.scroll_view.rulersVisible()
        {
            // Source styling must not accidentally shrink punctuation or retain preview heading heights.
            return Err(
                "native Source mode did not preserve literal marker geometry and visible gutter"
                    .into(),
            );
        }
        assert_native_markdown_attributes(surface, underline_index, false, "source underline")?;
        surface.set_presentation_mode(MarkdownPresentationMode::InlinePreview)?;
        if surface.model.text_storage.presentation_mode() != MarkdownPresentationMode::InlinePreview
        {
            // Preview re-entry must restore derived semantics rather than leaving the source-mode presentation active.
            return Err("the native Markdown surface did not return to preview mode".to_owned());
        }
        assert_native_markdown_attributes(
            surface,
            underline_index,
            true,
            "preview underline after mode switch",
        )?;

        // An out-of-range NSString bulk read must initialize the caller's buffer deterministically.
        let mut invalid_bulk_read = [0xdead_u16; 2];
        let invalid_bulk_offset = surface.document().borrow().utf16_len().saturating_add(1);
        copy_piece_backed_utf16_range(
            &surface.document().borrow(),
            invalid_bulk_offset,
            &mut invalid_bulk_read,
        );
        if invalid_bulk_read != [0xfffd_u16; 2] {
            // Even rejected native string reads must initialize all requested output units deterministically.
            return Err("an invalid bulk NSString read left output bytes uninitialized".to_owned());
        }
        assert_textkit_matches_piece_tree(surface)?;

        // Drive the public input-client contract, including AppKit's current-selection sentinel.
        let input_client = ProtocolObject::<dyn NSTextInputClient>::from_ref(surface.text_view());
        let no_explicit_replacement = NSRange::new(NSNotFound as usize, 0);

        surface
            .text_view()
            .setSelectedRange(NSRange::new(heading_prefix.encode_utf16().count(), 0));
        let ascii = NSString::from_str(" A");
        // SAFETY: this live NSString is an accepted NSTextInputClient dynamic input value;
        // the main-thread view and its valid current selection outlive the synchronous call.
        unsafe {
            input_client.insertText_replacementRange(&ascii, no_explicit_replacement);
        }
        expect_document_text(surface, &format!("{heading_prefix} A{fixture_suffix}"))?;
        let selection_after_ascii = surface.text_view().selectedRange();
        if selection_after_ascii != NSRange::new(heading_prefix.encode_utf16().count() + 2, 0) {
            // The accepted insertion must advance by its UTF-16 length without losing the native selection.
            return Err(format!(
                "TextKit moved the caret after ASCII insertion: location={}, length={}",
                selection_after_ascii.location, selection_after_ascii.length
            ));
        }
        assert_textkit_matches_piece_tree(surface)?;

        // Two successive marked-text updates emulate IME composition without duplicating the first value.
        let marked_start = heading_prefix.encode_utf16().count() + 2;
        let marked_ascii = NSString::from_str("zh");
        // SAFETY: the live NSString satisfies the dynamic marked-text argument contract;
        // the selected UTF-16 range lies within it and NSNotFound requests the current selection.
        unsafe {
            input_client.setMarkedText_selectedRange_replacementRange(
                &marked_ascii,
                NSRange::new(2, 0),
                no_explicit_replacement,
            );
        }
        let first_marked_range = input_client.markedRange();
        if !input_client.hasMarkedText()
            || first_marked_range != NSRange::new(marked_start, marked_ascii.len_utf16())
        {
            // The initial IME candidate must own exactly the inserted composition span, not an unrelated document range.
            return Err(format!(
                "TextKit did not establish the first marked-text range: location={}, length={}",
                first_marked_range.location, first_marked_range.length
            ));
        }
        expect_document_text(surface, &format!("{heading_prefix} Azh{fixture_suffix}"))?;

        // Attributed Chinese replacement must update the same marked span before explicit commit.
        let marked_chinese = NSString::from_str("中文");
        let attributed_marked =
            NSAttributedString::initWithString(NSAttributedString::alloc(), &marked_chinese);
        // SAFETY: this live NSAttributedString is a supported dynamic marked-text input;
        // its internal UTF-16 selection is valid and the main-thread view owns the replacement state.
        unsafe {
            input_client.setMarkedText_selectedRange_replacementRange(
                &attributed_marked,
                NSRange::new(2, 0),
                no_explicit_replacement,
            );
        }
        let chinese_marked_range = input_client.markedRange();
        if !input_client.hasMarkedText()
            || chinese_marked_range != NSRange::new(marked_start, marked_chinese.len_utf16())
        {
            // Replacing a marked candidate must preserve one composition identity while updating its Unicode payload.
            return Err(format!(
                "TextKit lost marked text during the Chinese update: location={}, length={}",
                chinese_marked_range.location, chinese_marked_range.length
            ));
        }
        expect_document_text(surface, &format!("{heading_prefix} A中文{fixture_suffix}"))?;
        input_client.unmarkText();
        if surface.text_view.native_marked_state_for_probe().0 {
            // Public custom getters must not mask a composition still retained by NSTextView itself.
            return Err("NSTextView kept its native marked state after composition commit".into());
        }
        if input_client.hasMarkedText() {
            // Committing composition must remove marked state before subsequent Unicode edits and history commands.
            return Err("TextKit did not commit marked text".to_owned());
        }

        // A supplementary scalar and combining mark expose UTF-16 versus byte/scalar boundary mistakes.
        let unicode_tail = NSString::from_str("😀e\u{301}");
        // SAFETY: this live NSString is a supported dynamic input; the main-thread TextKit
        // responder owns the valid current selection selected by the NSNotFound replacement sentinel.
        unsafe {
            input_client.insertText_replacementRange(&unicode_tail, no_explicit_replacement);
        }
        let edited_text = format!("{heading_prefix} A中文😀e\u{301}{fixture_suffix}");
        expect_document_text(surface, &edited_text)?;
        assert_textkit_matches_piece_tree(surface)?;

        // Preserve an explicit UTF-16 selection spanning Chinese and an emoji, then restore the end caret.
        let unicode_selection_start = heading_prefix.encode_utf16().count() + 2;
        let unicode_selection = NSRange::new(unicode_selection_start, 4);
        surface.text_view().setSelectedRange(unicode_selection);
        if input_client.selectedRange() != unicode_selection {
            // The responder must preserve native code-unit coordinates across multibyte and surrogate text.
            return Err("TextKit did not preserve a UTF-16 selection".to_owned());
        }
        let history_insert_offset = format!("{heading_prefix} A中文😀e\u{301}")
            .encode_utf16()
            .count();
        surface
            .text_view()
            .setSelectedRange(NSRange::new(history_insert_offset, 0));

        // Structural undo/redo must replay canonical edits without enabling AppKit's full-string history.
        let history_probe = NSString::from_str("!");
        // SAFETY: the live NSString and main-thread input client satisfy the synchronous dynamic
        // input contract; the current scalar-aligned UTF-16 selection is the replacement target.
        unsafe {
            input_client.insertText_replacementRange(&history_probe, no_explicit_replacement);
        }
        let history_text = format!("{heading_prefix} A中文😀e\u{301}!{fixture_suffix}");
        expect_document_text(surface, &history_text)?;
        // SAFETY: undo: is a standard responder selector implemented by this main-thread text view;
        // the call takes no borrowed argument or escaping pointer.
        unsafe {
            input_client.doCommandBySelector(sel!(undo:));
        }
        expect_document_text(surface, &edited_text)?;
        assert_textkit_matches_piece_tree(surface)?;
        // SAFETY: redo: is the standard responder selector implemented by this live main-thread view;
        // the canonical structural history and receiver remain owned for the synchronous call.
        unsafe {
            input_client.doCommandBySelector(sel!(redo:));
        }
        expect_document_text(surface, &history_text)?;
        assert_textkit_matches_piece_tree(surface)?;

        // Cell activation, commit, Tab navigation, and cancellation are mandatory acceptance paths.
        assert_native_markdown_table_cell_editing(surface)?;

        // Text input schedules selection scrolling and post-layout actions. Complete a real visible
        // window layout/display transaction before teardown, matching the product ownership boundary.
        complete_native_markdown_probe_layout_transaction(
            application,
            panel,
            container_view,
            surface,
        )?;

        Ok(())
    }

    /// Ends ownership after the exercise callback returned and produces weak-only release evidence.
    pub fn close(self) -> Result<NativeMarkdownRuntimeProbeCompletion, String> {
        // Destructuring transfers the unique graph into this finite teardown transaction.
        let Self {
            surface,
            panel,
            container_view,
            ..
        } = self;
        let mtm = MainThreadMarker::new().expect("native probe teardown runs on the main thread");
        let mut cohort = NativeMarkdownDeallocationCohort::new(mtm);
        cohort.track(&*panel, mtm);
        cohort.track(&*container_view, mtm);
        let panel_weak = Weak::from_retained(&panel);
        let container_view_weak = Weak::from_retained(&container_view);
        // Snapshot weak release evidence only; explicitly drop temporary retained layout handles below.
        let document_weak = Rc::downgrade(surface.document());
        let scroll_view_weak = Weak::from_retained(&surface.scroll_view);
        let gutter_weak = Weak::from_retained(&surface.gutter);
        let text_view_weak = Weak::from_retained(&surface.text_view);
        let text_storage_weak = Weak::from_retained(&surface.model.text_storage);
        let text_content_storage_weak = Weak::from_retained(&surface.text_content_storage);
        let text_layout_manager = surface
            .text_view
            .textLayoutManager()
            .ok_or_else(|| "TextKit 2 layout manager disappeared before teardown".to_owned())?;
        let text_container = text_layout_manager
            .textContainer()
            .ok_or_else(|| "TextKit 2 text container disappeared before teardown".to_owned())?;
        let text_layout_manager_weak = Weak::from_retained(&text_layout_manager);
        let text_container_weak = Weak::from_retained(&text_container);
        let table_cell_editor_weak = surface
            .text_view
            .ivars()
            .cell_editor
            .get()
            .map(Weak::from_retained);
        drop(text_container);
        drop(text_layout_manager);
        // The window's responder transition asks AppKit to deactivate the input context itself.
        if !panel.makeFirstResponder(None) {
            // A refused resignation leaves AppKit input/caret ownership active and invalidates teardown.
            return Err("the native responder refused to resign before close".into());
        }
        if panel.firstResponder().is_some_and(|responder| {
            std::ptr::eq::<objc2_app_kit::NSResponder>(
                &*responder,
                surface.text_view().as_super().as_super().as_super(),
            )
        }) {
            // Successful resignation must not leave the original text responder installed.
            return Err(
                "the old native text view remained first responder after resignation".into(),
            );
        }
        // The product seals input after successful commit/save/resignation and before hierarchy teardown.
        surface.text_view.commit_composition_before_release()?;
        let sealed_generation = surface.document().borrow().generation();
        let sealed_length = surface.document().borrow().utf16_len();
        surface.stop_accepting_input();
        let late_input = NSString::from_str("late-native-input");
        let input_client = ProtocolObject::<dyn NSTextInputClient>::from_ref(surface.text_view());
        // SAFETY: this supported native string and scalar range stay live through the synchronous public input callback.
        unsafe {
            input_client.insertText_replacementRange(&late_input, NSRange::new(0, 0));
        }
        // A direct delayed TextKit primitive must be sealed as well as the native view input adapter.
        surface
            .model
            .text_storage
            .replaceCharactersInRange_withString(NSRange::new(0, 0), &late_input);
        if surface.document().borrow().generation() != sealed_generation
            || surface.document().borrow().utf16_len() != sealed_length
        {
            // No source mutation may be accepted after the saved close gate, even before final native deallocation.
            return Err("late native input mutated the sealed Markdown document".into());
        }
        panel.setInitialFirstResponder(None);
        panel.orderOut(None);
        panel.close();
        panel.setContentView(None);
        // Use the product release order: detach the editor/TextKit graph before dropping window owners.
        let release_barrier = destroy_native_markdown_editor(surface);
        release_barrier.track_surviving_nodes(&mut cohort, mtm);
        drop(container_view);
        drop(panel);
        cohort.seal();
        Ok(NativeMarkdownRuntimeProbeCompletion {
            cohort,
            panel: panel_weak,
            container_view: container_view_weak,
            release_barrier,
            document: document_weak,
            scroll_view: scroll_view_weak,
            gutter: gutter_weak,
            text_view: text_view_weak,
            text_storage: text_storage_weak,
            text_content_storage: text_content_storage_weak,
            text_layout_manager: text_layout_manager_weak,
            text_container: text_container_weak,
            table_cell_editor: table_cell_editor_weak,
        })
    }
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Consume real window/TextKit geometry and the derived table transaction before inspecting presentation.
fn complete_native_markdown_probe_layout_transaction(
    application: &NSApplication,
    panel: &NativeMarkdownPanel,
    container_view: &NSView,
    surface: &NativeMarkdownEditorSurface,
) -> Result<(), String> {
    // A mounted visible hierarchy supplies valid TextKit geometry, unlike an unhosted view-only shortcut.
    container_view.layoutSubtreeIfNeeded();
    let text_layout_manager = surface
        .text_view()
        .textLayoutManager()
        .ok_or_else(|| "TextKit 2 layout manager disappeared during layout".to_owned())?;
    // Establish TextKit geometry before driving the unchanged derived table transaction body once.
    text_layout_manager.ensureLayoutForBounds(surface.text_view().bounds());
    drop(text_layout_manager);
    complete_native_markdown_probe_table_layout(&surface.text_view)?;
    // Display/update consumes native post-layout actions before this harness requests destruction.
    surface.text_view().displayIfNeeded();
    panel.displayIfNeeded();
    application.updateWindows();
    Ok(())
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Execute one scheduled product layout transaction for deterministic geometry/input assertions.
///
/// This checks the real transaction body and resulting cache; actual dispatch latency and callback
/// delivery require the separate application event-loop acceptance path. No nested run-loop wait may
/// substitute for returning from the current serial native callback.
fn complete_native_markdown_probe_table_layout(
    text_view: &PieceBackedMarkdownTextView,
) -> Result<(), String> {
    // Give TextKit current mounted-view geometry before requesting the product's derived transaction.
    let manager = text_view.textLayoutManager().ok_or_else(|| {
        "TextKit 2 manager missing before the deterministic table transaction".to_owned()
    })?;
    manager.ensureLayoutForBounds(text_view.bounds());
    drop(manager);
    text_view.schedule_visible_table_layout_update();
    // Drive exactly the product transaction body once; an already-queued weak callback becomes inert
    // after this body clears its scheduled flag. This helper does not alter production scheduling.
    text_view.run_scheduled_table_layout_update();
    let current_key = text_view.current_table_layout_request_key();
    let cache_is_current = current_key.is_some_and(|request_key| {
        // The derived cache must match the exact current generation/viewport request identity.
        text_view
            .ivars()
            .visible_table_layout_cache
            .borrow()
            .as_ref()
            .is_some_and(|cache| {
                // A stale successfully generated table is insufficient for the current visible geometry.
                cache.request_key == request_key
            })
    });
    if cache_is_current
        && !text_view.ivars().table_layout_update_scheduled.get()
        && !text_view.ivars().table_layout_rebuilding.get()
    {
        // The one deterministic product transaction completed with the expected cache and idle flags.
        return Ok(());
    }
    // Failure preserves scalar scheduling evidence and cannot become a skipped acceptance path.
    Err(format!(
        "native Markdown deterministic table transaction failed: cache_current={cache_is_current} scheduled={} rebuilding={}",
        text_view.ivars().table_layout_update_scheduled.get(),
        text_view.ivars().table_layout_rebuilding.get()
    ))
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Translate a known fixture byte boundary into the UTF-16 coordinates consumed by native text APIs.
fn markdown_probe_utf16_offset(source: &str, byte_offset: usize) -> Result<usize, String> {
    // Checked slicing rejects a byte offset inside a UTF-8 scalar before counting native code units.
    source
        .get(..byte_offset)
        .map(|prefix| {
            // The checked prefix is scalar-aligned; native text APIs count its UTF-16 units.
            prefix.encode_utf16().count()
        })
        .ok_or_else(|| "runtime-probe byte offset was not a UTF-8 boundary".to_owned())
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Compare the small synthetic fixture with canonical piece-tree text without logging its contents.
fn expect_document_text(
    surface: &NativeMarkdownEditorSurface,
    expected: &str,
) -> Result<(), String> {
    // Probe fixtures are bounded; production never materializes a complete document for comparison.
    let document = surface.document().borrow();
    let slice = document
        .materialize_viewport(0..document.utf16_len(), 0)
        .map_err(|error| format!("failed to inspect bounded probe text: {error}"))?;
    if slice.text == expected {
        // Exact equality proves the input operation produced only the expected canonical edit.
        return Ok(());
    }

    // Failure diagnostics include lengths and the first scalar offset without printing document contents.
    let mismatch = slice
        .text
        .chars()
        .zip(expected.chars())
        .position(|(actual, expected)| {
            // Stop at the first distinct scalar so failure reports remain bounded and text-free.
            actual != expected
        })
        .unwrap_or_else(|| slice.text.chars().count().min(expected.chars().count()));
    Err(format!(
        "the piece tree contains unexpected text (actual_utf16={}, expected_utf16={}, first_mismatch={mismatch})",
        slice.text.encode_utf16().count(),
        expected.encode_utf16().count(),
    ))
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Check bounded native attribute ranges and explicit underline/font semantics at one valid fixture index.
fn assert_native_markdown_attributes(
    surface: &NativeMarkdownEditorSurface,
    index: usize,
    expected_underline: bool,
    semantic_label: &str,
) -> Result<(), String> {
    // A valid canonical UTF-16 position is a prerequisite for the native attribute accessor.
    let document_length = surface.document().borrow().utf16_len();
    if index >= document_length {
        // Reject invalid fixture coordinates before invoking an attributed-string accessor that requires bounds.
        return Err(format!(
            "{semantic_label} attribute probe index is outside the document"
        ));
    }

    let mut effective_range = NSRange::new(0, 0);
    // SAFETY: index is inside the still-owned attributed string; effective_range is a live aligned
    // writable NSRange and the synchronous accessor retains no reference to its stack pointer.
    let attributes = unsafe {
        NSAttributedString::attributesAtIndex_effectiveRange(
            surface.model.text_storage(),
            index,
            &mut effective_range,
        )
    };
    let effective_end = effective_range
        .location
        .checked_add(effective_range.length)
        .ok_or_else(|| format!("{semantic_label} attribute range overflowed"))?;
    if effective_range.length == 0
        || effective_end > document_length
        || effective_range.length > NATIVE_MARKDOWN_ATTRIBUTE_WINDOW_UTF16_UNITS
    {
        // Effective attributes must stay within both canonical document bounds and the presentation-window budget.
        return Err(format!(
            "{semantic_label} attribute range was not a non-empty bounded document range"
        ));
    }

    // SAFETY: these immutable AppKit attribute-name constants have process lifetime.
    let (font_key, underline_key) = unsafe { (NSFontAttributeName, NSUnderlineStyleAttributeName) };
    if attributes.objectForKey(font_key).is_none() {
        // Every accepted native semantic span needs an explicit usable font attribute.
        return Err(format!(
            "{semantic_label} attributes did not contain a font"
        ));
    }
    let has_underline = attributes.objectForKey(underline_key).is_some();
    if has_underline != expected_underline {
        // Underline is authorized only by explicit semantic presentation, not incidental link/rule syntax.
        return Err(format!(
            "{semantic_label} underline attribute presence did not match the presentation contract"
        ));
    }
    Ok(())
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Verify required and forbidden Markdown semantics in the current bounded presentation query.
fn assert_native_markdown_styles(
    surface: &NativeMarkdownEditorSurface,
    index: usize,
    required: &[MarkdownPresentationStyles],
    forbidden: &[MarkdownPresentationStyles],
    semantic_label: &str,
) -> Result<(), String> {
    // Request this position through the same bounded presentation path used by native attribute reads.
    let _ = surface.model.text_storage.attributes_at_index(index);
    let presentation = surface.model.text_storage.ivars().presentation.borrow();
    let query = presentation
        .query(index)
        .ok_or_else(|| format!("{semantic_label} had no presentation query"))?;
    if required.iter().any(|style| {
        // Every requested semantic bit must exist in this independently queried presentation span.
        !query.styles.contains(*style)
    }) {
        // A matching display string alone cannot prove the requested semantic style was actually applied.
        return Err(format!(
            "{semantic_label} was missing a required Markdown presentation style"
        ));
    }
    if forbidden.iter().any(|style| {
        // A single forbidden semantic bit is enough to reject presentation overlap/precedence drift.
        query.styles.contains(*style)
    }) {
        // Overlapping parser markers must not leak unrelated heading/rule/underline styles into this span.
        return Err(format!(
            "{semantic_label} contained a forbidden Markdown presentation style"
        ));
    }
    Ok(())
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Require delimiter parsing to recover the complete six-column semantic table and aligned source ranges.
fn assert_native_markdown_table_recovered(
    surface: &NativeMarkdownEditorSurface,
    delimiter_index: usize,
) -> Result<(), String> {
    // A delimiter query must recover its whole semantic table within the bounded presentation contract.
    let _ = surface
        .model
        .text_storage
        .attributes_at_index(delimiter_index);
    let presentation = surface.model.text_storage.ivars().presentation.borrow();
    let table = presentation
        .tables()
        .iter()
        .find(|table| table.delimiter_utf16_range.contains(&delimiter_index))
        .ok_or_else(|| "table delimiter query did not recover a complete table".to_owned())?;
    if table.rows.len() != 5 || table.cells.len() != 30 {
        // Delimiter recovery must keep the whole expected semantic grid instead of a truncated viewport fragment.
        return Err("table prototype did not retain the six-column fixture".to_owned());
    }
    if !table.rows[0].is_header || table.rows.iter().skip(1).any(|row| row.is_header) {
        // Exactly the first parsed row is a header; body rows must not inherit header semantics.
        return Err("table prototype did not preserve the header row".to_owned());
    }
    // This fixture deliberately combines left/center/right columns to expose semantic alignment drift.
    let expected_alignments = [
        MarkdownTableAlignment::Left,
        MarkdownTableAlignment::Left,
        MarkdownTableAlignment::Center,
        MarkdownTableAlignment::Center,
        MarkdownTableAlignment::Right,
        MarkdownTableAlignment::Left,
    ];
    for (cell_index, cell) in table.cells.iter().enumerate() {
        // Row-major semantic coordinates must repeat the declared alignment for each fixture column.
        if cell.column_index != cell_index % expected_alignments.len()
            || cell.alignment != expected_alignments[cell_index % expected_alignments.len()]
        {
            // Every row-major cell must preserve the alignment declared by its column delimiter.
            return Err("table prototype lost a column alignment".to_owned());
        }
    }
    if table.header_utf16_range.start < table.utf16_range.start
        || table.delimiter_utf16_range.end > table.utf16_range.end
    {
        // All component source ranges must remain confined to their containing semantic table.
        return Err("table prototype ranges escaped the recovered table".to_owned());
    }
    Ok(())
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Check visible table row/cell geometry, column alignment, plain display text, and inline font traits.
fn assert_native_markdown_table_layout(
    surface: &NativeMarkdownEditorSurface,
) -> Result<(), String> {
    // Return to the first table and explicitly invalidate derived geometry before checking a fresh grid.
    let text_view = &surface.text_view;
    surface.scroll_view.layoutSubtreeIfNeeded();
    let clip_view = surface.scroll_view.contentView();
    clip_view.scrollToPoint(NSPoint::new(0.0, 0.0));
    surface.scroll_view.reflectScrolledClipView(&clip_view);
    text_view
        .ivars()
        .visible_table_layout_cache
        .borrow_mut()
        .take();
    complete_native_markdown_probe_table_layout(text_view)?;
    let cache = text_view.ivars().visible_table_layout_cache.borrow();
    let table = cache
        .as_ref()
        .and_then(|cache| {
            // Inspect only the current bounded visible-layout snapshot; no body/native owner escapes.
            cache.tables.iter().find(|table| {
                // Match the independently known fixture identity/shape rather than first-table order.
                table.rows.len() == 5 && table.cells.len() == 30 && table.column_offsets.len() == 7
            })
        })
        .ok_or_else(|| {
            // Missing layout reports bounded viewport/shape metadata without accepting an empty cache.
            let summary = cache.as_ref().map_or_else(
                || "cache=none".to_owned(),
                |cache| {
                    // Copy current native geometry only on failure to locate the missing viewport table.
                    let visible_rect = text_view.visibleRect();
                    let text_origin = text_view.textContainerOrigin();
                    let probe_x = text_origin.x + 1.0;
                    let top_index = text_view.characterIndexForInsertionAtPoint(NSPoint::new(
                        probe_x,
                        visible_rect.origin.y + 1.0,
                    ));
                    let bottom_index = text_view.characterIndexForInsertionAtPoint(NSPoint::new(
                        probe_x,
                        visible_rect.origin.y + (visible_rect.size.height - 1.0).max(0.0),
                    ));
                    let tables = cache
                        .tables
                        .iter()
                        .map(|table| {
                            // Each bounded visible table contributes scalar dimensions only.
                            format!(
                                "rows={},cells={},offsets={}",
                                table.rows.len(),
                                table.cells.len(),
                                table.column_offsets.len()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(";");
                    format!(
                        "visible_rect={:?},bounds={:?},text_origin={:?},viewport={:?},presentation={:?},tables=[{tables}]",
                        visible_rect,
                        text_view.bounds(),
                        text_origin,
                        cache.viewport_utf16_window,
                        cache.utf16_window
                    ) + &format!(",top_index={top_index},bottom_index={bottom_index}")
                },
            );
            format!("the visible layout did not retain the six-column table: {summary}")
        })?;
    // Subpixel rendering may round by less than one point; larger discrepancies violate the shared grid.
    let geometry_matches = |actual: f64, expected: f64| {
        // Compare points in the same native coordinate space with a fixed explicit tolerance.
        (actual - expected).abs() <= 0.75
    };

    if !geometry_matches(table.column_offsets[0], 0.0)
        || !geometry_matches(
            *table.column_offsets.last().unwrap_or(&0.0),
            table.frame.size.width,
        )
        || table
            .column_offsets
            .windows(2)
            .any(|offsets| offsets[1] <= offsets[0])
    {
        // Non-monotonic or incomplete offsets would create overlapping cells or unallocated grid width.
        return Err("the table columns did not form a strictly increasing grid".to_owned());
    }
    if !geometry_matches(table.rows[0].frame.origin.y, table.frame.origin.y)
        || !geometry_matches(
            table
                .rows
                .last()
                .map_or(0.0, |row| row.frame.origin.y + row.frame.size.height),
            table.frame.origin.y + table.frame.size.height,
        )
    {
        // The table frame must cover all row bands exactly without clipping the first or final row.
        return Err("the table frame did not enclose its visible rows".to_owned());
    }

    // This fixture deliberately combines left/center/right columns to expose semantic alignment drift.
    let expected_alignments = [
        MarkdownTableAlignment::Left,
        MarkdownTableAlignment::Left,
        MarkdownTableAlignment::Center,
        MarkdownTableAlignment::Center,
        MarkdownTableAlignment::Right,
        MarkdownTableAlignment::Left,
    ];
    for (row_index, row) in table.rows.iter().enumerate() {
        // Each visible row must preserve semantic order and occupy a contiguous positive grid band.
        if row.row_index != row_index || row.is_header != (row_index == 0) {
            // Native row order/header identity must agree with the parsed row-major semantic model.
            return Err("the visible table rows lost their semantic identity".to_owned());
        }
        if !geometry_matches(row.frame.origin.x, table.frame.origin.x)
            || !geometry_matches(row.frame.size.width, table.frame.size.width)
            || row.frame.size.height <= 0.0
        {
            // Each row must use the common table origin/width and a positive visible height.
            return Err("a table row escaped the table grid".to_owned());
        }
        if row_index > 0 {
            // Adjacent rows must share an exact boundary; the header has no previous row to compare.
            let previous = &table.rows[row_index - 1];
            if !geometry_matches(
                previous.frame.origin.y + previous.frame.size.height,
                row.frame.origin.y,
            ) {
                // Adjacent rows must share their boundary to keep native hit testing and rendering consistent.
                return Err("the table rows contained a vertical gap or overlap".to_owned());
            }
        }

        let row_cells: Vec<&NativeMarkdownVisibleTableCellLayout> = table
            .cells
            .iter()
            .filter(|cell| {
                // The row's visible population must cover exactly the declared set of columns.
                cell.coordinate.row_index == row_index
            })
            .collect();
        if row_cells.len() != expected_alignments.len() {
            // A complete semantic row needs exactly one visible cell per declared fixture column.
            return Err("a visible table row did not contain six cells".to_owned());
        }
        for (column_index, expected_alignment) in expected_alignments.iter().enumerate() {
            // Every declared column must provide one cell with the exact shared row/column geometry.
            let cell = row_cells
                .iter()
                .copied()
                .find(|cell| {
                    // Resolve by semantic coordinate, independent of vector allocation order.
                    cell.coordinate.column_index == column_index
                })
                .ok_or_else(|| "a visible table cell coordinate was missing".to_owned())?;
            let expected_x = table.frame.origin.x + table.column_offsets[column_index];
            let expected_width =
                table.column_offsets[column_index + 1] - table.column_offsets[column_index];
            if cell.alignment != *expected_alignment
                || cell.is_header != row.is_header
                || !geometry_matches(cell.frame.origin.x, expected_x)
                || !geometry_matches(cell.frame.origin.y, row.frame.origin.y)
                || !geometry_matches(cell.frame.size.width, expected_width)
                || !geometry_matches(cell.frame.size.height, row.frame.size.height)
            {
                // Native cell geometry and semantic coordinates must agree with the shared row/column grid.
                return Err("a table cell did not occupy its exact grid slot".to_owned());
            }

            // Text alignment is geometric as well as semantic: verify the actual native text frame.
            let text_minimum_x = cell.text_frame.origin.x;
            let text_maximum_x = cell.text_frame.origin.x + cell.text_frame.size.width;
            let cell_maximum_x = cell.frame.origin.x + cell.frame.size.width;
            let alignment_is_geometric = match cell.alignment {
                // Left-aligned text starts at the shared horizontal padding.
                MarkdownTableAlignment::Left => geometry_matches(
                    text_minimum_x,
                    cell.frame.origin.x + NATIVE_MARKDOWN_TABLE_CELL_HORIZONTAL_PADDING,
                ),
                // Center-aligned text shares the cell midpoint independent of glyph width.
                MarkdownTableAlignment::Center => geometry_matches(
                    text_minimum_x + cell.text_frame.size.width / 2.0,
                    cell.frame.origin.x + cell.frame.size.width / 2.0,
                ),
                // Right-aligned text ends at the shared trailing padding.
                MarkdownTableAlignment::Right => geometry_matches(
                    text_maximum_x,
                    cell_maximum_x - NATIVE_MARKDOWN_TABLE_CELL_HORIZONTAL_PADDING,
                ),
            };
            if !alignment_is_geometric {
                // Declared alignment must affect actual glyph-frame placement, not only cached semantic metadata.
                return Err("a table cell text frame ignored its column alignment".to_owned());
            }
        }
    }

    // Resolve fixture coordinates without relying on incidental native cell allocation order.
    let table_cell = |row_index: usize, column_index: usize| {
        // A missing coordinate remains an explicit acceptance failure at each required use below.
        table.cells.iter().find(|cell| {
            // Both coordinates must agree before a native fixture cell can be inspected.
            cell.coordinate.row_index == row_index && cell.coordinate.column_index == column_index
        })
    };
    let expected_headers = [
        "任务编号 (ID)",
        "模块名称 (Module)",
        "运行状态 (Status)",
        "优先级 (Priority)",
        "响应时间 (Latency)",
        "详细说明 (Notes)",
    ];
    for (column_index, expected) in expected_headers.iter().enumerate() {
        // Native header display must strip source markers while preserving the independent fixture text.
        let cell = table_cell(0, column_index)
            .ok_or_else(|| "a table header cell was missing".to_owned())?;
        if native_markdown_probe_table_cell_text(cell) != *expected {
            // Preview cell display must remove source syntax while preserving header text.
            return Err("a table header retained Markdown source markers".to_owned());
        }
    }

    // Representative code, strong, emoji, and emphasis cells exercise plain display and font semantics.
    let first_id = table_cell(1, 0).ok_or_else(|| "the first ID cell was missing".to_owned())?;
    let first_module =
        table_cell(1, 1).ok_or_else(|| "the first module cell was missing".to_owned())?;
    let first_status =
        table_cell(1, 2).ok_or_else(|| "the first status cell was missing".to_owned())?;
    let last_note =
        table_cell(4, 5).ok_or_else(|| "the final notes cell was missing".to_owned())?;
    if native_markdown_probe_table_cell_text(first_id) != "#101"
        || native_markdown_probe_table_cell_text(first_module) != "用户鉴权中心 Auth"
        || native_markdown_probe_table_cell_text(first_status) != "🟢 正常"
        || native_markdown_probe_table_cell_text(last_note) != "生成文件大小约 12.5 MB"
    {
        // Representative native cell labels must equal the independently known parsed fixture contents.
        return Err("table display text did not match the parsed Markdown semantics".to_owned());
    }
    if first_status.attributed_text.length() != "🟢 正常".encode_utf16().count() {
        // Emoji display must preserve its surrogate length across the native attributed-string boundary.
        return Err("the table status cell lost its UTF-16 emoji length".to_owned());
    }

    assert_native_markdown_table_cell_font_trait(
        first_id,
        "#101",
        NSFontDescriptorSymbolicTraits::TraitMonoSpace,
        "inline-code ID",
    )?;
    assert_native_markdown_table_cell_font_trait(
        first_module,
        "用户鉴权中心",
        NSFontDescriptorSymbolicTraits::TraitBold,
        "strong module name",
    )?;
    assert_native_markdown_table_cell_font_trait(
        first_module,
        "Auth",
        NSFontDescriptorSymbolicTraits::TraitMonoSpace,
        "inline-code module tag",
    )?;
    assert_native_markdown_table_cell_font_trait(
        last_note,
        "12.5 MB",
        NSFontDescriptorSymbolicTraits::TraitItalic,
        "emphasized note size",
    )?;
    Ok(())
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Require a complete native grid for the final four-column table after scrolling to the document tail.
fn assert_native_markdown_eof_table_layout(
    surface: &NativeMarkdownEditorSurface,
    table_utf16_start: usize,
) -> Result<(), String> {
    // Select the tail table by semantic identity and exact expected shape, never by viewport order alone.
    let text_view = &surface.text_view;
    let cache = text_view.ivars().visible_table_layout_cache.borrow();
    let table = cache
        .as_ref()
        .and_then(|cache| {
            // Inspect only the current bounded visible-layout snapshot; no body/native owner escapes.
            cache.tables.iter().find(|table| {
                // Match the independently known fixture identity/shape rather than first-table order.
                table.table_utf16_start == table_utf16_start
                    && table.rows.len() == 5
                    && table.cells.len() == 20
                    && table.column_offsets.len() == 5
            })
        })
        .ok_or_else(|| {
            // Missing layout reports bounded viewport/shape metadata without accepting an empty cache.
            let summary = cache.as_ref().map_or_else(
                || "cache=none".to_owned(),
                |cache| {
                    // A bounded shape summary identifies tail-table omission without printing cell text.
                    format!(
                        "viewport={:?},presentation={:?},tables={:?}",
                        cache.viewport_utf16_window,
                        cache.utf16_window,
                        cache
                            .tables
                            .iter()
                            .map(|table| (
                                table.table_utf16_start,
                                table.rows.len(),
                                table.cells.len()
                            ))
                            .collect::<Vec<_>>()
                    )
                },
            );
            format!("the EOF table was not materialized as a native grid: {summary}")
        })?;

    if table.rows.iter().any(|row| {
        // Every bounded EOF row needs the full expected four-column visible population.
        table
            .cells
            .iter()
            .filter(|cell| cell.coordinate.row_index == row.row_index)
            .count()
            != 4
    }) {
        // The tail viewport must materialize each complete row rather than omitting off-screen-source cells.
        return Err("an EOF table row did not contain four native cells".to_owned());
    }
    let header = table
        .cells
        .iter()
        .find(|cell| cell.coordinate.row_index == 0 && cell.coordinate.column_index == 0)
        .ok_or_else(|| "the EOF table header cell was missing".to_owned())?;
    if native_markdown_probe_table_cell_text(header) != "阶段" {
        // The final table uses the same source-stripping contract as the initial viewport table.
        return Err("the EOF table header retained Markdown source markers".to_owned());
    }
    Ok(())
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Copy the bounded displayed cell string within a local autorelease pool for semantic assertions.
fn native_markdown_probe_table_cell_text(cell: &NativeMarkdownVisibleTableCellLayout) -> String {
    autoreleasepool(|pool| {
        // Cell text is a bounded presentation value; no borrowed Foundation bytes survive this scope.
        let string = cell.attributed_text.string();
        // SAFETY: NSString stays retained and immutable while the pool-scoped UTF-8 borrow
        // is copied into an owned Rust string; no borrowed byte can escape the autorelease pool.
        unsafe { string.to_str(pool).to_owned() }
    })
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Resolve a fixture substring and verify the native font trait used to present that semantic fragment.
fn assert_native_markdown_table_cell_font_trait(
    cell: &NativeMarkdownVisibleTableCellLayout,
    fragment: &str,
    required_trait: NSFontDescriptorSymbolicTraits,
    semantic_label: &str,
) -> Result<(), String> {
    // Locate the independently known semantic fragment before converting its scalar boundary to UTF-16.
    let display_text = native_markdown_probe_table_cell_text(cell);
    let byte_offset = display_text
        .find(fragment)
        .ok_or_else(|| format!("{semantic_label} was absent from the rendered cell"))?;
    let utf16_offset = display_text[..byte_offset].encode_utf16().count();
    let mut effective_range = NSRange::new(0, 0);
    // SAFETY: fragment lookup establishes an in-bounds scalar boundary in this same cell string;
    // effective_range is an aligned live writable NSRange used only by the synchronous accessor.
    let attributes = unsafe {
        NSAttributedString::attributesAtIndex_effectiveRange(
            &cell.attributed_text,
            utf16_offset,
            &mut effective_range,
        )
    };
    // SAFETY: AppKit's immutable font attribute-name constant remains valid for process lifetime.
    let font_key = unsafe { NSFontAttributeName };
    let font = attributes
        .objectForKey(font_key)
        .ok_or_else(|| format!("{semantic_label} had no font attribute"))?
        .downcast::<NSFont>()
        .map_err(|_| format!("{semantic_label} font attribute had the wrong type"))?;
    if !font
        .fontDescriptor()
        .symbolicTraits()
        .contains(required_trait)
    {
        // Parsed inline semantics must survive conversion into concrete AppKit font descriptor traits.
        return Err(format!("{semantic_label} lost its native font trait"));
    }
    Ok(())
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Exercise reentrancy/IME scheduling guards and restore original scalar state on every outcome.
fn assert_native_markdown_table_input_guards(
    surface: &NativeMarkdownEditorSurface,
) -> Result<(), String> {
    // Save scheduling/composition scalars so this adversarial guard exercise cannot alter later tests.
    let text_view = &surface.text_view;
    let ivars = text_view.ivars();
    let original_input_depth = ivars.textkit_input_depth.get();
    let original_marked_range = ivars.marked_text_range.get();
    let original_scheduled = ivars.table_layout_update_scheduled.get();

    let result = (|| {
        // Active TextKit input must refuse derived layout work even with an explicitly missing cache.
        ivars.visible_table_layout_cache.borrow_mut().take();
        ivars.table_layout_update_scheduled.set(false);
        ivars
            .textkit_input_depth
            .set(original_input_depth.saturating_add(1));
        text_view.schedule_visible_table_layout_update();
        if ivars.table_layout_update_scheduled.get()
            || ivars.visible_table_layout_cache.borrow().is_some()
        {
            // Reentrant input must not allocate or publish derived table geometry during canonical mutation.
            return Err("table layout work entered an active TextKit input transaction".to_owned());
        }

        // IME marked text independently blocks scheduling after the input transaction itself has ended.
        ivars.textkit_input_depth.set(original_input_depth);
        ivars.marked_text_range.set(Some(NSRange::new(1, 1)));
        text_view.schedule_visible_table_layout_update();
        if ivars.table_layout_update_scheduled.get()
            || ivars.visible_table_layout_cache.borrow().is_some()
        {
            // Marked composition owns its transient range and blocks table rebuilding until commit/cancel.
            return Err("table layout work entered an active IME composition".to_owned());
        }

        // Clearing both guards must allow the unchanged product transaction body to recover.
        ivars.marked_text_range.set(original_marked_range);
        ivars.table_layout_update_scheduled.set(false);
        complete_native_markdown_probe_table_layout(text_view)?;
        if !ivars
            .visible_table_layout_cache
            .borrow()
            .as_ref()
            .is_some_and(|cache| !cache.tables.is_empty())
        {
            // Clearing input guards must resume the real scheduled layout path rather than leaving it permanently suppressed.
            return Err("table layout did not recover after the input guards cleared".to_owned());
        }
        Ok(())
    })();

    // Restore original scalar guards on both success and any fallible assertion/timeout above.
    ivars.textkit_input_depth.set(original_input_depth);
    ivars.marked_text_range.set(original_marked_range);
    ivars.table_layout_update_scheduled.set(original_scheduled);
    if result.is_err()
        && original_input_depth == 0
        && !original_marked_range.is_some_and(|range| range.length != 0)
    {
        // Failed adversarial work must not leave an apparently valid cache for subsequent assertions.
        ivars.visible_table_layout_cache.borrow_mut().take();
    }
    result
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Validate lazy cell editor ownership, canonical commit, Tab navigation, and cancellation without hidden state.
fn assert_native_markdown_table_cell_editing(
    surface: &NativeMarkdownEditorSurface,
) -> Result<(), String> {
    // Return to the first table and explicitly invalidate derived geometry before checking a fresh grid.
    let text_view = &surface.text_view;
    surface.scroll_view.layoutSubtreeIfNeeded();
    let clip_view = surface.scroll_view.contentView();
    clip_view.scrollToPoint(NSPoint::new(0.0, 0.0));
    surface.scroll_view.reflectScrolledClipView(&clip_view);
    text_view
        .ivars()
        .visible_table_layout_cache
        .borrow_mut()
        .take();
    complete_native_markdown_probe_table_layout(text_view)?;
    // Copy only the first bounded cell layout before leaving the cache borrow and activating native input.
    let (table_utf16_start, first_cell) = {
        let cache = text_view.ivars().visible_table_layout_cache.borrow();
        let table = cache
            .as_ref()
            .and_then(|cache| cache.tables.iter().find(|table| table.cells.len() == 30))
            .ok_or_else(|| {
                let summary = cache.as_ref().map_or_else(
                    || "cache=none".to_owned(),
                    |cache| {
                        let visible_rect = text_view.visibleRect();
                        let text_origin = text_view.textContainerOrigin();
                        format!(
                            "visible_rect={visible_rect:?},text_origin={text_origin:?},viewport={:?},presentation={:?},tables={:?}",
                            cache.viewport_utf16_window,
                            cache.utf16_window,
                            cache
                                .tables
                                .iter()
                                .map(|table| (table.rows.len(), table.cells.len()))
                                .collect::<Vec<_>>()
                        )
                    },
                );
                format!("the editable six-column table was not visible: {summary}")
            })?;
        let first_cell = table
            .cells
            .iter()
            .find(|cell| cell.coordinate.row_index == 1 && cell.coordinate.column_index == 0)
            .cloned()
            .ok_or_else(|| "the first editable body cell was missing".to_owned())?;
        (table.table_utf16_start, first_cell)
    };
    if text_view.ivars().cell_editor.get().is_some() {
        // Idle table presentation must not allocate a reusable field editor before explicit cell interaction.
        return Err("the table cell editor was allocated before first activation".to_owned());
    }
    if !text_view.begin_table_cell_editing(first_cell) {
        // A valid visible body cell must support the normal product activation path.
        return Err("the first table cell did not enter edit mode".to_owned());
    }
    // First activation is the only point allowed to allocate the reusable native cell field.
    let editor = text_view
        .ivars()
        .cell_editor
        .get()
        .ok_or_else(|| "the table cell editor was not lazily allocated".to_owned())?;
    // Exceed this fixture's 2 MiB canonical document ceiling with one explicit synthetic cell draft.
    // Rejection must preserve both its native field and active state, rather than silently hiding input.
    let generation_before_rejection = surface.document().borrow().generation();
    let oversized = NSString::from_str(&"x".repeat(2 * 1_024 * 1_024));
    editor.setStringValue(&oversized);
    if text_view.commit_active_table_cell().is_ok()
        || editor.isHidden()
        || editor.stringValue().length() != oversized.length()
        || text_view.ivars().active_table_cell.borrow().is_none()
        || surface.document().borrow().generation() != generation_before_rejection
    {
        // A failed capacity transaction cannot clear the visible draft, mutate history or release cell ownership.
        return Err(
            "a rejected table edit lost its visible draft or changed canonical text".into(),
        );
    }
    drop(oversized);
    // Restore a valid bounded draft so the unchanged commit/navigation/cancel acceptance continues.
    editor.setStringValue(&NSString::from_str("#101-updated"));
    text_view.navigate_active_table_cell(NativeMarkdownTableNavigation::Next);
    // Drive the same scheduled product transaction body once after Tab commits the first cell.
    // Actual queued-callback latency is measured separately by the real application event loop.
    complete_native_markdown_probe_table_layout(text_view)?;
    let target_activated = !text_view.ivars().table_layout_update_scheduled.get()
        && text_view
            .ivars()
            .pending_table_cell_activation
            .get()
            .is_none()
        && text_view
            .ivars()
            .active_table_cell
            .borrow()
            .as_ref()
            .is_some_and(|active| {
                // The first body cell's row-major successor is column one of that same row.
                active.coordinate.row_index == 1 && active.coordinate.column_index == 1
            });
    if !target_activated {
        // A successful source commit must also install the exact successor edit coordinate.
        return Err(
            "Tab navigation did not activate the next table cell after its transaction".to_owned(),
        );
    }
    let active = text_view
        .ivars()
        .active_table_cell
        .borrow()
        .clone()
        .ok_or_else(|| "the next table cell had no active edit state".to_owned())?;
    if active.coordinate.row_index != 1 || active.coordinate.column_index != 1 {
        // Navigation must advance to the exact next body-cell coordinate, not an arbitrary visible control.
        return Err("Tab navigation did not follow spreadsheet row order".to_owned());
    }

    // Reparse canonical table ranges after commit, then compare source rather than a cached native label.
    let text_storage = &surface.model.text_storage;
    let (first_source_range, second_source_range) = {
        let snapshot = text_storage
            .table_presentation_snapshot_at_index(table_utf16_start)
            .ok_or_else(|| "the edited table was not reparsed".to_owned())?;
        let table = snapshot
            .tables
            .iter()
            .find(|table| table.utf16_range.start == table_utf16_start)
            .ok_or_else(|| "the edited table semantic identity changed".to_owned())?;
        let source_range = |column_index: usize| {
            // Resolve current canonical ranges after reparsing; old ranges may shift with the committed edit.
            table.rows[1]
                .cell_index_range
                .clone()
                .find_map(|cell_index| {
                    // The bounded row range visits only semantic cells belonging to this body row.
                    table.cells.get(cell_index).and_then(|cell| {
                        // Copy only the requested column's scalar source range, not its body.
                        (cell.column_index == column_index).then(|| cell.utf16_range.clone())
                    })
                })
        };
        (
            source_range(0).ok_or_else(|| "the edited first cell range was missing".to_owned())?,
            source_range(1).ok_or_else(|| "the second cell range was missing".to_owned())?,
        )
    };
    if text_storage
        .table_cell_source(&first_source_range)
        .as_deref()
        != Some("#101-updated")
    {
        // Native editing acceptance requires a canonical source mutation, not merely a changed field label.
        return Err("the committed table cell did not update the piece tree".to_owned());
    }
    // Escape must discard the second field's draft and remove all active/pending native edit state.
    let original_second_source = text_storage
        .table_cell_source(&second_source_range)
        .ok_or_else(|| "the second table cell source was unavailable".to_owned())?;
    editor.setStringValue(&NSString::from_str("must-not-be-committed"));
    text_view.cancel_active_table_cell();
    if text_storage
        .table_cell_source(&second_source_range)
        .as_deref()
        != Some(original_second_source.as_str())
    {
        // Cancellation must preserve the original canonical cell source even after editing a draft field.
        return Err("Escape committed a cancelled table cell edit".to_owned());
    }
    if text_view.ivars().active_table_cell.borrow().is_some()
        || text_view
            .ivars()
            .pending_table_cell_activation
            .get()
            .is_some()
        || !editor.isHidden()
        || editor.stringValue().length() != 0
    {
        // Hidden/reusable fields must clear draft text and all active/pending coordinates after cancellation.
        return Err("the cancelled table cell editor retained active state".to_owned());
    }
    Ok(())
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Require the native string primitive to agree with every UTF-16 code unit of the small probe document.
fn assert_textkit_matches_piece_tree(surface: &NativeMarkdownEditorSurface) -> Result<(), String> {
    // Borrow the canonical-backed NSString primitive without asking NSTextView for a legacy layout graph.
    let textkit_string = surface.model.text_storage().string();
    let document = surface.document().borrow();
    if textkit_string.len_utf16() != document.utf16_len() {
        // A native primitive length mismatch would invalidate every downstream selection/range calculation.
        return Err("TextKit and the piece tree disagree on UTF-16 length".to_owned());
    }
    for index in 0..document.utf16_len() {
        // Exhaustive code-unit comparison is bounded by the small synthetic fixture, including surrogates.
        if textkit_string.characterAtIndex(index)
            != document
                .utf16_code_unit_at(index)
                .map_err(|error| format!("failed to inspect probe code unit: {error}"))?
        {
            // The attributed-string primitive must expose the exact canonical code units, including surrogate halves.
            return Err("TextKit and the piece tree disagree on a UTF-16 code unit".to_owned());
        }
    }
    Ok(())
}
