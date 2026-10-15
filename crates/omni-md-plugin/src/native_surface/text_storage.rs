//! Adapts the sole piece-tree document to virtual NSString and bounded NSTextStorage primitives.
//!
//! This module owns derived viewport/font dictionaries and balanced native edit notifications.
//! Every document backlink is weak; native temporary retention cannot prolong canonical text.

use super::*;

/// A native string adapter borrows document lifetime weakly and retains no text bytes.
pub(super) struct PieceBackedStringIvars {
    // AppKit may temporarily retain this string after surface teardown. A weak document
    // link prevents delayed callbacks from extending the canonical text owner's lifetime.
    pub(super) document: WeakMarkdownDocument,
}

define_class!(
    // SAFETY: NSString requires length and characterAtIndex: primitives; getCharacters:range:
    // provides bounded bulk access. The adapter and document remain main-thread-only.
    #[unsafe(super = NSString)]
    #[thread_kind = MainThreadOnly]
    #[ivars = PieceBackedStringIvars]
    pub(super) struct PieceBackedMarkdownString;

    impl PieceBackedMarkdownString {
        #[unsafe(method(length))]
        /// Reports canonical UTF-16 length without allocating a Foundation text mirror.
        fn length(&self) -> NSUInteger {
            // After document teardown a delayed native query observes an empty adapter.
            self.ivars()
                .document
                .upgrade()
                .map_or(0, |document| document.borrow().utf16_len())
        }

        #[unsafe(method(characterAtIndex:))]
        /// Reads one canonical UTF-16 code unit for NSString primitive clients.
        fn character_at_index(&self, index: NSUInteger) -> unichar {
            // Missing/out-of-range document access yields a deterministic replacement code unit.
            self.ivars().document.upgrade().map_or(0xfffd, |document| {
                document
                    .borrow()
                    .utf16_code_unit_at(index)
                    .unwrap_or(0xfffd)
            })
        }

        #[unsafe(method(getCharacters:range:))]
        /// Copies a caller-provided UTF-16 range without retaining a duplicate source buffer.
        ///
        /// # Safety
        /// Foundation must supply a destination writable for range.length code units.
        unsafe fn get_characters_range(&self, destination: NonNull<unichar>, range: NSRange) {
            // SAFETY: Foundation provides a destination writable for at least range.length
            // UTF-16 code units for the duration of this primitive call.
            let destination =
                unsafe { std::slice::from_raw_parts_mut(destination.as_ptr(), range.length) };
            if let Some(document) = self.ivars().document.upgrade() {
                // A live canonical owner supplies the requested bounded UTF-16 range.
                let document = document.borrow();
                copy_piece_backed_utf16_range(&document, range.location, destination);
            } else {
                // A delayed query after teardown must still initialize every destination code unit.
                destination.fill(0xfffd);
            }
        }
    }
);

/// Fills a native buffer from scalar-safe canonical ranges, deterministically replacing invalid requests.
pub(super) fn copy_piece_backed_utf16_range(
    document: &MarkdownDocumentBuffer,
    utf16_offset: usize,
    destination: &mut [unichar],
) {
    let valid_range = utf16_offset
        .checked_add(destination.len())
        .is_some_and(|range_end| range_end <= document.utf16_len());
    if !valid_range
        || document
            .copy_utf16_code_units(utf16_offset, destination)
            .is_err()
    {
        // Objective-C primitives cannot return Rust errors. Invalid requests still initialize
        // the entire destination deterministically rather than exposing unwritten memory.
        destination.fill(0xfffd);
    }
}

impl PieceBackedMarkdownString {
    /// Creates a virtual string with a weak link to the session's unique document owner.
    pub(super) fn new(document: &SharedMarkdownDocument, mtm: MainThreadMarker) -> Retained<Self> {
        // The weak link lets delayed Foundation string queries expire without retaining an old mmap.
        let string = Self::alloc(mtm).set_ivars(PieceBackedStringIvars {
            document: Rc::downgrade(document),
        });
        // SAFETY: the custom class implements the required NSString primitives and initializes
        // all Rust ivars before invoking the superclass initializer.
        unsafe { msg_send![super(string), init] }
    }
}

/// One transient system-required pasteboard string, never retained in plugin history or caches.
pub(super) struct MarkdownPasteboardStringWriter {
    pub(super) value: Retained<NSMutableString>,
}
impl MarkdownPasteboardStringWriter {
    /// Reserves the single native value used only for the user's explicit copy-all operation.
    pub(super) fn with_utf16_capacity(utf16_capacity: usize) -> Self {
        // Capacity is known from piece-tree metrics; appending streams the canonical pieces once.
        Self {
            value: NSMutableString::stringWithCapacity(utf16_capacity),
        }
    }
}

impl Write for MarkdownPasteboardStringWriter {
    /// Appends one UTF-8 piece into the sole transient native pasteboard value.
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        // Invalid source encoding propagates before any uninitialized/native text can be exposed.
        let piece = std::str::from_utf8(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let piece = NSString::from_str(piece);
        self.value.appendString(&piece);
        Ok(bytes.len())
    }

    /// Completes the in-memory writer without creating a file or a retained cache.
    fn flush(&mut self) -> io::Result<()> {
        // Every accepted piece was synchronously appended, so no pending I/O remains.
        Ok(())
    }
}

#[derive(Debug, Error)]
/// Bounded copy-all failures surfaced without mutating the active document.
pub(super) enum MarkdownPasteboardWriteError {
    #[error("无法读取当前 Markdown 正文")]
    DocumentReadUnavailable,
    #[error("系统剪贴板暂时不可用")]
    PasteboardWriteRejected,
}

/// Streams unsaved canonical text into one explicit system pasteboard write.
pub(super) fn write_markdown_document_to_pasteboard(
    document: &MarkdownDocumentBuffer,
    pasteboard: &NSPasteboard,
) -> Result<(), MarkdownPasteboardWriteError> {
    // Complete source streaming before replacing the system pasteboard contents.
    let mut writer = MarkdownPasteboardStringWriter::with_utf16_capacity(document.utf16_len());
    document
        .write_to(&mut writer)
        .map_err(|_| MarkdownPasteboardWriteError::DocumentReadUnavailable)?;

    // The write is explicit user intent; no Omni clipboard-history entry or retained mirror is created.
    pasteboard.clearContents();
    // SAFETY: AppKit exports this immutable pasteboard type for the process lifetime.
    let string_type = unsafe { NSPasteboardTypeString };
    pasteboard
        .setString_forType(&writer.value, string_type)
        .then_some(())
        .ok_or(MarkdownPasteboardWriteError::PasteboardWriteRejected)
}

