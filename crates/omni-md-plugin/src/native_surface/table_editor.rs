//! Owns the one reusable native table-cell draft and semantic spreadsheet navigation.
//!
//! Commit failures preserve the visible draft; cancel restores canonical display. Cell identity is
//! resolved again after edits rather than retaining stale byte offsets or an entire table body.

use super::*;

impl PieceBackedMarkdownTextView {
    /// Resolves a mouse point against already-built visible-cell geometry without reparsing source.
    pub(super) fn visible_table_cell_at_point(
        &self,
        point: NSPoint,
    ) -> Option<NativeMarkdownVisibleTableCellLayout> {
        self.ivars()
            .visible_table_layout_cache
            .borrow()
            .as_ref()?
            .tables
            .iter()
            .flat_map(|table| table.cells.iter())
            .find(|cell| NSPointInRect(point, cell.frame))
            .cloned()
    }

    /// Commits any prior cell and starts editing the selected current-generation visible cell.
    pub(super) fn activate_table_cell(&self, cell: NativeMarkdownVisibleTableCellLayout) {
        if self
            .ivars()
            .active_table_cell
            .borrow()
            .as_ref()
            .is_some_and(|active| active.coordinate == cell.coordinate)
        {
            // Clicking the same active cell preserves its uncommitted draft and restores native field focus.
            if let Some(editor) = self.ivars().cell_editor.get()
                && let Some(window) = self.window()
            {
                // Reuse the already owned field instead of reconstructing its draft or source range.
                window.makeFirstResponder(Some(editor));
            }
            return;
        }
        if !self.commit_cell_before_command() {
            // Preserve the pending cell draft rather than advancing this native command.
            return;
        }
        self.request_table_cell_activation(cell.coordinate);
    }

    /// Stores semantic cell identity until layout can resolve its current geometry after an edit.
    pub(super) fn request_table_cell_activation(
        &self,
        coordinate: NativeMarkdownTableCellCoordinate,
    ) -> bool {
        let Some(text_storage) = self.ivars().text_storage.load() else {
            // Late cell activation has no canonical owner after graph teardown.
            return false;
        };
        let Some(snapshot) =
            text_storage.table_presentation_snapshot_at_index(coordinate.table_utf16_start)
        else {
            // Bounded source fallback or provisional composition provides no reliable cell identity.
            return false;
        };
        let Some(table) = snapshot
            .tables
            .iter()
            .find(|table| table.utf16_range.start == coordinate.table_utf16_start)
        else {
            // An intervening edit removed/rebased the requested table; stale coordinates cannot select another one.
            return false;
        };
        let Some(row) = table.rows.get(coordinate.row_index) else {
            // Row deletion invalidates the request rather than clamping it into a different user cell.
            return false;
        };
        let Some(source_utf16_range) = row.cell_index_range.clone().find_map(|cell_index| {
            // Search only this bounded parsed row for the exact semantic column.
            table.cells.get(cell_index).and_then(|cell| {
                // Preserve the canonical scalar range rather than any previously drawn cell label.
                (cell.column_index == coordinate.column_index).then(|| cell.utf16_range.clone())
            })
        }) else {
            // A missing current column leaves the prior editor/selection untouched.
            return false;
        };
        // Record the activation intent before scrolling can synchronously draw. Drawing is
        // read-only; the asynchronous layout transaction activates the prepared cell later.
        self.ivars()
            .pending_table_cell_activation
            .set(Some(coordinate));
        self.scrollRangeToVisible(NSRange::new(
            source_utf16_range.start,
            source_utf16_range
                .end
                .saturating_sub(source_utf16_range.start),
        ));
        self.ivars().visible_table_layout_cache.borrow_mut().take();
        self.schedule_visible_table_layout_update();
        self.setNeedsDisplay(true);
        true
    }

    /// Resolves a pending semantic cell against the current cache without keeping stale source offsets.
    pub(super) fn activate_pending_table_cell(&self) -> bool {
        let Some(coordinate) = self.ivars().pending_table_cell_activation.get() else {
            // Ordinary layout transactions do not allocate or activate a field without explicit user intent.
            return false;
        };
        let cell = self
            .ivars()
            .visible_table_layout_cache
            .borrow()
            .as_ref()
            .and_then(|cache| {
                cache
                    .tables
                    .iter()
                    .find(|table| table.table_utf16_start == coordinate.table_utf16_start)
            })
            .and_then(|table| {
                table
                    .cells
                    .iter()
                    .find(|cell| cell.coordinate == coordinate)
            })
            .cloned();
        let Some(cell) = cell else {
            // Keep the pending semantic intent until a real visible viewport supplies matching geometry.
            return false;
        };
        self.ivars().pending_table_cell_activation.set(None);
        self.begin_table_cell_editing(cell)
    }

