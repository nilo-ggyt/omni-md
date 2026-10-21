//! Native list for the current bounded metadata page; scrolling and reuse neither read note bodies nor advance the directory cursor.

use super::*;
use objc2_app_kit::{
    NSTableColumn, NSTableView, NSTableViewColumnAutoresizingStyle, NSTableViewDataSource,
    NSTableViewDelegate, NSTableViewStyle, NSUserInterfaceItemIdentification,
};
use objc2_foundation::NSIndexSet;

/// Search subtitles change only result-row height; ordinary directory/note rows retain the existing 30 pt height.
const SEARCH_ROW_HEIGHT: f64 = 48.0;

/// Own only two visible labels, while the table's current metadata page retains file identity.
struct NativeMarkdownSidebarSearchRowIvars {
    title: Retained<NSTextField>,
    subtitle: Retained<NSTextField>,
}

define_class!(
    /// Each reused row owns two visible labels; file identity remains in the original table's bounded metadata page.
    // SAFETY: This main-thread NSView owns two declared controls and implements no event or text-input protocol.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = NativeMarkdownSidebarSearchRowIvars]
    struct NativeMarkdownSidebarSearchRow;

    // SAFETY: NSObjectProtocol adds no methods; construction and cleanup remain on the original AppKit main thread.
    unsafe impl NSObjectProtocol for NativeMarkdownSidebarSearchRow {}
);

impl NativeMarkdownSidebarSearchRow {
    /// Construct only on an actual visible-row request; the same NSTableView reuses offscreen rows without a snippet cache.
    fn new(
        width: f64,
        ledger: &NativeMarkdownOwnedLedger,
        mtm: MainThreadMarker,
    ) -> Result<Retained<Self>, String> {
        let title = Self::label(
            13.0,
            NSRect::new(NSPoint::new(8.0, 27.0), NSSize::new(width - 16.0, 18.0)),
            mtm,
        );
        let subtitle = Self::label(
            11.0,
            NSRect::new(NSPoint::new(8.0, 7.0), NSSize::new(width - 16.0, 16.0)),
            mtm,
        );
        let allocated =
            Self::alloc(mtm).set_ivars(NativeMarkdownSidebarSearchRowIvars { title, subtitle });
        // SAFETY: Both labels are initialized; NSView's designated initializer receives bounded column width and fixed row height.
        let row: Retained<Self> = unsafe {
            msg_send![super(allocated), initWithFrame: NSRect::new(NSPoint::ZERO, NSSize::new(width, SEARCH_ROW_HEIGHT))]
        };
        row.setIdentifier(Some(ns_string!("OmniMarkdownSearchRow")));
        row.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        row.setClipsToBounds(true);
        row.addSubview(&row.ivars().title);
        row.addSubview(&row.ivars().subtitle);
        // Register container, labels, and cells in the session ledger; registration failure drops the whole unpublished row.
        ledger.include("sidebar.search-row", &row)?;
        ledger.include_control("sidebar.row", &row.ivars().title)?;
        ledger.include_control("sidebar.row", &row.ivars().subtitle)?;
        Ok(row)
    }

    /// Independent single-line cells truncate separately, preventing a long title from displacing the snippet or changing row height.
    fn label(size: f64, frame: NSRect, mtm: MainThreadMarker) -> Retained<NSTextField> {
        let label = NSTextField::labelWithString(ns_string!(""), mtm);
        label.setFrame(frame);
        label.setFont(Some(&NSFont::systemFontOfSize(size)));
        label.setEditable(false);
        label.setSelectable(false);
        label.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        if let Some(cell) = label.cell() {
            // Keep native single-line layout while allowing the actual column width to determine tail truncation.
            cell.setUsesSingleLineMode(true);
            cell.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
        }
        label
    }
}

/// One session owns one table/scroll pair; paging replaces only the current metadata projection.
pub(in crate::native_surface) struct NativeMarkdownSidebarList {
    pub(in crate::native_surface) table: Retained<NativeMarkdownSidebarTable>,
    scroll: Retained<NSScrollView>,
}

/// Data-source callbacks access only this bounded projection, without borrowing foreground surface or directory scanner.
pub(in crate::native_surface) struct NativeMarkdownSidebarTableIvars {
    rows: RefCell<Vec<NativeMarkdownSidebarRow>>,
    active_path: RefCell<Option<String>>,
    /// Browse directory is independent of the open body; paging, collapse, and search cannot reset it to the note's parent.
    selected_directory: RefCell<String>,
    selection_revision: Cell<u64>,
    revision: Cell<u64>,
    closed: Cell<bool>,
    ledger: std::rc::Weak<NativeMarkdownOwnedLedger>,
    /// Track created labels weakly, allowing close to clear text still held temporarily by AppKit's reuse pool.
    labels: RefCell<Vec<Weak<NSTextField>>>,
}