#[derive(Clone)]
/// One bounded semantic window shared by visible table geometry, tagged with its canonical generation.
pub(super) struct NativeMarkdownTablePresentationSnapshot {
    pub(super) document_generation: u64,
    pub(super) utf16_window: std::ops::Range<usize>,
    pub(super) tables: Vec<MarkdownTablePrototype>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// A visible-cell UTF-16 interval carries only the semantic style required for native drawing.
pub(super) struct NativeMarkdownTableCellStyleSpan {
    pub(super) utf16_range: std::ops::Range<usize>,
    pub(super) styles: MarkdownPresentationStyles,
}

#[derive(Debug, Eq, PartialEq)]
/// A bounded cell projection removes concealed markers while retaining exact visible style coordinates.
pub(super) struct NativeMarkdownTableCellDisplayText {
    pub(super) text: String,
    pub(super) style_spans: Vec<NativeMarkdownTableCellStyleSpan>,
}

/// Derives one cell display string while retaining UTF-16 style spans after hiding Markdown markers.
pub(super) fn native_markdown_table_cell_display_text_from_source(
    source: &str,
    absolute_utf16_start: usize,
    mut styles_at: impl FnMut(usize) -> MarkdownPresentationStyles,
) -> NativeMarkdownTableCellDisplayText {
    // Only this bounded cell is projected: original byte offsets and absolute UTF-16 positions
    // advance independently from the output offsets after hidden markers/escapes disappear.
    let mut text = String::with_capacity(source.len());
    let mut style_spans: Vec<NativeMarkdownTableCellStyleSpan> = Vec::new();
    let mut source_byte_offset = 0;
    let mut absolute_utf16 = absolute_utf16_start;
    let mut visible_utf16 = 0;

    let append_scalar = |scalar: char,
                         styles: MarkdownPresentationStyles,
                         text: &mut String,
                         style_spans: &mut Vec<NativeMarkdownTableCellStyleSpan>,
                         visible_utf16: &mut usize| {
        // Append one visible scalar and coalesce adjacent equal-style output spans.
        text.push(scalar);
        let scalar_range = *visible_utf16..*visible_utf16 + scalar.len_utf16();
        *visible_utf16 = scalar_range.end;
        if let Some(previous) = style_spans.last_mut()
            && previous.utf16_range.end == scalar_range.start
            && previous.styles == styles
        {
            // Equal adjacent styles share one output interval, independent of removed source markers.
            previous.utf16_range.end = scalar_range.end;
        } else {
            // A new style or disjoint span needs its own exact visible UTF-16 range.
            style_spans.push(NativeMarkdownTableCellStyleSpan {
                utf16_range: scalar_range,
                styles,
            });
        }
    };

    // Stream complete source scalars once; this never creates a full-document attributed mirror.
    while source_byte_offset < source.len() {
        let scalar = source[source_byte_offset..]
            .chars()
            .next()
            .expect("有效 UTF-8 标量");
        let scalar_utf16_length = scalar.len_utf16();
        let styles =
            styles_at(absolute_utf16).without(MarkdownPresentationStyles::TABLE_ROW_PLACEHOLDER);
        if styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER)
            || styles.contains(MarkdownPresentationStyles::MARKER)
        {
            // Hidden syntax advances source coordinates while contributing no display text or style span.
            source_byte_offset += scalar.len_utf8();
            absolute_utf16 = absolute_utf16.saturating_add(scalar_utf16_length);
            continue;
        }

        if scalar == '\\' {
            // Escape parity differs inside code spans and normal CommonMark punctuation escapes.
            let run_styles = styles;
            let mut run_length = 0_usize;
            // Count only the contiguous slash run so a later literal character resets escape meaning.
            while source_byte_offset < source.len()
                && source[source_byte_offset..].starts_with('\\')
            {
                run_length += 1;
                source_byte_offset += 1;
                absolute_utf16 = absolute_utf16.saturating_add(1);
            }
            let next_scalar = source[source_byte_offset..].chars().next();
            let next_styles = next_scalar
                .map(|_| {
                    styles_at(absolute_utf16)
                        .without(MarkdownPresentationStyles::TABLE_ROW_PLACEHOLDER)
                })
                .unwrap_or_default();
            let inline_code_escape = next_scalar == Some('|')
                && run_styles.contains(MarkdownPresentationStyles::INLINE_CODE)
                && next_styles.contains(MarkdownPresentationStyles::INLINE_CODE);
            let commonmark_escape = next_scalar.is_some_and(|scalar| scalar.is_ascii_punctuation())
                && !run_styles.contains(MarkdownPresentationStyles::INLINE_CODE);
            let visible_backslashes = if inline_code_escape {
                // GFM table pipes inside inline code consume one table-level escaping backslash.
                run_length.saturating_sub(1)
            } else if commonmark_escape {
                // Outside code, paired backslashes collapse and the odd slash escapes punctuation.
                run_length / 2
            } else {
                // Unrecognized escape targets preserve every literal source backslash.
                run_length
            };
            // Emit only the visible slashes while keeping their original semantic face/style.
            for _ in 0..visible_backslashes {
                append_scalar(
                    '\\',
                    run_styles,
                    &mut text,
                    &mut style_spans,
                    &mut visible_utf16,
                );
            }
            continue;
        }

        // Ordinary visible content keeps its scalar and derived style at the new display offset.
        append_scalar(
            scalar,
            styles,
            &mut text,
            &mut style_spans,
            &mut visible_utf16,
        );
        source_byte_offset += scalar.len_utf8();
        absolute_utf16 = absolute_utf16.saturating_add(scalar_utf16_length);
    }
    NativeMarkdownTableCellDisplayText { text, style_spans }
}

/// Weak document access plus editor-bounded derived attributes and synchronous input state.
pub(super) struct PieceBackedTextStorageIvars {
    // AppKit may retain storage beyond the surface callback. The model owns text, and
    // this weak adapter link prevents delayed TextKit work from keeping that text alive.
    pub(super) document: WeakMarkdownDocument,
    pub(super) string: Retained<PieceBackedMarkdownString>,
    pub(super) base_attributes: Retained<MarkdownTextAttributes>,
    pub(super) palette: MarkdownTextAttributePalette,
    pub(super) font_selection_cache: RefCell<MarkdownFontSelectionCache>,
    pub(super) attribute_cache: RefCell<MarkdownTextAttributeCache>,
    pub(super) font_run_scratch: RefCell<Vec<unichar>>,
    // A single bounded semantic cache parses source once for both visible source syntax and inline preview.
    pub(super) presentation: RefCell<MarkdownViewportPresentation>,
    pub(super) presentation_mode: Cell<MarkdownPresentationMode>,
    pub(super) pending_textkit_selection: Cell<Option<NSRange>>,
    pub(super) composition_active: Cell<bool>,
    // Close preflight seals canonical mutation before native window callbacks can deliver stale input.
    pub(super) accepts_edits: Cell<bool>,
    // Derived attribute edits must wait until the outer NSTextView input transaction ends.
    pub(super) native_input_depth: Cell<usize>,
    pub(super) deferred_presentation_invalidation: RefCell<Option<std::ops::Range<usize>>>,
}