    /// Loads one bounded source cell into the reusable native field and records its cancellation state.
    pub(super) fn begin_table_cell_editing(
        &self,
        cell: NativeMarkdownVisibleTableCellLayout,
    ) -> bool {
        let Some(text_storage) = self.ivars().text_storage.load() else {
            // Late cell activation has no canonical owner after graph teardown.
            return false;
        };
        let Some(source) = text_storage.table_cell_source(&cell.source_utf16_range) else {
            // An over-budget or changed cell cannot be replaced by a truncated editable draft.
            return false;
        };
        let editor = self.table_cell_editor();
        // The native field stores only this explicitly edited bounded cell, never a table/document mirror.
        editor.setFrame(NSInsetRect(cell.frame, 1.0, 1.0));
        editor.setAccessibilityLabel(Some(&NSString::from_str(&format!(
            "Markdown table cell, row {}, column {}",
            cell.coordinate.row_index + 1,
            cell.coordinate.column_index + 1
        ))));
        editor.setAccessibilityHelp(Some(ns_string!(
            "Tab and Shift-Tab move between cells; Return moves down; Escape cancels this draft."
        )));
        editor.setStringValue(&NSString::from_str(&source));
        editor.setAlignment(match cell.alignment {
            // Default Markdown columns use ordinary leading text alignment.
            MarkdownTableAlignment::Left => NSTextAlignment::Left,
            // A centered delimiter projects to the native field without changing stored source.
            MarkdownTableAlignment::Center => NSTextAlignment::Center,
            // Right-aligned source columns retain their editing alignment.
            MarkdownTableAlignment::Right => NSTextAlignment::Right,
        });
        editor.setFont(Some(if cell.is_header {
            // Header editing reuses the fixed stronger face rather than attributed document state.
            &text_storage.ivars().palette.table_header_font
        } else {
            // Body editing uses the ordinary foreground source face.
            &text_storage.ivars().palette.normal_font
        }));
        editor.setHidden(false);
        *self.ivars().active_table_cell.borrow_mut() = Some(NativeMarkdownTableCellEditorState {
            coordinate: cell.coordinate,
            source_utf16_range: cell.source_utf16_range,
            frame: cell.frame,
            original_source: source,
        });
        // SAFETY: the editor is a child of this live view, and its delegate is retained on the same main thread.
        unsafe { editor.selectText(None) };
        self.setNeedsDisplayInRect(cell.frame);
        true
    }