define_class!(
    /// AppKit owns visibility, scrolling, arrow navigation, and type selection; the project supplies current-page metadata/actions only.
    // SAFETY: This main-thread NSTableView uses public data-source/delegate APIs and exact declared selector signatures.
    #[unsafe(super = NSTableView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = NativeMarkdownSidebarTableIvars]
    pub(in crate::native_surface) struct NativeMarkdownSidebarTable;

    impl NativeMarkdownSidebarTable {
        #[unsafe(method(createMarkdownNoteInSelectedDirectory:))]
        fn create_note_in_selected_directory(&self, _sender: &AnyObject) {
            // Capture the selected directory before queuing the existing create-note workflow.
            self.enqueue_directory_action(NativeMarkdownSidebarAction::CreateNote);
        }

        #[unsafe(method(createMarkdownDirectoryInSelectedDirectory:))]
        fn create_directory_in_selected_directory(&self, _sender: &AnyObject) {
            // Directory creation shares original selection authority and the same deferred mutation boundary.
            self.enqueue_directory_action(NativeMarkdownSidebarAction::CreateDirectory);
        }

        #[unsafe(method(importMarkdownNoteInSelectedDirectory:))]
        fn import_note_in_selected_directory(&self, _sender: &AnyObject) {
            // Import keeps the selected-directory ticket rather than deriving destination from the open body.
            self.enqueue_directory_action(NativeMarkdownSidebarAction::ImportNote);
        }

        #[unsafe(method(activateMarkdownSidebarSelection:))]
        fn activate_selection(&self, _sender: Option<&AnyObject>) {
            if let Ok(row) = usize::try_from(self.selectedRow()) {
                // Queue one existing directory/note action, matching the old button without switching body on the native callback stack.
                self.enqueue_activation(row);
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if matches!(event.keyCode(), 36 | 76)
                && event.modifierFlags().intersection(NSEventModifierFlags::DeviceIndependentFlagsMask).is_empty()
            {
                // Unmodified Return activates the focused list row; it neither registers nor handles a Host global shortcut.
                if let Ok(row) = usize::try_from(self.selectedRow()) {
                    self.enqueue_activation(row);
                }
            } else {
                // Preserve NSTableView's arrows, Tab, and type selection without reinterpreting text-editing keys.
                // SAFETY: Forward the original NSEvent synchronously to the same NSTableView superclass ABI.
                unsafe { let _: () = msg_send![super(self), keyDown: event]; }
            }
        }

        #[unsafe(method_id(menuForEvent:))]
        fn menu_for_event(&self, event: &NSEvent) -> Option<Retained<NSMenu>> {
            // Build only the actually hit row's temporary owned menu; stale/closed targets return none.
            self.context_menu(event)
        }
    }

    // SAFETY: NSObjectProtocol adds no methods; every instance and message stays on AppKit's main thread.
    unsafe impl NSObjectProtocol for NativeMarkdownSidebarTable {}
    // SAFETY: NSTableViewDelegate requires this public parent conformance, without additional text-editing actions.
    unsafe impl NSControlTextEditingDelegate for NativeMarkdownSidebarTable {}

    // SAFETY: The synchronous data source returns the bounded page count, without disk access or borrowing external UI owners.
    unsafe impl NSTableViewDataSource for NativeMarkdownSidebarTable {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> isize {
            // The admitted page is at most 13 entries, so its metadata length fits the native row-count ABI.
            self.ivars().rows.borrow().len() as isize
        }
    }

    // SAFETY: viewFor returns NSView ownership required by the protocol; metadata borrows end before native calls.
    unsafe impl NSTableViewDelegate for NativeMarkdownSidebarTable {
        #[unsafe(method(tableView:heightOfRow:))]
        fn height_of_row(&self, _table: &NSTableView, row: isize) -> f64 {
            // Read per-row metadata without changing Core page capacity or directory scrolling according to viewport height.
            if usize::try_from(row).ok().and_then(|index| self.ivars().rows.borrow().get(index).map(|row| row.subtitle.is_some())).unwrap_or(false) {
                SEARCH_ROW_HEIGHT
            } else {
                sidebar_geometry::ROW_HEIGHT
            }
        }

        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn row_view(&self, table: &NSTableView, _column: Option<&NSTableColumn>, row: isize) -> Option<Retained<NSView>> {
            // Delegate visible-row construction to the fallible helper after AppKit supplies its actual row index.
            self.make_row_view(table, row)
        }

        #[unsafe(method_id(tableView:typeSelectStringForTableColumn:row:))]
        fn type_select_title(&self, _table: &NSTableView, _column: Option<&NSTableColumn>, row: isize) -> Option<Retained<NSString>> {
            // Native type selection compares original metadata title, excluding indentation and file-opening side effects.
            self.type_select_string(row)
        }
    }
);

impl NativeMarkdownSidebarTable {
    /// Native type selection compares current-page original titles only, without indentation or document opening.
    fn type_select_string(&self, row: isize) -> Option<Retained<NSString>> {
        if self.ivars().closed.get() {
            // A framework-cached table cannot restore access to metadata after close.
            return None;
        }
        let index = usize::try_from(row).ok()?;
        let title = self.ivars().rows.borrow().get(index)?.title.clone();
        Some(NSString::from_str(&title))
    }

    /// Ordinary Rust return values handle closed lists, missing pages, and menu registration failure.
    fn context_menu(&self, event: &NSEvent) -> Option<Retained<NSMenu>> {
        if self.ivars().closed.get() {
            return None; // Closed reused views grant no authority to generate file actions.
        }
        let point = self.convertPoint_fromView(event.locationInWindow(), None);
        let row = usize::try_from(self.rowAtPoint(point)).ok()?;
        let revision = self.ivars().revision.get();
        let kind = self.ivars().rows.borrow().get(row)?.kind;
        if !self.permits(revision, row) {
            return None; // Require the actually hit current row; blank space cannot reuse the last clicked row.
        }
        let ledger = self.ivars().ledger.upgrade()?;
        let target = NativeMarkdownSidebarMenuTarget::new(row, revision, self, self.mtm());
        let menu = native_markdown_sidebar_menu(&target, kind, self.mtm());
        if let Err(error) = ledger
            .include("sidebar.context.target", &target)
            .and_then(|()| ledger.include_menu("sidebar.context.menu", &menu))
        {
            present_native_markdown_error(&error);
            return None; // Unregistered target/menu stay local and cannot be transferred to AppKit tracking.
        }
        Some(menu)
    }
    /// Request labels for current visible rows only; the Rust helper preserves fallible reuse and ledger registration.
    fn make_row_view(&self, table: &NSTableView, row: isize) -> Option<Retained<NSView>> {
        let index = usize::try_from(row).ok()?;
        let metadata = self.ivars().rows.borrow().get(index).cloned()?;
        let active =
            self.ivars().active_path.borrow().as_deref() == Some(metadata.relative_path.as_str());
        let revision = self.ivars().revision.get();
        if metadata.subtitle.is_some() {
            // Search reuses a separate two-label container; ordinary rows keep the single-label construction below.
            return self.make_search_row_view(table, metadata, active, revision);
        }
        // SAFETY: Only this table creates NSTextField under this identifier; there is no nib or factory with another owner.
        let reused = unsafe {
            table.makeViewWithIdentifier_owner(ns_string!("OmniMarkdownSidebarRow"), None)
        };
        let field = match reused {
            Some(view) => view.downcast::<NSTextField>().ok()?, // Reject an incompatible reuse identity without rebuilding an unknown subtree.
            None => {
                let field = NSTextField::labelWithString(ns_string!(""), self.mtm());
                field.setIdentifier(Some(ns_string!("OmniMarkdownSidebarRow")));
                field.setFont(Some(&NSFont::systemFontOfSize(13.0)));
                field.setEditable(false);
                field.setSelectable(false);
                field.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
                if let Some(cell) = field.cell() {
                    // Width changes affect tail truncation only, preserving relative file identity and complete accessibility text.
                    cell.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
                }
                let ledger = self.ivars().ledger.upgrade()?;
                if let Err(error) = ledger.include_control("sidebar.row", &field) {
                    present_native_markdown_error(&error);
                    return None; // A row absent from the original resource ledger cannot become visible content.
                }
                let mut labels = self.ivars().labels.take();
                labels.retain(|label| label.load().is_some());
                labels.push(Weak::from_retained(&field));
                *self.ivars().labels.borrow_mut() = labels;
                field
            }
        };
        if self.ivars().closed.get() || self.ivars().revision.get() != revision {
            return None; // Creation/reuse reentry that changes page or closes the table cannot publish old text into a successor row.
        }
        field.setStringValue(&NSString::from_str(&native_markdown_sidebar_button_title(
            &metadata,
        )));
        field.setTextColor(Some(&native_markdown_note_color(
            active && metadata.kind == NativeMarkdownSidebarEntryKind::Note,
        )));
        // Open-note identity differs from native table selection; font weight still identifies the note after focus moves to the body.
        // Reset weight on every reuse so an old note's emphasis cannot carry into another page or directory row.
        let weight = if active && metadata.kind == NativeMarkdownSidebarEntryKind::Note {
            // SAFETY: This public process-lifetime font-weight constant retains neither row nor business object.
            unsafe { NSFontWeightSemibold }
        } else {
            // SAFETY: The same public-constant contract applies; a reused inactive row restores regular weight.
            unsafe { NSFontWeightRegular }
        };
        field.setFont(Some(&NSFont::systemFontOfSize_weight(13.0, weight)));
        field.setToolTip(Some(&NSString::from_str(&metadata.relative_path)));
        let kind = match metadata.kind {
            NativeMarkdownSidebarEntryKind::Root => "笔记库",
            NativeMarkdownSidebarEntryKind::Directory => {
                if metadata.is_collapsed {
                    "已折叠文件夹"
                } else {
                    "已展开文件夹"
                }
            }
            NativeMarkdownSidebarEntryKind::Note => {
                if active {
                    "当前笔记"
                } else {
                    "笔记"
                }
            }
        };
        field.setAccessibilityLabel(Some(&NSString::from_str(&format!(
            "{kind}：{}",
            metadata.title
        ))));
        Some(field.into_super().into_super())
    }

    /// Present original title/path/snippet separately; the table's page ticket still owns row identity, menus, and activation.
    fn make_search_row_view(
        &self,
        table: &NSTableView,
        metadata: NativeMarkdownSidebarRow,
        active: bool,
        revision: u64,
    ) -> Option<Retained<NSView>> {
        // SAFETY: Only this class creates search containers under this identifier; never inspect unknown system reuse subtrees.
        let reused = unsafe {
            table.makeViewWithIdentifier_owner(ns_string!("OmniMarkdownSearchRow"), None)
        };
        let row = match reused {
            Some(view) => view.downcast::<NativeMarkdownSidebarSearchRow>().ok()?, // An incompatible reused container grants no label authority.
            None => {
                let ledger = self.ivars().ledger.upgrade()?;
                let width = table.bounds().size.width;
                let row = match NativeMarkdownSidebarSearchRow::new(width, &ledger, self.mtm()) {
                    Ok(row) => row,
                    Err(error) => {
                        present_native_markdown_error(&error);
                        return None; // Ledger failure prevents publication of a partially registered row.
                    }
                };
                // Share the existing weak cleanup list; a framework-cached container must not retain old business text.
                let mut labels = self.ivars().labels.take();
                labels.retain(|label| label.load().is_some());
                labels.push(Weak::from_retained(&row.ivars().title));
                labels.push(Weak::from_retained(&row.ivars().subtitle));
                *self.ivars().labels.borrow_mut() = labels;
                row
            }
        };
        if self.ivars().closed.get() || self.ivars().revision.get() != revision {
            return None; // A page change during construction/reuse prevents old search text entering successor presentation.
        }
        let title = &row.ivars().title;
        let subtitle = &row.ivars().subtitle;
        let current = || !self.ivars().closed.get() && self.ivars().revision.get() == revision;
        // Guard every native temporary and setter with the original page ticket; reentry cannot mutate successor reused labels.
        let native_title =
            native_markdown_table_intent_step(&current, || NSString::from_str(&metadata.title))
                .ok()?;
        native_markdown_table_intent_step(&current, || title.setStringValue(&native_title)).ok()?;
        let native_subtitle = native_markdown_table_intent_step(&current, || {
            NSString::from_str(metadata.subtitle.as_deref().expect("搜索行具有副行"))
        })
        .ok()?;
        native_markdown_table_intent_step(&current, || subtitle.setStringValue(&native_subtitle))
            .ok()?;
        let title_color =
            native_markdown_table_intent_step(&current, || native_markdown_note_color(active))
                .ok()?;
        native_markdown_table_intent_step(&current, || title.setTextColor(Some(&title_color)))
            .ok()?;
        let subtitle_color = native_markdown_table_intent_step(&current, || {
            NSColor::colorWithSRGBRed_green_blue_alpha(0.60, 0.64, 0.71, 1.0)
        })
        .ok()?;
        native_markdown_table_intent_step(&current, || {
            subtitle.setTextColor(Some(&subtitle_color))
        })
        .ok()?;
        let description = native_markdown_table_intent_step(&current, || {
            NSString::from_str(&metadata.search_description())
        })
        .ok()?;
        native_markdown_table_intent_step(&current, || title.setToolTip(Some(&description)))
            .ok()?;
        native_markdown_table_intent_step(&current, || subtitle.setToolTip(Some(&description)))
            .ok()?;
        native_markdown_table_intent_step(&current, || {
            title.setAccessibilityLabel(Some(&description))
        })
        .ok()?;
        let subtitle_description = native_markdown_table_intent_step(&current, || {
            NSString::from_str(&format!(
                "路径与摘要：{}",
                metadata.subtitle.as_deref().expect("搜索行具有副行")
            ))
        })
        .ok()?;
        native_markdown_table_intent_step(&current, || {
            subtitle.setAccessibilityLabel(Some(&subtitle_description))
        })
        .ok()?;
        Some(row.into_super())
    }

    /// Validate original list/page/row identity, never treating a reused label address as business file identity.
    pub(in crate::native_surface) fn permits(&self, revision: u64, row: usize) -> bool {
        self.permits_origin(&NativeMarkdownSidebarActionOrigin::Row {
            row_index: row,
            revision,
        })
    }

    /// Capture actual directory selection without native getters or requiring that directory in the bounded current page.
    pub(in crate::native_surface) fn directory_origin(&self) -> NativeMarkdownSidebarActionOrigin {
        NativeMarkdownSidebarActionOrigin::SelectedDirectory {
            relative_path: self.ivars().selected_directory.borrow().clone(),
            selection_revision: self.ivars().selection_revision.get(),
        }
    }

    /// Explicit selection or successful receipt revokes old button requests, even if the same path is selected again later.
    fn select_directory(&self, path: &str) {
        if self.ivars().closed.get() {
            return; // Late installation/receipts cannot repopulate directory metadata after teardown.
        }
        self.ivars().selection_revision.set(
            self.ivars()
                .selection_revision
                .get()
                .checked_add(1)
                .expect("原生目录选择代次不能耗尽"),
        );
        *self.ivars().selected_directory.borrow_mut() = path.to_owned();
    }

    /// Ordinary callbacks check the unique foreground; the actual surface lease uses the same pure origin validation.
    pub(in crate::native_surface) fn permits_origin(
        &self,
        origin: &NativeMarkdownSidebarActionOrigin,
    ) -> bool {
        ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            active
                .as_ref()
                .is_some_and(|surface| self.origin_matches_surface(surface, origin))
        })
    }

    /// Page revision identifies metadata and directory revision identifies user intent; no global Active borrow is needed at mutation admission.
    pub(in crate::native_surface) fn origin_matches_surface(
        &self,
        surface: &NativeMarkdownWindowSurface,
        origin: &NativeMarkdownSidebarActionOrigin,
    ) -> bool {
        if self.ivars().closed.get() || !std::ptr::eq(&*surface.sidebar_list.table, self) {
            return false; // Equal directory names or page numbers cannot transfer authority across sessions.
        }
        match origin {
            // Row actions require the original page and actual row, without borrowing independent browse-directory authority.
            NativeMarkdownSidebarActionOrigin::Row {
                row_index,
                revision,
            } => {
                self.ivars().revision.get() == *revision
                    && *row_index < self.ivars().rows.borrow().len()
            }
            // Directory selection is page-independent; save refresh/paging preserves intent, while directory change revokes its generation.
            NativeMarkdownSidebarActionOrigin::SelectedDirectory {
                relative_path,
                selection_revision,
            } => {
                self.ivars().selection_revision.get() == *selection_revision
                    && *self.ivars().selected_directory.borrow() == *relative_path
            }
        }
    }

    /// Three fixed buttons capture directory before the first native operation, then share context-menu queuing and file workflow.
    fn enqueue_directory_action(&self, action: NativeMarkdownSidebarAction) {
        let origin = self.directory_origin();
        if !self.permits_origin(&origin) {
            return; // A closed or currently leased original table cannot create files for a successor session.
        }
        enqueue_native_markdown_sidebar_action(Weak::from(self), origin, action, self.mtm());
    }

    /// Queue the original row/page weakly so native selection returns before note switching or directory refresh.
    fn enqueue_activation(&self, row: usize) {
        let revision = self.ivars().revision.get();
        if !self.permits(revision, row) {
            return; // A late event cannot open the equal-index note from a replacement page.
        }
        let weak = MainThreadBound::new(Weak::from(self), self.mtm());
        enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("侧栏选择在AppKit主队列执行");
            if !weak
                .get(mtm)
                .load()
                .is_some_and(|table| table.permits(revision, row))
            {
                return; // Changed page/foreground while queued revokes this action without acquiring successor file authority.
            }
            if let Err(error) = activate_native_markdown_sidebar_row(row) {
                // Report actual activation failure without replaying a possibly completed save/switch operation.
                present_native_markdown_error(&error);
                eprintln!("Markdown sidebar activation failed: {error}");
            }
        });
    }

    /// Reuse pools may hold offscreen labels; copy weak handles before clearing them, keeping borrows outside native messages.
    fn clear_labels(&self) {
        let labels = self.ivars().labels.borrow().clone();
        for label in labels {
            if let Some(label) = label.load() {
                // Erase business text and path/accessibility metadata even when AppKit still leases the empty label shell.
                label.setStringValue(ns_string!(""));
                label.setToolTip(None);
                label.setAccessibilityLabel(None);
            }
        }
    }
}