define_class!(
    // SAFETY: this NSTextStorage subclass implements all four required mutable attributed
    // string primitives and reports edits through edited:range:changeInLength:.
    // All state and callbacks are confined to the AppKit main thread.
    #[unsafe(super = NSTextStorage)]
    #[thread_kind = MainThreadOnly]
    #[ivars = PieceBackedTextStorageIvars]
    pub(super) struct PieceBackedMarkdownTextStorage;

    impl PieceBackedMarkdownTextStorage {
        #[unsafe(method(fixesAttributesLazily))]
        /// Declares derived attributes lazy so Foundation consumes bounded queries instead of an eager mirror.
        fn fixes_attributes_lazily(&self) -> bool {
            // TextKit must query the bounded viewport provider on demand. If the
            // superclass eagerly normalizes attributes after every edit, it can
            // re-enter `attributesAtIndex:effectiveRange:` while our piece tree
            // transaction is still unwinding and repeatedly fix the same range.
            // Declaring the storage lazy is the documented NSTextStorage contract
            // for virtual/derived attributes and keeps input and undo finite.
            true
        }

        #[unsafe(method_id(string))]
        /// Returns the same virtual NSString adapter, never a flattened document copy.
        fn string(&self) -> Retained<NSString> {
            // Retaining the adapter does not extend the weak canonical document lifetime.
            self.ivars().string.clone().into_super()
        }

        #[unsafe(method_id(attributesAtIndex:effectiveRange:))]
        /// Supplies one bounded semantic run to TextKit without persisting mutable attributes.
        ///
        /// # Safety
        /// A non-null effective_range must be writable for one NSRange during this call.
        unsafe fn attributes_at_index_effective_range(
            &self,
            index: NSUInteger,
            effective_range: *mut NSRange,
        ) -> Retained<MarkdownTextAttributes> {
            let (attributes, range) = if self.ivars().composition_active.get() {
                // Marked text uses stable base attributes until its synchronous IME transaction commits.
                let range = self.composition_base_attribute_range(index);
                (self.ivars().base_attributes.clone(), range)
            } else {
                // Outside composition, derive attributes from the bounded current-generation viewport.
                self.attributes_at_index(index)
            };
            if !effective_range.is_null() {
                // Foundation requested the effective range; a null optional output requires no write.
                // SAFETY: Foundation supplies either null or a writable NSRange pointer for this call.
                // The computed effective range is bounded by the current canonical document.
                unsafe { effective_range.write(range) };
            }
            attributes
        }

        #[unsafe(method(replaceCharactersInRange:withString:))]
        /// Mutates the piece tree before publishing one synchronous TextKit character transaction.
        fn replace_characters_in_range(&self, range: NSRange, replacement: &NSString) {
            if !self.ivars().accepts_edits.get() {
                // After successful save/close preflight, delayed native input may not create an unsaved revision.
                return;
            }
            // Reject arithmetic overflow before changing the canonical text or semantic selection.
            let Some(range_end) = range.location.checked_add(range.length) else {
                // Foundation cannot receive a Rust Result here; invalid ranges preserve the current document.
                return;
            };
            // Native selection coordinates count UTF-16 units, independently of the incoming UTF-8 byte length.
            let replacement_utf16_length = replacement.len_utf16();
            let Some(selection_offset) = range.location.checked_add(replacement_utf16_length) else {
                // An unrepresentable caret would violate canonical selection bounds; reject the whole edit.
                return;
            };
            // TextKit reports signed UTF-16 deltas, independently of the native unsigned range width.
            let Ok(delta) = isize::try_from(replacement_utf16_length)
                .and_then(|inserted| isize::try_from(range.length).map(|removed| inserted - removed))
            else {
                // A delta outside isize cannot be reported faithfully; leave text and history unchanged.
                return;
            };

            let Some(document) = self.document() else {
                // Delayed native edits after teardown cannot recreate a canonical document.
                return;
            };
            // Group storage notifications, but release the Rust mutable borrow before reentrant native processing.
            self.beginEditing();
            let edit_result = autoreleasepool(|pool| {
                // The temporary replacement view is borrowed only for this complete canonical transaction.
                // SAFETY: the borrowed UTF-8 view is fully consumed before this inner autorelease pool drains.
                let replacement = unsafe { replacement.to_str(pool) };
                let mut document = document.borrow_mut();
                document.replace_utf16_range_with_selection(
                    range.location..range_end,
                    replacement,
                    MarkdownTextSelection {
                        anchor_utf16: range.location,
                        head_utf16: range_end,
                    },
                    MarkdownTextSelection::caret(selection_offset),
                )
            });
            if edit_result.is_ok() {
                // Publish text/selection changes only after the canonical scalar-aligned edit committed.
                self.ivars()
                    .pending_textkit_selection
                    .set(Some(NSRange::new(selection_offset, 0)));
                self.edited_range_changeInLength(
                    NSTextStorageEditActions::EditedCharacters,
                    range,
                    delta,
                );
                self.invalidate_presentation_attributes();
            }
            // Failed edits publish no change; both success and rejection close the native editing group.
            self.endEditing();
            if edit_result.is_ok() {
                // Observe dirtiness only after native observers finish this primitive's edit group.
                queue_native_markdown_autosave_for_storage(self);
            }
        }

        #[unsafe(method(setAttributes:range:))]
        /// Ignores mutable native attribute writes because viewport semantics own presentation.
        ///
        /// # Safety
        /// The optional dictionary must contain native attribute keys/values valid for this call.
        unsafe fn set_attributes_range(
            &self,
            _attributes: Option<&MarkdownTextAttributes>,
            _range: NSRange,
        ) {
            // Presentation attributes come from bounded viewport semantics. Typing attributes
            // must neither become a second mutable model nor change canonical Markdown bytes.
        }

        #[unsafe(method(fixAttributesInRange:))]
        /// Completes lazy Foundation bookkeeping without scanning/parsing or storing mutable attributes.
        fn fix_attributes_in_range(&self, _range: NSRange) {
            // The presentation layer never stores invalid attributes in the
            // text storage. Attribute runs are computed from the current
            // piece-tree generation when queried, so Foundation's generic
            // paragraph/attachment fixer has no mutable backing attributes to
            // repair. Keeping this override finite is what makes the documented
            // lazy NSTextStorage contract suitable for a virtual string.
        }
    }
);

impl PieceBackedMarkdownTextStorage {
    /// Creates session-local derived caches and virtual native primitives over one weak canonical owner.
    pub(super) fn new(document: SharedMarkdownDocument, mtm: MainThreadMarker) -> Retained<Self> {
        // Fixed palette/base attributes are native resources acquired only for an explicitly active editor.
        let string = PieceBackedMarkdownString::new(&document, mtm);
        let font = NSFont::systemFontOfSize(15.0);
        let palette = MarkdownTextAttributePalette::new(font);
        let font_object: &AnyObject = &palette.normal_font;
        let foreground_color_object: &AnyObject = &palette.foreground;
        // SAFETY: AppKit exports these immutable keys for the process lifetime. The dictionary
        // retains its font and color values beyond the temporary stack references.
        let base_attributes = unsafe {
            MarkdownTextAttributes::from_slices(
                &[NSFontAttributeName, NSForegroundColorAttributeName],
                &[font_object, foreground_color_object],
            )
        };
        // Install all Rust ivars before superclass initialization can invoke virtual primitives.
        let storage = Self::alloc(mtm).set_ivars(PieceBackedTextStorageIvars {
            document: Rc::downgrade(&document),
            string,
            base_attributes,
            palette,
            font_selection_cache: RefCell::new(MarkdownFontSelectionCache::default()),
            attribute_cache: RefCell::new(MarkdownTextAttributeCache::default()),
            font_run_scratch: RefCell::new(Vec::with_capacity(
                NATIVE_MARKDOWN_ATTRIBUTE_WINDOW_UTF16_UNITS,
            )),
            presentation: RefCell::new(MarkdownViewportPresentation::default()),
            presentation_mode: Cell::new(MarkdownPresentationMode::InlinePreview),
            pending_textkit_selection: Cell::new(None),
            composition_active: Cell::new(false),
            accepts_edits: Cell::new(true),
            native_input_depth: Cell::new(0),
            deferred_presentation_invalidation: RefCell::new(None),
        });
        // SAFETY: all required NSTextStorage primitives and Rust ivars are initialized before
        // the superclass initializer can dispatch to the subclass.
        unsafe { msg_send![super(storage), init] }
    }

    /// Temporarily acquires the canonical document only while its foreground model still owns it.
    pub(super) fn document(&self) -> Option<SharedMarkdownDocument> {
        // A weak upgrade cannot recreate a document after the model's teardown boundary.
        self.ivars().document.upgrade()
    }
    /// Detaches any compatibility TextKit 1 graph so it cannot keep this storage alive.
    pub(super) fn detach_legacy_layout_managers(&self) -> usize {
        let managers = self.as_super().layoutManagers();
        let mut detached = 0usize;
        // Reverse removal keeps remaining native indices valid while the array shrinks.
        for index in (0..managers.count()).rev() {
            let manager = managers.objectAtIndex(index);
            manager.setDelegate(None);
            let containers = manager.textContainers();
            // A compatibility manager must stop owning containers before its storage link is detached.
            for container_index in (0..containers.count()).rev() {
                manager.removeTextContainerAtIndex(container_index);
            }
            // The owning storage operation clears the manager's reverse link itself. Directly
            // assigning NSLayoutManager.textStorage would bypass the documented owner-side contract.
            self.as_super().removeLayoutManager(&manager);
            detached = detached.saturating_add(1);
        }
        detached
    }