    /// Lazily acquires the session-owned native cell field instead of allocating a control for every table cell.
    pub(super) fn table_cell_editor(&self) -> Retained<NSTextField> {
        self.ivars()
            .cell_editor
            .get_or_init(|| {
                // The first explicit cell interaction alone acquires this reusable native field.
                let mtm = MainThreadMarker::new()
                    .expect("Markdown table cell editor must be created on the AppKit main thread");
                let editor = NSTextField::initWithFrame(
                    NSTextField::alloc(mtm),
                    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)),
                );
                editor.setEditable(true);
                editor.setSelectable(true);
                editor.setBezeled(false);
                editor.setDrawsBackground(true);
                editor.setBackgroundColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
                    0.13, 0.20, 0.31, 1.0,
                )));
                editor.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
                    0.91, 0.93, 0.97, 1.0,
                )));
                editor.setFont(Some(&NSFont::systemFontOfSize(14.0)));
                editor.setFocusRingType(NSFocusRingType::None);
                editor.setHidden(true);
                // SAFETY: NSTextField borrows its delegate. The text view owns the editor subview,
                // which does not strongly retain its delegate in return.
                unsafe {
                    editor.setDelegate(Some(ProtocolObject::from_ref(self)));
                }
                self.addSubview(&editor);
                editor
            })
            .clone()
    }

    /// Commits one cell only after canonical acceptance; failure preserves the visible draft and focus.
    pub(super) fn commit_active_table_cell(&self) -> Result<bool, String> {
        // Clone finite source/range metadata before calling native field methods or storage observers.
        let Some(state) = self.ivars().active_table_cell.borrow().clone() else {
            // No active field contributes a pending edit to this command.
            return Ok(false);
        };
        let editor = self
            .ivars()
            .cell_editor
            .get()
            .ok_or("The active table editor is unavailable")?;
        if let Some(field_editor) = editor
            .currentEditor()
            .and_then(|native| native.downcast::<NSTextView>().ok())
        {
            // The system field editor owns physical typing/IME while editing; the control's
            // stored cell value is not authoritative until native composition and validation finish.
            let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*field_editor);
            if input.hasMarkedText() {
                // End the actual field composition without invoking a system-only input-context hook.
                input.unmarkText();
            }
            if input.hasMarkedText() {
                // Refusal preserves the field, focus and provisional draft rather than saving stale cell text.
                return Err(
                    "The table cell composition could not be committed; the draft remains open"
                        .into(),
                );
            }
        }
        // Copy the current field editor's accepted value into its control before reading/sanitizing it.
        editor.validateEditing();
        let replacement = autoreleasepool(|pool| {
            let value = editor.stringValue();
            // SAFETY: value stays retained and its pool-scoped UTF-8 is copied before the pool drains.
            native_markdown_sanitize_table_cell_input(unsafe { value.to_str(pool) })
        });
        let changed = replacement != state.original_source;
        if changed {
            // Capacity, scalar and lifetime failures must veto the command before hiding the user's draft.
            let storage = self
                .ivars()
                .text_storage
                .load()
                .ok_or("The table document is no longer available")?;
            if storage
                .replace_table_cell(state.source_utf16_range, &replacement)
                .is_none()
            {
                // The canonical source remains unchanged; the reusable field still owns the visible input.
                return Err("The table edit could not be committed; the draft remains open".into());
            }
        }
        // Publish no-active-draft before abortEditing can reenter field-editor delegate callbacks.
        self.ivars().active_table_cell.borrow_mut().take();
        editor.abortEditing();
        editor.setHidden(true);
        editor.setStringValue(ns_string!(""));
        self.setNeedsDisplayInRect(state.frame);
        if changed {
            // Accepted source changes invalidate only the derived viewport geometry.
            self.ivars().visible_table_layout_cache.borrow_mut().take();
            self.setNeedsDisplay(true);
            self.schedule_visible_table_layout_update();
        }
        Ok(changed)
    }

    /// Reports a failed cell commit and prevents a native command from discarding its pending draft.
    pub(super) fn commit_cell_before_command(&self) -> bool {
        match self.commit_active_table_cell() {
            // An unchanged but successfully ended field is also safe for the next command.
            Ok(_) => true,
            // No subsequent activation/navigation/presentation command may replace this visible draft.
            Err(error) => {
                present_native_markdown_error(&error);
                false
            }
        }
    }

    /// Discards the native single-cell draft while preserving the original canonical source.
    pub(super) fn cancel_active_table_cell(&self) {
        self.ivars().pending_table_cell_activation.set(None);
        let state = self.ivars().active_table_cell.borrow_mut().take();
        if let Some(editor) = self.ivars().cell_editor.get() {
            // Clear active state first, then end field-editor ownership without committing cancelled source.
            editor.abortEditing();
            editor.setHidden(true);
            editor.setStringValue(ns_string!(""));
        }
        if let Some(state) = state {
            // Repaint the previously edited cell from its unchanged canonical display projection.
            self.setNeedsDisplayInRect(state.frame);
        }
        if let Some(window) = self.window() {
            // Return keyboard focus to the source editor after ending the reusable field draft.
            window.makeFirstResponder(Some(self));
        }
    }

    /// Commits the current cell and resolves spreadsheet-style movement against newly parsed semantics.
    pub(super) fn navigate_active_table_cell(&self, navigation: NativeMarkdownTableNavigation) {
        let Some(active) = self.ivars().active_table_cell.borrow().clone() else {
            // No reusable field draft exists to navigate, move, or replace.
            return;
        };
        let Some(text_storage) = self.ivars().text_storage.load() else {
            // A disconnected weak storage cannot mutate or navigate a successor document.
            return;
        };
        let Some(snapshot) =
            text_storage.table_presentation_snapshot_at_index(active.coordinate.table_utf16_start)
        else {
            // Source fallback leaves the current draft visible until exact table semantics are available.
            return;
        };
        let Some(table) = snapshot
            .tables
            .iter()
            .find(|table| table.utf16_range.start == active.coordinate.table_utf16_start)
        else {
            // A removed table has no valid navigation target; do not commit into another parsed block.
            return;
        };
        let Some(target) =
            native_markdown_table_navigation_target(table, active.coordinate, navigation)
        else {
            // Movement beyond an existing row/column boundary preserves the current field and draft.
            return;
        };
        if !self.commit_cell_before_command() {
            // Preserve the pending cell draft rather than advancing this native command.
            return;
        }
        self.request_table_cell_activation(target);
    }

    /// Moves the reusable field when matching viewport geometry is available, preserving any uncached draft.
    pub(super) fn reposition_active_table_cell_editor(&self) {
        let Some(active) = self.ivars().active_table_cell.borrow().clone() else {
            // No reusable field draft exists to navigate, move, or replace.
            return;
        };
        let frame = self
            .ivars()
            .visible_table_layout_cache
            .borrow()
            .as_ref()
            .and_then(|cache| {
                cache.tables.iter().find_map(|table| {
                    (table.table_utf16_start == active.coordinate.table_utf16_start).then(|| {
                        table
                            .cells
                            .iter()
                            .find(|cell| cell.coordinate == active.coordinate)
                    })?
                })
            })
            .map(|cell| cell.frame);
        let Some(frame) = frame else {
            // Missing/offscreen geometry never discards a draft or guesses its visual placement.
            return;
        };
        if let Some(editor) = self.ivars().cell_editor.get() {
            // Move only the reusable native shell after ending the layout-cache borrow above.
            editor.setFrame(NSInsetRect(frame, 1.0, 1.0));
        }
        if let Some(active) = self.ivars().active_table_cell.borrow_mut().as_mut() {
            // Update finite cancellation/damage metadata after native geometry has settled.
            active.frame = frame;
        }
    }

    /// Releases derived table geometry and optionally commits the active cell before changing presentation.
    pub(super) fn invalidate_visible_table_layouts(
        &self,
        end_cell_editing: bool,
    ) -> Result<(), String> {
        if end_cell_editing {
            // A failed cell edit vetoes presentation changes while retaining the visible draft.
            self.commit_active_table_cell()?;
        }
        // Only an accepted transition can release pending semantic coordinates and derived geometry.
        self.ivars().pending_table_cell_activation.set(None);
        self.ivars().visible_table_layout_cache.borrow_mut().take();
        self.setNeedsDisplay(true);
        Ok(())
    }

    /// Inserts one small source table with valid block spacing through the canonical text-input path.
    pub(super) fn insert_default_markdown_table(&self) {
        if !self.commit_cell_before_command() {
            // Preserve the pending cell draft rather than advancing this native command.
            return;
        }
        if !self.commit_composition_before_command() {
            // The toolbar inserts canonical block source only after the native provisional composition ends.
            return;
        }
        let Some(text_storage) = self.ivars().text_storage.load() else {
            // A disconnected weak storage cannot mutate or navigate a successor document.
            return;
        };
        let selection = self
            .semantic_selection_range()
            .unwrap_or_else(|| self.super_selected_range());
        let Some(selection_end) = selection.location.checked_add(selection.length) else {
            // Invalid native arithmetic cannot select a canonical range or publish a partial source table.
            return;
        };
        let Some(document) = text_storage.document() else {
            // A delayed button action cannot reconstruct the document after its unique owner released it.
            return;
        };
        let document = document.borrow();
        let document_length = document.utf16_len();
        if selection_end > document_length {
            // Stale native selection is rejected instead of clipping away source the user did not select.
            return;
        }
        let needs_prefix = selection.location > 0
            && !document
                .utf16_code_unit_at(selection.location - 1)
                .is_ok_and(|unit| unit == b'\n' as u16 || unit == b'\r' as u16);
        let needs_suffix = selection_end < document_length
            && !document
                .utf16_code_unit_at(selection_end)
                .is_ok_and(|unit| unit == b'\n' as u16 || unit == b'\r' as u16);
        drop(document);
        let markdown = native_markdown_default_table_source(needs_prefix, needs_suffix);
        // The optional blank-line prefix shifts both table authority and its first editable cell by two UTF-16 units.
        let prefix_units = usize::from(needs_prefix) * 2;
        let first_cell_offset = selection.location + prefix_units + "| ".encode_utf16().count();
        if text_storage
            .replace_table_cell(selection.location..selection_end, &markdown)
            .is_none()
        {
            // Capacity/scalar failure preserves source and leaves the current selection/table field unchanged.
            return;
        }
        self.clear_pending_selection();
        self.setSelectedRange(NSRange::new(first_cell_offset, 0));
        self.ivars().visible_table_layout_cache.borrow_mut().take();
        self.scrollRangeToVisible(NSRange::new(first_cell_offset, 0));
        self.ivars()
            .pending_table_cell_activation
            .set(Some(NativeMarkdownTableCellCoordinate {
                table_utf16_start: selection.location + prefix_units,
                row_index: 0,
                column_index: 0,
            }));
        self.schedule_visible_table_layout_update();
        self.setNeedsDisplay(true);
    }
}