impl NativeMarkdownSidebarList {
    /// Build the session's one table/scroll pair, register actual native roles, and publish only a complete bounded projection.
    pub(in crate::native_surface) fn new(
        rows: &[NativeMarkdownSidebarRow],
        selected_path: Option<&str>,
        content: &NSView,
        ledger: &Rc<NativeMarkdownOwnedLedger>,
        mtm: MainThreadMarker,
    ) -> Result<Self, String> {
        let frame = sidebar_geometry::list_frame(content.bounds().size.height);
        let table =
            NativeMarkdownSidebarTable::alloc(mtm).set_ivars(NativeMarkdownSidebarTableIvars {
                rows: RefCell::new(Vec::new()),
                active_path: RefCell::new(None),
                selected_directory: RefCell::new(
                    selected_path
                        .map(native_markdown_note_parent)
                        .unwrap_or_default(),
                ),
                selection_revision: Cell::new(0),
                revision: Cell::new(0),
                closed: Cell::new(false),
                ledger: Rc::downgrade(ledger),
                labels: RefCell::new(Vec::new()),
            });
        // SAFETY: Initialize this table once; every data-source callback sees fully initialized Rust ivars.
        let table: Retained<NativeMarkdownSidebarTable> = unsafe {
            msg_send![super(table), initWithFrame: NSRect::new(NSPoint::ZERO, frame.size)]
        };
        let column =
            NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), ns_string!("note"));
        column.setWidth(frame.size.width);
        table.addTableColumn(&column);
        table.setHeaderView(None);
        table.setCornerView(None);
        table.setAllowsColumnReordering(false);
        table.setAllowsColumnResizing(false);
        table.setColumnAutoresizingStyle(
            NSTableViewColumnAutoresizingStyle::LastColumnOnlyAutoresizingStyle,
        );
        table.setAllowsMultipleSelection(false);
        table.setAllowsEmptySelection(true);
        table.setAllowsColumnSelection(false);
        table.setAllowsTypeSelect(true);
        table.setStyle(NSTableViewStyle::Plain);
        table.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        table.setRowHeight(sidebar_geometry::ROW_HEIGHT);
        table.setIntercellSpacing(NSSize::new(0.0, 2.0));
        table.setBackgroundColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
            0.06, 0.067, 0.086, 1.0,
        ));
        table.setAccessibilityLabel(Some(ns_string!("笔记列表")));
        // SAFETY: The unique session retains table; weak data-source/delegate/target links are explicitly detached at close.
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(&*table)));
            table.setDelegate(Some(ProtocolObject::from_ref(&*table)));
            table.setTarget(Some(&*table));
            table.setAction(Some(sel!(activateMarkdownSidebarSelection:)));
        }
        // Scrolling changes only the native viewport; it cannot advance a Core pager or read a document body.
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
        scroll.setHasVerticalScroller(true);
        scroll.setHasHorizontalScroller(false);
        scroll.setAutohidesScrollers(true);
        scroll.setDrawsBackground(false);
        scroll.setAutoresizingMask(NSAutoresizingMaskOptions::ViewHeightSizable);
        scroll.setDocumentView(Some(&table));
        // Keep the real scroll/clip/table/column identities in the original ledger before publishing to content.
        ledger.include("sidebar.scroll", &scroll)?;
        ledger.include("sidebar.clip", &scroll.contentView())?;
        ledger.include("sidebar.table", &table)?;
        ledger.include("sidebar.column", &column)?;
        let list = Self { table, scroll };
        list.replace(rows, selected_path)?;
        content.addSubview(&list.scroll);
        Ok(list)
    }

    /// Paging replaces only this 13-row projection; programmatic selection triggers no click action or document open.
    pub(in crate::native_surface) fn replace(
        &self,
        rows: &[NativeMarkdownSidebarRow],
        selected_path: Option<&str>,
    ) -> Result<(), String> {
        if self.table.ivars().closed.get() {
            // Framework-retained closed tables cannot be repopulated by late page delivery.
            return Err("笔记列表已经关闭".into());
        }
        if rows.len() > sidebar_geometry::MINIMUM_PAGE_ROWS {
            // Reject excess metadata rather than silently truncating a Core page or enlarging the UI resource budget.
            return Err("笔记列表超出当前页容量".into());
        }
        let revision = self
            .table
            .ivars()
            .revision
            .get()
            .checked_add(1)
            .ok_or("笔记列表已结束，请重新打开")?;
        self.table.ivars().revision.set(revision);
        *self.table.ivars().rows.borrow_mut() = rows.to_vec();
        self.table.clear_labels();
        if self.table.ivars().closed.get() || self.table.ivars().revision.get() != revision {
            // Clearing reused labels can call native code; reject a page or close that replaced this exact projection.
            return Err("笔记列表已经变化".into());
        }
        self.select_active(selected_path);
        if self.table.ivars().closed.get() || self.table.ivars().revision.get() != revision {
            // Selection/reload reentry cannot scroll a successor page using this old installation intent.
            return Err("笔记列表已经变化".into());
        }
        self.scroll.contentView().scrollToPoint(NSPoint::ZERO);
        self.scroll
            .reflectScrolledClipView(&self.scroll.contentView());
        Ok(())
    }

    /// Open-note identity changes visual emphasis only; native selected row still follows the independent browse directory.
    pub(in crate::native_surface) fn select_active(&self, path: Option<&str>) {
        if self.table.ivars().closed.get() {
            return; // Late document installation cannot reload old titles into a closed table.
        }
        let revision = self.table.ivars().revision.get();
        let selection_revision = self.table.ivars().selection_revision.get();
        *self.table.ivars().active_path.borrow_mut() = path.map(str::to_owned);
        let selected = {
            let directory = self.table.ivars().selected_directory.borrow();
            self.table.ivars().rows.borrow().iter().position(|row| {
                row.kind != NativeMarkdownSidebarEntryKind::Note && row.relative_path == *directory
            })
        };
        self.table.reloadData();
        if self.table.ivars().closed.get()
            || self.table.ivars().revision.get() != revision
            || self.table.ivars().selection_revision.get() != selection_revision
        {
            return; // reloadData can reenter page/selection/close; an old position cannot select an equal-index successor row.
        }
        match selected {
            Some(index) => self.table.selectRowIndexes_byExtendingSelection(
                &NSIndexSet::indexSetWithIndex(index),
                false,
            ),
            None => {
                // SAFETY: A nil sender requests programmatic deselection only, without file actions or editing transactions.
                unsafe {
                    self.table.deselectAll(None);
                }
            }
        }
    }

    /// Revoke the current row projection through the same bounded replacement/revision contract.
    pub(in crate::native_surface) fn clear(&self) -> Result<(), String> {
        self.replace(&[], None)
    }

    /// Only explicit directory selection or successful body installation changes browse destination; no native message is sent here.
    pub(in crate::native_surface) fn select_directory(&self, path: &str) {
        self.table.select_directory(path);
    }

    /// Consume the same original mutation's successful receipt; failure, cancellation, and unrelated work cannot move browse selection.
    pub(in crate::native_surface) fn apply_directory_mutation(
        &self,
        mutation: &NativeMarkdownFileMutation,
        result: &NativeMarkdownFileMutationResult,
    ) {
        let directory = self.table.ivars().selected_directory.borrow().clone();
        if let Some(path) = native_markdown_directory_after_mutation(&directory, mutation, result) {
            self.select_directory(&path);
        }
    }

    /// Revoke callbacks/metadata before detaching containers, leaving no filename or action target in a reuse cache.
    pub(in crate::native_surface) fn detach(&self) {
        if self.table.ivars().closed.replace(true) {
            return; // Explicit close and Rust Drop share one cleanup, without sending a second message pass to a detached graph.
        }
        // Return the whole bounded page/path allocation; clear alone would leave old Vec capacity in a framework-cached table.
        // take ends each slot borrow before destruction, so native detachment crosses no metadata borrow.
        drop(self.table.ivars().rows.take());
        drop(self.table.ivars().active_path.take());
        drop(self.table.ivars().selected_directory.take());
        // SAFETY: The original unique table owner detaches every weak protocol/action entry on the main thread.
        unsafe {
            self.table.setAction(None);
            self.table.setTarget(None);
            self.table.setDelegate(None);
            self.table.setDataSource(None);
        }
        self.table.clear_labels();
        // Clear actual labels' text, path tooltips, and accessibility metadata before returning weak-array capacity.
        drop(self.table.ivars().labels.take());
        self.table.reloadData();
        // Table owns columns; do not delay their release until table destruction. Copy actual columns before mutation,
        // avoiding iteration over the changing native array; temporary strong column claims end after removal.
        let columns = self.table.tableColumns().to_vec();
        for column in columns {
            self.table.removeTableColumn(&column);
        }
        self.scroll.setDocumentView(None);
        self.table.removeFromSuperview();
        self.scroll.removeFromSuperview();
    }
}