    /// Replace explicit imported/generated text with a fallible canonical transaction.
    ///
    /// Native insertText: has no error result. File commands need a positive commit result before
    /// retaining a copied attachment, so they use this same character publisher after core validation.
    pub(super) fn apply_explicit_replacement(
        &self,
        range: NSRange,
        replacement: &str,
    ) -> Result<MarkdownHistoryTextEdit, String> {
        if !self.ivars().accepts_edits.get() {
            // A delayed import cannot publish text after the accepted save/close boundary.
            return Err("The Markdown editor is closing".into());
        }
        // Validate native arithmetic before beginning an editing group or acquiring mutable text.
        let range_end = range
            .location
            .checked_add(range.length)
            .ok_or("The insertion range overflows UTF-16 coordinates")?;
        let inserted = replacement.encode_utf16().count();
        let caret = range
            .location
            .checked_add(inserted)
            .ok_or("The insertion caret overflows UTF-16 coordinates")?;
        let inserted_delta = isize::try_from(inserted)
            .map_err(|_| "The insertion is too large for a native character notification")?;
        let removed_delta = isize::try_from(range.length)
            .map_err(|_| "The replaced range is too large for a native character notification")?;
        let delta = inserted_delta
            .checked_sub(removed_delta)
            .ok_or("The insertion delta overflows native character coordinates")?;
        let document = self.document().ok_or("The editor is closed")?;
        let edit = MarkdownHistoryTextEdit {
            replaced_utf16_range: range.location..range_end,
            change_in_utf16_length: delta,
            selection_after: MarkdownTextSelection::caret(caret),
        };
        self.beginEditing();
        let result = document.borrow_mut().replace_utf16_range_with_selection(
            edit.replaced_utf16_range.clone(),
            replacement,
            MarkdownTextSelection {
                anchor_utf16: range.location,
                head_utf16: range_end,
            },
            edit.selection_after,
        );
        match result {
            // Native observers receive exactly one change after the core committed all capacity/history checks.
            Ok(()) => {
                self.finish_canonical_edit(Some(&edit));
                Ok(edit)
            }
            // Rejection balances the group and returns ownership to the pending attachment's rollback guard.
            Err(error) => {
                self.finish_canonical_edit(None);
                Err(error.to_string())
            }
        }
    }

    /// Apply one validated format command and publish its exact native edit, with balanced failure handling.
    pub(super) fn apply_format(
        &self,
        action: MarkdownFormatAction,
        selection: MarkdownTextSelection,
    ) -> Result<MarkdownHistoryTextEdit, String> {
        if !self.ivars().accepts_edits.get() {
            // A closing storage has no authority to create a format/keyboard revision after its last save.
            return Err("The Markdown editor is closing".into());
        }
        let document = self.document().ok_or("The editor is closed")?;
        self.beginEditing();
        let result = document.borrow_mut().apply_format(action, selection);
        match result {
            // Atomic preparation supplies one before-document interval and final canonical selection.
            Ok(edit) => {
                self.finish_canonical_edit(Some(&edit));
                Ok(edit)
            }
            // A rejected format preserves history/generation/source and still closes its native edit group.
            Err(error) => {
                self.finish_canonical_edit(None);
                Err(error.to_string())
            }
        }
    }

    /// Apply a canonical keyboard command while preserving the distinction between fallback and consumed no-op.
    pub(super) fn apply_editing_command(
        &self,
        command: MarkdownEditingCommand,
        selection: MarkdownTextSelection,
    ) -> Result<MarkdownEditingOutcome, String> {
        if !self.ivars().accepts_edits.get() {
            // A closing storage has no authority to create a format/keyboard revision after its last save.
            return Err("The Markdown editor is closing".into());
        }
        let document = self.document().ok_or("The editor is closed")?;
        self.beginEditing();
        let result = document
            .borrow_mut()
            .apply_editing_command(command, selection);
        match result {
            // Only a committed changed result publishes source/selection notifications and autosave dirtiness.
            Ok(MarkdownEditingOutcome::Changed(edit)) => {
                self.finish_canonical_edit(Some(&edit));
                Ok(MarkdownEditingOutcome::Changed(edit))
            }
            // Fallback and consumed no-op both close the group without fabricating text/history changes.
            Ok(outcome) => {
                self.finish_canonical_edit(None);
                Ok(outcome)
            }
            // Capacity, scalar or context errors veto the native command without a partial edit.
            Err(error) => {
                self.finish_canonical_edit(None);
                Err(error.to_string())
            }
        }
    }

    /// Restore one structural history root and forward its exact receipt through the same native publisher.
    pub(super) fn restore_history(
        &self,
        direction: MarkdownHistoryDirection,
    ) -> Option<MarkdownHistoryTextEdit> {
        if !self.ivars().accepts_edits.get() {
            // History is immutable after the accepted close gate, even while AppKit retains its shell.
            return None;
        }
        let document = self.document()?;
        self.beginEditing();
        let edit = {
            // End the canonical mutable borrow before native observers can request strings/attributes again.
            let mut document = document.borrow_mut();
            match direction {
                // Undo restores the previous shared root and its original semantic selection.
                MarkdownHistoryDirection::Undo => document.undo_text_edit(),
                // Redo restores the next root unless a new accepted edit invalidated that branch.
                MarkdownHistoryDirection::Redo => document.redo_text_edit(),
            }
        };
        self.finish_canonical_edit(edit.as_ref());
        edit
    }

    /// Close one beginEditing group, publishing only a completed canonical receipt before native reentry.
    fn finish_canonical_edit(&self, edit: Option<&MarkdownHistoryTextEdit>) {
        if let Some(edit) = edit {
            // This marker prevents transient TextKit reconciliation ranges replacing the receipt's selection.
            let start = edit
                .selection_after
                .anchor_utf16
                .min(edit.selection_after.head_utf16);
            let end = edit
                .selection_after
                .anchor_utf16
                .max(edit.selection_after.head_utf16);
            self.ivars()
                .pending_textkit_selection
                .set(Some(NSRange::new(start, end - start)));
            self.edited_range_changeInLength(
                NSTextStorageEditActions::EditedCharacters,
                NSRange::new(
                    edit.replaced_utf16_range.start,
                    edit.replaced_utf16_range.len(),
                ),
                edit.change_in_utf16_length,
            );
            self.invalidate_presentation_attributes();
        }
        // Every success, no-op, fallback and rejection balances its native editing group exactly once.
        self.endEditing();
        self.ivars().pending_textkit_selection.take();
        if edit.is_some() {
            // Reset one weak foreground save deadline only after actual canonical mutation and native observation.
            queue_native_markdown_autosave_for_storage(self);
        }
    }

    /// Resolves a scalar-aligned bounded semantic/font run for one canonical index.
    pub(super) fn attributes_at_index(
        &self,
        index: usize,
    ) -> (Retained<MarkdownTextAttributes>, NSRange) {
        // Lazy NSTextStorage must consume its pending attribute-fix bookkeeping.
        // Our finite fixAttributesInRange: override derives no mutable attribute mirror.
        let current_length = self
            .document()
            .map_or(0, |document| document.borrow().utf16_len());
        if index < current_length {
            // In-document accesses consume the finite lazy-fix contract before deriving a run.
            self.ensureAttributesAreFixedInRange(NSRange::new(index, 1));
        }
        let Some(document) = self.document() else {
            // A disconnected adapter has no text range and must not retain a replacement document.
            return (self.ivars().base_attributes.clone(), NSRange::new(0, 0));
        };
        let document_ref = document.borrow();
        let document_length = document_ref.utf16_len();
        if document_length == 0 || index >= document_length {
            // Empty/EOF requests expose an empty base run at current length, without materialization.
            return (
                self.ivars().base_attributes.clone(),
                NSRange::new(document_length, 0),
            );
        }

        // Bound the request before parsing so giant lines and surrogate queries cannot expand work arbitrarily.
        let (requested_window, scalar_index) =
            markdown_attribute_request_window(&document_ref, index);
        let generation = document_ref.generation();
        drop(document_ref);

        let source_mode = self.presentation_mode() == MarkdownPresentationMode::Source;
        let mut presentation = self.ivars().presentation.borrow_mut();
        if presentation.needs_rebuild(generation, scalar_index) {
            // Both presentation modes derive syntax from one bounded window; source attributes retain all markers.
            let document_ref = document.borrow();
            match document_ref.materialize_markdown_viewport(
                requested_window.clone(),
                MARKDOWN_PRESENTATION_OVERSCAN_UTF16_UNITS,
            ) {
                // A successfully copied window still distinguishes complete syntax from bounded source fallback.
                Ok(slice) => match slice.semantic_status {
                    MarkdownViewportSemanticStatus::Complete => {
                        // Complete bounded semantics can populate the viewport cache safely.
                        let _ = presentation.rebuild(generation, slice.utf16_range, &slice.text);
                    }
                    MarkdownViewportSemanticStatus::SourceFallback => {
                        // Incomplete bounded syntax stays readable as source without a whole-document fallback.
                        let _ = presentation
                            .record_source_fallback_window(generation, slice.utf16_range);
                    }
                },
                Err(_) => {
                    // Materialization failure leaves the cache invalid and falls back to scalar-sized
                    // base attributes. Never pass the full document or an arbitrary unbounded slice to the parser.
                }
            }
        }

        // Missing semantics use base styles within the same bounded request; no parser retry expands its scope.
        let query = presentation.query(scalar_index);
        let (styles, range) = query.map_or_else(
            || {
                (
                    MarkdownPresentationStyles::default(),
                    requested_window.clone(),
                )
            },
            |query| (query.styles, query.utf16_range.start..query.utf16_range.end),
        );
        drop(presentation);

        // Clamp semantic runs before Unicode fallback subdivision so every returned interval stays finite.
        let style_range = clamp_markdown_attribute_range(
            range,
            requested_window.clone(),
            scalar_index,
            document_length,
        );
        let style_range_end = style_range
            .location
            .checked_add(style_range.length)
            .unwrap_or(style_range.location)
            .min(document_length);
        // Copy only this bounded style run to discover adjacent fallback-font coverage.
        let mut scratch = self.ivars().font_run_scratch.borrow_mut();
        scratch.resize(style_range_end.saturating_sub(style_range.location), 0);
        {
            let document_ref = document.borrow();
            copy_piece_backed_utf16_range(&document_ref, style_range.location, &mut scratch);
        }
        let local_index = scalar_index
            .saturating_sub(style_range.location)
            .min(scratch.len().saturating_sub(1));
        let scalar = native_markdown_utf16_scalar_at(&scratch, local_index)
            .map(|scalar| scalar.value)
            .unwrap_or(char::REPLACEMENT_CHARACTER);
        let resolved_font = self.ivars().font_selection_cache.borrow_mut().resolve(
            styles,
            scalar,
            &self.ivars().palette,
            source_mode,
        );
        let font_range = native_markdown_font_run_range(
            &scratch,
            style_range.location,
            local_index,
            &resolved_font.base_coverage,
            resolved_font.fallback_coverage.as_deref(),
        );
        drop(scratch);
        let range = NSRange::new(
            font_range.start,
            font_range.end.saturating_sub(font_range.start),
        );
        // Share the complete immutable style/face dictionary from the editor's bounded LRU.
        let attributes = self.ivars().attribute_cache.borrow_mut().get_or_insert(
            styles,
            &resolved_font.font,
            &self.ivars().palette,
            &self.ivars().base_attributes,
            source_mode,
        );
        (attributes, range)
    }

    /// Invalidates only derived viewport semantics after a canonical edit and reports their affected interval.
    pub(super) fn invalidate_presentation_attributes(&self) {
        let invalidated = self
            .ivars()
            .presentation
            .borrow_mut()
            .invalidate_document_generation();
        self.notify_or_defer_attribute_range(invalidated);
    }

    /// Synchronizes a settled native selection with canonical history and selection-dependent marker visibility.
    pub(super) fn update_active_selection(&self, selected_range: NSRange) {
        if !self.ivars().accepts_edits.get() {
            // A closed native shell cannot alter canonical selection/history metadata while awaiting deallocation.
            return;
        }
        // A pending value marks the synchronous NSTextStorage transaction. All
        // selection notifications in that interval are TextKit's transient
        // reconciliation ranges; keep the Rust memento selection authoritative
        // and release the marker only after `endEditing` returns.
        if self.ivars().pending_textkit_selection.get().is_some() {
            // Transient reconciliation selections cannot overwrite the command/history receipt selection.
            return;
        }
        let Some(end) = selected_range.location.checked_add(selected_range.length) else {
            // An overflowing native range cannot identify an authoritative canonical selection.
            return;
        };
        let selection_start = selected_range.location.min(end);
        let selection_end = selected_range.location.max(end);
        let selection = MarkdownTextSelection {
            anchor_utf16: selection_start,
            head_utf16: selection_end,
        };
        let Some(document) = self.document() else {
            return;
        };
        if document.borrow_mut().set_selection(selection).is_err() {
            // Reject out-of-range or split-surrogate native selections without invalidating presentation.
            return;
        }
        let invalidated = self
            .ivars()
            .presentation
            .borrow_mut()
            .set_active_selection(selection_start..selection_end);
        self.notify_or_defer_attribute_range(invalidated);
    }

    /// Changes derived source/preview presentation without changing canonical Markdown bytes.
    pub(super) fn set_presentation_mode(&self, mode: MarkdownPresentationMode) {
        if self.ivars().presentation_mode.replace(mode) == mode {
            // Reselecting the same native mode preserves the current bounded syntax cache.
            return;
        }
        // The semantic parser stays in its complete syntax mode; only the native projection changes.
        let invalidated = self
            .ivars()
            .presentation
            .borrow_mut()
            .invalidate_document_generation();
        self.notify_or_defer_attribute_range(invalidated);
    }