impl Drop for NativeMarkdownSidebarList {
    /// Balance partial construction and explicit-close ownership through the same idempotent native teardown.
    fn drop(&mut self) {
        self.detach(); // Failed construction/installation returns the same actual graph, without a replacement empty list.
    }
}

#[cfg(feature = "native-markdown-runtime-probe")]
/// Full acceptance reuses the current app/main thread/ledger for this real component, without claiming workspace or product acceptance.
pub(in crate::native_surface) fn verify_search_rows(
    parent: &NSView,
    ledger: &Rc<NativeMarkdownOwnedLedger>,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    autoreleasepool(|_| {
        // Use the same bounded search metadata mapping, without reading files, constructing a body, or opening a new window.
        let rows: Vec<_> = (0..sidebar_geometry::MINIMUM_PAGE_ROWS)
            .map(|index| {
                NativeMarkdownSidebarRow::from_search_result(
                    omni_markdown_core::MarkdownSearchResult {
                        relative_path: format!("原目录{index:02}/同名.md"),
                        title: "中文长标题、原始空格与 emoji 😀".repeat(3),
                        snippet: Some("保留完整命中摘要，宽度改变时只按各自行尾省略。".repeat(5)),
                    },
                )
            })
            .collect();
        let list = NativeMarkdownSidebarList::new(
            &rows,
            Some(&rows[0].relative_path),
            parent,
            ledger,
            mtm,
        )?;
        for width in [224.0, 144.0, 224.0] {
            // Real scroll/column resizing lets AppKit lay out reused rows; do not fabricate final label frames.
            let size = list.scroll.frame().size;
            list.scroll.setFrameSize(NSSize::new(width, size.height));
            list.scroll.tile();
            list.table.layoutSubtreeIfNeeded();
            for index in [0usize, 1] {
                let view = list
                    .table
                    .viewAtColumn_row_makeIfNecessary(0, index as isize, true)
                    .ok_or("搜索结果未生成真实行视图")?;
                let row = view
                    .downcast::<NativeMarkdownSidebarSearchRow>()
                    .map_err(|_| "搜索结果未使用两行容器")?;
                let frame = row.bounds();
                let title = &row.ivars().title;
                let subtitle = &row.ivars().subtitle;
                let title_frame = title.frame();
                let subtitle_frame = subtitle.frame();
                let contains = |rect: NSRect| {
                    rect.origin.x >= 0.0
                        && rect.origin.y >= 0.0
                        && rect.size.width > 0.0
                        && rect.size.height > 0.0
                        && rect.origin.x + rect.size.width <= frame.size.width
                        && rect.origin.y + rect.size.height <= frame.size.height
                };
                // SDK rectOfRow includes intercell spacing while content view does not; preserve actual 48 pt content and spacing separately.
                if frame.size.height != SEARCH_ROW_HEIGHT
                    || list.table.rectOfRow(index as isize).size.height
                        != SEARCH_ROW_HEIGHT + list.table.intercellSpacing().height
                    || frame.size.width > list.scroll.contentView().bounds().size.width
                    || !contains(title_frame)
                    || !contains(subtitle_frame)
                    || NSIntersectsRect(title_frame, subtitle_frame)
                    || title_frame.size.width != frame.size.width - 16.0
                    || subtitle_frame.size.width != frame.size.width - 16.0
                {
                    // A real out-of-bounds/overlapping native layout fails, with geometry-only diagnostics.
                    return Err(format!(
                        "搜索两行的真实行高、窄列宽或标签边界不一致：requested_width={width} row={index} scroll={:?} clip={:?} table={:?} row_rect={:?} bounds={frame:?} title={title_frame:?} subtitle={subtitle_frame:?}",
                        list.scroll.frame(),
                        list.scroll.contentView().bounds(),
                        list.table.frame(),
                        list.table.rectOfRow(index as isize),
                    ));
                }
                for label in [title, subtitle] {
                    let cell = label.cell().ok_or("搜索标签没有实际文本cell")?;
                    if !cell.usesSingleLineMode()
                        || cell.lineBreakMode() != objc2_app_kit::NSLineBreakMode::ByTruncatingTail
                        || label.toolTip().is_none_or(|value| {
                            value.to_string() != rows[index].search_description()
                        })
                    {
                        // Each real label independently retains single-line truncation and its complete original tooltip.
                        return Err("搜索两行没有各自尾截断或完整Tooltip".into());
                    }
                }
                if title.stringValue().to_string() != rows[index].title
                    || subtitle.stringValue().to_string()
                        != rows[index].subtitle.as_deref().unwrap()
                    || title
                        .accessibilityLabel()
                        .is_none_or(|value| value.to_string() != rows[index].search_description())
                    || subtitle
                        .accessibilityLabel()
                        .is_none_or(|value| !value.to_string().contains(&rows[index].relative_path))
                {
                    // Presentation must preserve title/path/snippet/accessibility identity rather than rewriting bounded source metadata.
                    return Err("搜索标题、原路径、完整摘要或无障碍身份被重写".into());
                }
            }
        }
        // Actual selection of the second equal-title row preserves its second path, without fabricating Active or opening a file.
        list.table
            .selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(1), false);
        let selected =
            usize::try_from(list.table.selectedRow()).map_err(|_| "搜索行无法实际选择")?;
        if list
            .table
            .ivars()
            .rows
            .borrow()
            .get(selected)
            .map(|row| row.relative_path.as_str())
            != Some(rows[1].relative_path.as_str())
        {
            return Err("重名搜索结果的真实选择丢失原相对路径".into());
        }
        // Reach the last bounded row through the actual native scroller, without changing page capacity to fit the viewport.
        let last = (rows.len() - 1) as isize;
        list.table.scrollRowToVisible(last);
        if !NSIntersectsRect(
            list.table.rectOfRow(last),
            list.scroll.contentView().documentVisibleRect(),
        ) {
            return Err("搜索末行不能通过真实滚动到达".into());
        }
        // Capture weak old labels before returning to an ordinary page, so reuse-cache payload cleanup remains observable.
        let previous_labels = list.table.ivars().labels.borrow().clone();
        let (_, ordinary) = native_markdown_sidebar_rows(
            MarkdownWorkspaceSnapshot {
                directories: Vec::new(),
                notes: Vec::new(),
            },
            "原目录",
        );
        list.replace(&ordinary, None)?;
        let ordinary_view = list
            .table
            .viewAtColumn_row_makeIfNecessary(0, 0, true)
            .ok_or("返回目录后没有实际原单行视图")?;
        if ordinary_view.bounds().size.height != sidebar_geometry::ROW_HEIGHT
            || list.table.rectOfRow(0).size.height
                != sidebar_geometry::ROW_HEIGHT + list.table.intercellSpacing().height
        {
            return Err("返回目录后仍使用搜索行高度".into());
        }
        for weak in previous_labels {
            // A surviving empty shell is allowed here, but no old title/path/accessibility payload may remain attached.
            if let Some(label) = weak.load()
                && (label.stringValue().length() != 0
                    || label.toolTip().is_some()
                    || label.accessibilityLabel().is_some())
            {
                return Err("换页后复用池仍持原搜索业务文本".into());
            }
        }
        list.detach();
        // Full close still strictly checks actual nil container/label/cell identities in this ledger, without erasing or exempting them here.
        eprintln!(
            "native_sidebar_search event=two_line_component_pass widths=224,144,224 rows=13 original_path_selection=true scroll_last=true ordinary_height_restored=true old_labels_empty=true formal=false whole_product=false"
        );
        Ok(())
    })
}