    /// Reads the current derived presentation mode without allocating native text.
    pub(super) fn presentation_mode(&self) -> MarkdownPresentationMode {
        self.ivars().presentation_mode.get()
    }
    /// Prepares Markdown semantics for one explicit bounded viewport.
    ///
    /// Table layout runs in a main-queue task after drawing. It must not indirectly invoke
    /// TextKit attribute fixing through attributes_at_index and reenter drawRect: during
    /// layout. This method materializes only its UTF-16 window into the shared cache.
    pub(super) fn table_presentation_snapshot(
        &self,
        requested_utf16_window: std::ops::Range<usize>,
    ) -> Option<NativeMarkdownTablePresentationSnapshot> {
        if self.presentation_mode() != MarkdownPresentationMode::InlinePreview
            || self.ivars().composition_active.get()
        {
            // Source mode and provisional IME input expose source text rather than table overlays.
            return None;
        }
        let document = self.document()?;
        let (generation, document_length, requested_utf16_window) = {
            let document_ref = document.borrow();
            let document_length = document_ref.utf16_len();
            if document_length == 0 {
                // Empty canonical text cannot contribute a semantic table or a native layout range.
                return None;
            }
            let start = requested_utf16_window.start.min(document_length);
            let end = requested_utf16_window.end.min(document_length).max(start);
            let start = markdown_utf16_boundary_down(&document_ref, start);
            let end = markdown_utf16_boundary_up(&document_ref, end).max(start);
            (
                document_ref.generation(),
                document_length,
                start..end.min(document_length),
            )
        };
        if document_length == 0 {
            // No canonical code unit remains to anchor a presentation query.
            return None;
        }

        let needs_rebuild = {
            let presentation = self.ivars().presentation.borrow();
            let probe = requested_utf16_window
                .start
                .min(document_length.saturating_sub(1));
            presentation.needs_rebuild(generation, probe)
                || (requested_utf16_window.end > requested_utf16_window.start
                    && presentation
                        .needs_rebuild(generation, requested_utf16_window.end.saturating_sub(1)))
        };
        if needs_rebuild {
            // Rebuild only when generation or either requested endpoint escapes the current bounded window.
            let rebuild_result = {
                let document_ref = document.borrow();
                document_ref.materialize_markdown_viewport(
                    requested_utf16_window.clone(),
                    MARKDOWN_PRESENTATION_OVERSCAN_UTF16_UNITS,
                )
            };
            let mut presentation = self.ivars().presentation.borrow_mut();
            match rebuild_result {
                // A successfully copied window still distinguishes complete syntax from bounded source fallback.
                Ok(slice) => match slice.semantic_status {
                    MarkdownViewportSemanticStatus::Complete => {
                        // Complete bounded semantics can populate the viewport cache safely.
                        let _ = presentation.rebuild(generation, slice.utf16_range, &slice.text);
                    }
                    MarkdownViewportSemanticStatus::SourceFallback => {
                        // Incomplete bounded syntax stays readable as source without a whole-document fallback.
                        let _ = presentation
                            .record_source_fallback_window(generation, slice.utf16_range);
                    }
                },
                Err(_) => {
                    // Cache a stable bounded source-mode fallback after materialization failure. Repeated
                    // draws must neither reschedule the same failed request nor parse the full document.
                    let _ = presentation
                        .record_source_fallback_window(generation, requested_utf16_window.clone());
                }
            }
        }

        let presentation = self.ivars().presentation.borrow();
        let utf16_window = presentation.cached_utf16_window()?;
        Some(NativeMarkdownTablePresentationSnapshot {
            document_generation: generation,
            utf16_window,
            tables: presentation.tables().to_vec(),
        })
    }

    /// Builds a bounded table snapshot around one canonical UTF-16 probe location.
    pub(super) fn table_presentation_snapshot_at_index(
        &self,
        probe_utf16_index: usize,
    ) -> Option<NativeMarkdownTablePresentationSnapshot> {
        let requested_window = {
            let document = self.document()?;
            let document_ref = document.borrow();
            markdown_attribute_request_window(&document_ref, probe_utf16_index).0
        };
        self.table_presentation_snapshot(requested_window)
    }

    /// Copies one bounded exact cell range for explicit single-cell editing.
    pub(super) fn table_cell_source(&self, range: &std::ops::Range<usize>) -> Option<String> {
        let document = self.document()?;
        let document_ref = document.borrow();
        let slice = document_ref.materialize_viewport(range.clone(), 0).ok()?;
        (slice.utf16_range == *range).then_some(slice.text)
    }

    /// Builds immutable visible-cell display attributes from the bounded semantic source projection.
    pub(super) fn table_cell_attributed_text(
        &self,
        range: &std::ops::Range<usize>,
    ) -> Option<Retained<NSAttributedString>> {
        let display = self.table_cell_display_text(range)?;
        let string = NSString::from_str(&display.text);
        // SAFETY: this module constructs fixed key/value types. Mutable attributed cell text
        // lives only in the bounded visible-table cache.
        let attributed = unsafe {
            NSMutableAttributedString::initWithString_attributes(
                NSMutableAttributedString::alloc(),
                &string,
                Some(&self.ivars().base_attributes),
            )
        };
        for span in display.style_spans {
            // Each span refers only to visible cell text, so native attributes never cover hidden markers.
            if span.utf16_range.is_empty() {
                // Concealed/empty style spans have no native string interval to annotate.
                continue;
            }
            let font = native_markdown_base_font(span.styles, &self.ivars().palette);
            // Share the complete immutable style/face dictionary from the editor's bounded LRU.
            let attributes = self.ivars().attribute_cache.borrow_mut().get_or_insert(
                span.styles,
                &font,
                &self.ivars().palette,
                &self.ivars().base_attributes,
                false,
            );
            // SAFETY: each span derives from UTF-16 appends to this same string and stays within
            // the mutable attributed string's current length.
            unsafe {
                attributed.addAttributes_range(
                    &attributes,
                    NSRange::new(
                        span.utf16_range.start,
                        span.utf16_range.end - span.utf16_range.start,
                    ),
                );
            }
        }
        Some(attributed.into_super())
    }

    /// Resolves visible cell text and style spans while keeping original Markdown source authoritative.
    pub(super) fn table_cell_display_text(
        &self,
        range: &std::ops::Range<usize>,
    ) -> Option<NativeMarkdownTableCellDisplayText> {
        let source = self.table_cell_source(range)?;
        let presentation = self.ivars().presentation.borrow();
        Some(native_markdown_table_cell_display_text_from_source(
            &source,
            range.start,
            |absolute_utf16| {
                // Resolve each scalar from the same bounded source cache used to construct this cell.
                presentation
                    .query(absolute_utf16)
                    .map(|query| query.styles)
                    .unwrap_or_default()
            },
        ))
    }

    /// Commits one cell through the canonical storage primitive and reports its signed UTF-16 size change.
    pub(super) fn replace_table_cell(
        &self,
        range: std::ops::Range<usize>,
        replacement: &str,
    ) -> Option<isize> {
        let document = self.document()?;
        let before_generation = document.borrow().generation();
        let replacement = NSString::from_str(replacement);
        self.replaceCharactersInRange_withString(
            NSRange::new(range.start, range.end.saturating_sub(range.start)),
            &replacement,
        );
        self.ivars().pending_textkit_selection.take();
        let document_ref = document.borrow();
        if document_ref.generation() == before_generation {
            // A primitive rejection leaves the visible cell draft uncommitted for its caller to preserve.
            return None;
        }
        let inserted = isize::try_from(replacement.len_utf16()).ok()?;
        let removed = isize::try_from(range.end.saturating_sub(range.start)).ok()?;
        Some(inserted - removed)
    }

    /// Defers semantic style notifications while the native input method owns provisional text.
    pub(super) fn begin_composition(&self) {
        self.ivars().composition_active.set(true);
    }

    /// Ends provisional composition and publishes or defers its coalesced style invalidation.
    pub(super) fn finish_composition(&self) {
        if !self.ivars().composition_active.replace(false) {
            // Repeated unmark/close callbacks cannot publish the same deferred style invalidation twice.
            return;
        }
        let invalidated = self
            .ivars()
            .deferred_presentation_invalidation
            .borrow_mut()
            .take();
        self.notify_or_defer_attribute_range(invalidated);
    }

    /// Starts a balanced native-input scope without retaining a second document or native graph.
    pub(super) fn begin_native_input(&self) {
        self.ivars()
            .native_input_depth
            .set(self.ivars().native_input_depth.get().saturating_add(1));
    }

    /// Publishes coalesced derived attributes only after NSTextView closes its outer edit transaction.
    pub(super) fn end_native_input(&self) {
        let depth = self.ivars().native_input_depth.get().saturating_sub(1);
        self.ivars().native_input_depth.set(depth);
        if depth == 0 && !self.ivars().composition_active.get() {
            // Character edits are already committed; deferred style edits can no longer invalidate
            // element caches while NSTextView's insertion code is scrolling its caret to visible.
            let invalidated = self
                .ivars()
                .deferred_presentation_invalidation
                .borrow_mut()
                .take();
            self.notify_attribute_range(invalidated);
        }
    }

    /// Translates an IME-relative selection into canonical absolute scalar-aligned UTF-16 coordinates.
    pub(super) fn set_composition_selection(
        &self,
        marked_range: NSRange,
        relative_selection: NSRange,
    ) {
        let selection_start = relative_selection.location.min(marked_range.length);
        let selection_end = relative_selection
            .location
            .saturating_add(relative_selection.length)
            .min(marked_range.length)
            .max(selection_start);
        let absolute_start = marked_range.location.saturating_add(selection_start);
        let absolute_end = marked_range.location.saturating_add(selection_end);
        let Some(document) = self.document() else {
            return;
        };
        let _ = document.borrow_mut().set_selection(MarkdownTextSelection {
            anchor_utf16: absolute_start,
            head_utf16: absolute_end,
        });
    }

    /// Supplies stable bounded base attributes for the scalar currently queried during composition.
    pub(super) fn composition_base_attribute_range(&self, index: usize) -> NSRange {
        let Some(document) = self.document() else {
            // Late IME attribute requests see an empty disconnected adapter rather than retained source.
            return NSRange::new(0, 0);
        };
        let document = document.borrow();
        let document_length = document.utf16_len();
        if index >= document_length {
            // EOF has no scalar to style and must not produce a range beyond NSString length.
            return NSRange::new(document_length, 0);
        }
        let start = markdown_utf16_boundary_down(&document, index);
        let end = markdown_utf16_boundary_up(&document, index.saturating_add(1))
            .max(start.saturating_add(1))
            .min(document_length);
        NSRange::new(start, end.saturating_sub(start))
    }

    /// Coalesces style-range metadata until both composition and native input transactions permit notification.
    pub(super) fn notify_or_defer_attribute_range(
        &self,
        invalidated: Option<std::ops::Range<usize>>,
    ) {
        if !self.ivars().composition_active.get() && self.ivars().native_input_depth.get() == 0 {
            // Outside native input and composition, style notifications can be delivered synchronously.
            self.notify_attribute_range(invalidated);
            return;
        }
        let Some(invalidated) = invalidated else {
            // A cache that was never populated contributes no stale style interval to defer.
            return;
        };
        // Merge only range metadata while native editing owns element-cache consistency.
        let mut deferred = self.ivars().deferred_presentation_invalidation.borrow_mut();
        *deferred = Some(match deferred.take() {
            // Multiple edits coalesce to one interval spanning every previously invalidated style.
            Some(previous) => {
                previous.start.min(invalidated.start)..previous.end.max(invalidated.end)
            }
            // The first invalidation needs only its own bounded coordinates.
            None => invalidated,
        });
    }

    /// Clamps stale derived ranges to the current document before notifying native attribute consumers.
    pub(super) fn notify_attribute_range(&self, invalidated: Option<std::ops::Range<usize>>) {
        let Some(range) = invalidated else {
            // An unpopulated presentation cache has no attribute notification to publish.
            return;
        };
        // Presentation caches are deliberately allowed to describe the window
        // that was visible before an edit.  A deletion can therefore shrink the
        // piece tree before this notification is delivered.  NSTextStorage
        // validates attribute ranges against its *current* NSString length;
        // forwarding the stale end would raise NSRangeException (for example
        // old `0..8259` after the new document is `8258` units long).
        let Some(document) = self.document() else {
            return;
        };
        let document_length = document.borrow().utf16_len();
        let start = range.start.min(document_length);
        let end = range.end.min(document_length).max(start);
        let length = end.saturating_sub(start);
        if length == 0 {
            // Deletion may remove the whole stale range; Foundation must not receive a phantom edit.
            return;
        }
        self.edited_range_changeInLength(
            NSTextStorageEditActions::EditedAttributes,
            NSRange::new(start, length),
            0,
        );
    }
}

/// Selects a fixed-size scalar-aligned context window containing the requested native code unit.
pub(super) fn markdown_attribute_request_window(
    document: &MarkdownDocumentBuffer,
    index: usize,
) -> (std::ops::Range<usize>, usize) {
    let document_length = document.utf16_len();
    if document_length == 0 {
        // Empty input has no containing scalar and requires no context allocation.
        return (0..0, 0);
    }

    let probe = index.min(document_length.saturating_sub(1));
    let maximum_window = NATIVE_MARKDOWN_ATTRIBUTE_WINDOW_UTF16_UNITS;

    // Always include the complete scalar containing `probe`. TextKit can ask for
    // the low surrogate of an emoji, while the range returned to Foundation must
    // still start and end on scalar boundaries.
    let probe_start = markdown_utf16_boundary_down(document, probe);
    let probe_end =
        markdown_utf16_boundary_up(document, probe.saturating_add(1).min(document_length))
            .max(probe_start.saturating_add(1))
            .min(document_length);
    let probe_length = probe_end.saturating_sub(probe_start);
    if probe_length >= maximum_window {
        // Preserve the complete queried scalar even if a future policy sets a smaller context budget.
        return (probe_start..probe_end, probe);
    }

    let context_budget = maximum_window - probe_length;
    let left_budget = context_budget / 2;
    let right_budget = context_budget - left_budget;
    let mut scalar_start =
        markdown_utf16_boundary_down(document, probe_start.saturating_sub(left_budget));
    let mut scalar_end = markdown_utf16_boundary_up(
        document,
        probe_end.saturating_add(right_budget).min(document_length),
    )
    .min(document_length);

    // Aligning both sides can expand a requested window by one UTF-16 unit at a
    // surrogate boundary. Trim the excess on a scalar boundary while retaining
    // the probe scalar. This makes the upper bound an invariant rather than a
    // best effort, including for documents made entirely of supplementary
    // characters.
    if scalar_end.saturating_sub(scalar_start) > maximum_window {
        let bounded_start =
            markdown_utf16_boundary_up(document, scalar_end.saturating_sub(maximum_window));
        if bounded_start <= probe_start {
            // Trim the left edge when doing so still contains the original queried scalar.
            scalar_start = bounded_start;
        } else {
            // Otherwise trim the right context, retaining the complete scalar at the left anchor.
            scalar_end =
                markdown_utf16_boundary_down(document, scalar_start.saturating_add(maximum_window))
                    .max(probe_end)
                    .min(document_length);
        }
    }

    // The probe scalar is at most two UTF-16 units, so the bounded branch above
    // cannot be empty. Keep this defensive fallback for malformed future scalar
    // metrics without ever returning an out-of-document range.
    if scalar_start >= scalar_end {
        // Defensive malformed-metric fallback retains only the known complete probe scalar.
        (probe_start..probe_end, probe)
    } else {
        // The usual path provides bounded context on both sides of the queried scalar.
        (scalar_start..scalar_end, probe)
    }
}

/// Moves a UTF-16 offset to the preceding scalar boundary without splitting a surrogate pair.
pub(super) fn markdown_utf16_boundary_down(
    document: &MarkdownDocumentBuffer,
    mut offset: usize,
) -> usize {
    while offset > 0 && document.byte_offset_for_utf16(offset).is_err() {
        // A valid canonical UTF-16 offset can be internal only to one two-unit surrogate pair.
        offset -= 1;
    }
    offset
}

/// Moves a UTF-16 offset to the following scalar boundary, capped at the current document end.
pub(super) fn markdown_utf16_boundary_up(
    document: &MarkdownDocumentBuffer,
    mut offset: usize,
) -> usize {
    let document_length = document.utf16_len();
    while offset < document_length && document.byte_offset_for_utf16(offset).is_err() {
        // Advance at most across the remaining half of the queried supplementary scalar.
        offset += 1;
    }
    offset.min(document_length)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// One decoded scalar and its local half-open UTF-16 interval in a bounded scratch window.
pub(super) struct NativeMarkdownUtf16Scalar {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) value: char,
}

/// Decodes the complete scalar containing a bounded UTF-16 code-unit index.
pub(super) fn native_markdown_utf16_scalar_at(
    code_units: &[unichar],
    index: usize,
) -> Option<NativeMarkdownUtf16Scalar> {
    if code_units.is_empty() || index >= code_units.len() {
        // The caller cannot derive a scalar from an empty or out-of-window native query.
        return None;
    }

    let mut start = index;
    if is_native_markdown_low_surrogate(code_units[start])
        && start > 0
        && is_native_markdown_high_surrogate(code_units[start - 1])
    {
        // Low-surrogate queries refer to the same complete scalar as their preceding high unit.
        start -= 1;
    }
    let first = code_units[start];
    if is_native_markdown_high_surrogate(first)
        && start + 1 < code_units.len()
        && is_native_markdown_low_surrogate(code_units[start + 1])
    {
        // Decode a complete pair as one scalar so fallback ranges cannot split an emoji.
        let high = u32::from(first) - 0xd800;
        let low = u32::from(code_units[start + 1]) - 0xdc00;
        return Some(NativeMarkdownUtf16Scalar {
            start,
            end: start + 2,
            value: char::from_u32(0x1_0000 + (high << 10) + low)
                .unwrap_or(char::REPLACEMENT_CHARACTER),
        });
    }

    // BMP input occupies one unit; isolated malformed surrogates use the replacement character.
    Some(NativeMarkdownUtf16Scalar {
        start,
        end: start + 1,
        value: char::from_u32(u32::from(first)).unwrap_or(char::REPLACEMENT_CHARACTER),
    })
}

/// Identify the leading half of a supplementary Unicode scalar in Foundation UTF-16 storage.
const fn is_native_markdown_high_surrogate(code_unit: unichar) -> bool {
    code_unit >= 0xd800 && code_unit <= 0xdbff
}

/// Identify the trailing half before extending an attribute range across its paired leading unit.
const fn is_native_markdown_low_surrogate(code_unit: unichar) -> bool {
    code_unit >= 0xdc00 && code_unit <= 0xdfff
}

/// Finds adjacent scalar-safe text sharing the resolved fallback-font policy.
pub(super) fn native_markdown_font_run_range(
    code_units: &[unichar],
    global_start: usize,
    query_index: usize,
    base_coverage: &NSCharacterSet,
    fallback_coverage: Option<&NSCharacterSet>,
) -> std::ops::Range<usize> {
    native_markdown_font_run_range_by(code_units, global_start, query_index, |scalar| {
        let base_covers = native_markdown_character_set_covers(base_coverage, scalar);
        fallback_coverage.map_or(base_covers, |fallback| {
            // A fallback run stops where the base face can draw or the selected fallback cannot.
            !base_covers && native_markdown_character_set_covers(fallback, scalar)
        })
    })
}

/// Expands a bounded font run through neighboring scalars accepted by the supplied face predicate.
pub(super) fn native_markdown_font_run_range_by(
    code_units: &[unichar],
    global_start: usize,
    query_index: usize,
    mut is_compatible: impl FnMut(char) -> bool,
) -> std::ops::Range<usize> {
    let Some(query) = native_markdown_utf16_scalar_at(code_units, query_index) else {
        // No scalar means no valid native attribute interval to extend.
        return global_start..global_start;
    };
    let mut run_start = query.start;
    let mut run_end = query.end;

    while run_start > 0 {
        // Extend left through adjacent complete scalars only while the same face policy applies.
        let Some(previous) = native_markdown_utf16_scalar_at(code_units, run_start - 1) else {
            // A missing predecessor ends the bounded run without scanning outside scratch.
            break;
        };
        if previous.end != run_start || !is_compatible(previous.value) {
            // A gap or different font requirement is the exact left boundary of this run.
            break;
        }
        run_start = previous.start;
    }

    while run_end < code_units.len() {
        // Extend right using the same scalar/coverage rule and the fixed scratch endpoint.
        let Some(next) = native_markdown_utf16_scalar_at(code_units, run_end) else {
            // An undecodable successor cannot extend a safe effective range.
            break;
        };
        if next.start != run_end || !is_compatible(next.value) {
            // A nonadjacent or differently covered scalar begins the next native font run.
            break;
        }
        run_end = next.end;
    }

    global_start.saturating_add(run_start)..global_start.saturating_add(run_end)
}

/// Intersects a candidate attribute run with its bounded fallback while preserving the queried index.
pub(super) fn clamp_markdown_attribute_range(
    range: std::ops::Range<usize>,
    fallback_range: std::ops::Range<usize>,
    index: usize,
    document_length: usize,
) -> NSRange {
    let fallback_start = fallback_range.start.min(document_length);
    let fallback_end = fallback_range.end.min(document_length).max(fallback_start);
    let start = range.start.max(fallback_start).min(fallback_end);
    let end = range.end.min(fallback_end).max(start);
    if start < end && start <= index && index < end {
        // The exact semantic/font intersection is valid and still contains the queried unit.
        NSRange::new(start, end - start)
    } else if fallback_start < fallback_end {
        // If the semantic run missed the query, use its already scalar-aligned bounded fallback.
        NSRange::new(fallback_start, fallback_end - fallback_start)
    } else if index < document_length {
        // The caller has already aligned the fallback window to scalar
        // boundaries. This branch is only a defensive zero-length case.
        NSRange::new(index.min(document_length.saturating_sub(1)), 1)
    } else {
        // Native EOF queries expose an empty interval, never a fabricated trailing code unit.
        NSRange::new(document_length, 0)
    }
}

#[cfg(test)]
mod native_markdown_font_run_tests {
    use super::{native_markdown_font_run_range_by, native_markdown_utf16_scalar_at};

    #[test]
    /// Resolve either half of a supplementary scalar to the same complete native UTF-16 character range.
    pub(super) fn utf16_scalar_lookup_keeps_a_supplementary_character_whole() {
        let source: Vec<u16> = "A😀B".encode_utf16().collect();

        let high = native_markdown_utf16_scalar_at(&source, 1).expect("emoji high surrogate");
        let low = native_markdown_utf16_scalar_at(&source, 2).expect("emoji low surrogate");

        assert_eq!(high, low);
        assert_eq!(high.start..high.end, 1..3);
        assert_eq!(high.value, '😀');
    }

    #[test]
    /// Stop fallback runs at adjacent ASCII boundaries while preserving their absolute canonical offsets.
    pub(super) fn fallback_font_run_contains_only_adjacent_non_ascii_scalars() {
        let source: Vec<u16> = "A中文B".encode_utf16().collect();
        let run = native_markdown_font_run_range_by(&source, 40, 1, |scalar| !scalar.is_ascii());

        assert_eq!(run, 41..43);
    }

    #[test]
    /// Expand fallback text across complete emoji scalars even when the requested index is a low surrogate.
    pub(super) fn fallback_font_run_does_not_split_emoji_surrogate_pairs() {
        let source: Vec<u16> = "A😀😃B".encode_utf16().collect();
        let run = native_markdown_font_run_range_by(&source, 7, 2, |scalar| !scalar.is_ascii());

        assert_eq!(run, 8..12);
    }
}
