//! Maps bounded vault metadata to sidebar rows and confined file actions.
//! Native menus borrow their target only while the surface owns that target.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// The vault root, a confined directory, or an editable note determines allowed sidebar actions.
pub(super) enum NativeMarkdownSidebarEntryKind {
    Root,
    Directory,
    Note,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// A bounded workspace snapshot row stores relative metadata without opening a document.
pub(super) struct NativeMarkdownSidebarRow {
    pub(super) kind: NativeMarkdownSidebarEntryKind,
    pub(super) relative_path: String,
    pub(super) title: String,
    pub(super) depth: usize,
    pub(super) is_collapsed: bool,
}

/// Build the root and path-sorted metadata rows while preserving the bounded note snapshot.
pub(super) fn native_markdown_sidebar_rows(
    snapshot: MarkdownWorkspaceSnapshot,
) -> (Vec<MarkdownNoteSummary>, Vec<NativeMarkdownSidebarRow>) {
    // The root always exists as a virtual row even when the bounded workspace contains no notes.
    let MarkdownWorkspaceSnapshot { directories, notes } = snapshot;
    let mut rows = Vec::with_capacity(directories.len() + notes.len() + 1);
    rows.push(NativeMarkdownSidebarRow {
        kind: NativeMarkdownSidebarEntryKind::Root,
        relative_path: String::new(),
        title: "OMNI".to_owned(),
        depth: 0,
        is_collapsed: false,
    });
    rows.extend(directories.into_iter().map(|relative_path| {
        // Directory rows copy relative metadata only; no directory or document stays open.
        NativeMarkdownSidebarRow {
            kind: NativeMarkdownSidebarEntryKind::Directory,
            title: native_markdown_path_name(&relative_path),
            depth: Path::new(&relative_path).components().count(),
            is_collapsed: false,
            relative_path,
        }
    }));
    // Notes retain their catalog snapshot separately from the presentation rows for later selection.
    rows.extend(notes.iter().map(|note| {
        // A row contains the stable relative identity and presentation title, not document contents.
        NativeMarkdownSidebarRow {
            kind: NativeMarkdownSidebarEntryKind::Note,
            relative_path: note.relative_path.clone(),
            title: note.title.clone(),
            depth: Path::new(&note.relative_path).components().count(),
            is_collapsed: false,
        }
    }));
    // Keep the virtual root first and sort the remaining metadata deterministically by full path.
    rows[1..].sort_unstable_by(|left, right| {
        // Component identity remains independent of localized display titles.
        left.relative_path.cmp(&right.relative_path)
    });
    (notes, rows)
}

/// Extract a display basename without changing the confined relative identity.
pub(super) fn native_markdown_path_name(relative_path: &str) -> String {
    // A valid UTF-8 basename is preferred; a defensive fallback preserves the existing identity.
    Path::new(relative_path)
        .file_name()
        .and_then(|name| {
            // A valid snapshot identity is UTF-8; invalid native components preserve the fallback below.
            name.to_str()
        })
        .unwrap_or(relative_path)
        .to_owned()
}

/// Return a relative parent identity, with an empty string representing the vault root.
pub(super) fn native_markdown_note_parent(relative_path: &str) -> String {
    // Root-level notes deliberately map to the empty virtual-root identifier.
    Path::new(relative_path)
        .parent()
        .and_then(Path::to_str)
        .unwrap_or_default()
        .to_owned()
}

/// Compare path components so a directory prefix cannot match an unrelated sibling name.
pub(super) fn native_markdown_path_is_inside_directory(
    relative_path: &str,
    directory: &str,
) -> bool {
    if directory.is_empty() {
        // Every confined relative path belongs to the virtual vault root.
        return true;
    }
    // Component comparison distinguishes `notes/a` from the sibling `notes/archive` prefix case.
    Path::new(relative_path).starts_with(Path::new(directory))
}

/// Rebase a descendant after a confirmed directory rename without rewriting sibling paths.
pub(super) fn native_markdown_rewrite_directory_prefix(
    relative_path: &str,
    old_directory: &str,
    new_directory: &str,
) -> Option<String> {
    // Refuse paths outside the renamed subtree instead of inventing a transformed identity.
    let suffix = Path::new(relative_path)
        .strip_prefix(Path::new(old_directory))
        .ok()?;
    Path::new(new_directory)
        .join(suffix)
        .to_str()
        .map(ToOwned::to_owned)
}

/// Render bounded indentation and entry-kind affordances from metadata only.
pub(super) fn native_markdown_sidebar_button_title(row: &NativeMarkdownSidebarRow) -> String {
    // Root depth has no leading indent; each nested component adds one bounded display level.
    let indentation = "   ".repeat(row.depth.saturating_sub(1));
    match row.kind {
        // The virtual root has no indentation or file-kind prefix.
        NativeMarkdownSidebarEntryKind::Root => row.title.clone(),
        // Directory affordances remain distinct from selectable note labels.
        NativeMarkdownSidebarEntryKind::Directory => {
            format!(
                "{indentation}{}  {}",
                if row.is_collapsed {
                    // A collapsed subtree remains discoverable without exposing its descendants.
                    "▸"
                } else {
                    // An expanded row advertises that bounded paging may traverse its descendants.
                    "▾"
                },
                row.title
            )
        }
        // Notes align beneath their parent directory without implying expandable native state.
        NativeMarkdownSidebarEntryKind::Note => format!("{indentation}   {}", row.title),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// User actions are interpreted against the selected row and revalidated by the file state machine.
pub(super) enum NativeMarkdownSidebarAction {
    CreateNote,
    CreateDirectory,
    Rename,
    MoveNote,
    CopyNote,
    RevealInFinder,
    Delete,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Owned mutation arguments survive the deferred release barrier without retaining native controls.
pub(super) enum NativeMarkdownFileMutation {
    CreateNote {
        parent_directory: String,
        title: String,
    },
    CreateDirectory {
        parent_directory: String,
        name: String,
    },
    RenameDirectory {
        relative_path: String,
        name: String,
    },
    DeleteDirectory {
        relative_path: String,
    },
    RenameNote {
        relative_path: String,
        title: String,
    },
    MoveNote {
        relative_path: String,
        destination_directory: String,
    },
    CopyNote {
        relative_path: String,
    },
    DeleteNote {
        relative_path: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Authoritative mutation receipts identify committed paths for editor recovery and selection.
pub(super) enum NativeMarkdownFileMutationResult {
    NoteCreated(String),
    DirectoryCreated,
    DirectoryRenamed(String),
    DirectoryDeleted,
    NoteRenamed(String),
    NoteMoved(String),
    NoteCopied(String),
    NoteDeleted,
}

/// Determine whether a mutation touches the active note directly or through an ancestor directory.
pub(super) fn native_markdown_mutation_targets_active_note(
    mutation: &NativeMarkdownFileMutation,
    active_relative_path: Option<&str>,
) -> bool {
    let Some(active_relative_path) = active_relative_path else {
        // An empty editor cannot be the target of any note/path mutation.
        return false;
    };
    match mutation {
        // Direct note mutations compare exact confined identities, including copy revision checks.
        NativeMarkdownFileMutation::RenameNote { relative_path, .. }
        | NativeMarkdownFileMutation::MoveNote { relative_path, .. }
        | NativeMarkdownFileMutation::CopyNote { relative_path }
        | NativeMarkdownFileMutation::DeleteNote { relative_path } => {
            relative_path == active_relative_path
        }
        // Ancestor mutation affects the active note only when its path lies inside that subtree.
        NativeMarkdownFileMutation::RenameDirectory { relative_path, .. }
        | NativeMarkdownFileMutation::DeleteDirectory { relative_path } => {
            native_markdown_path_is_inside_directory(active_relative_path, relative_path)
        }
        // Creating an independent entry does not mutate an existing note identity.
        NativeMarkdownFileMutation::CreateNote { .. }
        | NativeMarkdownFileMutation::CreateDirectory { .. } => false,
    }
}

/// Decide whether the old document must save and release before the requested file operation.
pub(super) fn native_markdown_mutation_requires_editor_release(
    mutation: &NativeMarkdownFileMutation,
    active_relative_path: Option<&str>,
) -> bool {
    match mutation {
        // A new note becomes active, so an existing editor must release before it is installed.
        NativeMarkdownFileMutation::CreateNote { .. } => active_relative_path.is_some(),
        // A directory-only addition leaves the current document and mapping unchanged.
        NativeMarkdownFileMutation::CreateDirectory { .. } => false,
        // Other mutations require release precisely when they affect the current note or ancestor.
        _ => native_markdown_mutation_targets_active_note(mutation, active_relative_path),
    }
}

/// Require optimistic revision protection when mutating or copying the currently open note.
pub(super) fn native_markdown_mutation_requires_expected_revision(
    mutation: &NativeMarkdownFileMutation,
    active_relative_path: Option<&str>,
) -> bool {
    // Creation and directory mutations use their own confined operation contracts; direct current-note
    // operations additionally compare the saved authority revision to prevent overwriting outside edits.
    matches!(
        mutation,
        NativeMarkdownFileMutation::RenameNote { .. }
            | NativeMarkdownFileMutation::MoveNote { .. }
            | NativeMarkdownFileMutation::CopyNote { .. }
            | NativeMarkdownFileMutation::DeleteNote { .. }
    ) && native_markdown_mutation_targets_active_note(mutation, active_relative_path)
}

/// Choose the next active path from a committed receipt and a successful bounded workspace refresh.
pub(super) fn native_markdown_active_path_after_mutation(
    mutation: &NativeMarkdownFileMutation,
    result: &NativeMarkdownFileMutationResult,
    active_relative_path: Option<&str>,
    first_remaining_note: Option<&str>,
) -> Option<String> {
    // Receipts describe committed authority; selection changes never guess a destination filename.
    match (mutation, result) {
        // Explicit creation opens the new note returned by the authoritative operation.
        (
            NativeMarkdownFileMutation::CreateNote { .. },
            NativeMarkdownFileMutationResult::NoteCreated(relative_path),
        ) => Some(relative_path.clone()),
        // Renaming an ancestor rebases only descendants; unrelated active notes keep their identity.
        (
            NativeMarkdownFileMutation::RenameDirectory { relative_path, .. },
            NativeMarkdownFileMutationResult::DirectoryRenamed(renamed_directory),
        ) => active_relative_path.map(|active_path| {
            // A missing active note remains absent rather than being replaced by an arbitrary descendant.
            if native_markdown_path_is_inside_directory(active_path, relative_path) {
                // Preserve the descendant suffix when its parent directory receives a confirmed new path.
                native_markdown_rewrite_directory_prefix(
                    active_path,
                    relative_path,
                    renamed_directory,
                )
                .unwrap_or_else(|| active_path.to_owned())
            } else {
                // A sibling directory rename must not alter the selected note.
                active_path.to_owned()
            }
        }),
        // Deleting the active subtree selects only a freshly enumerated remaining note, if available.
        (
            NativeMarkdownFileMutation::DeleteDirectory { relative_path },
            NativeMarkdownFileMutationResult::DirectoryDeleted,
        ) if active_relative_path.is_some_and(|active_path| {
            // Only deletion of the selected note's own ancestor changes the active document.
            native_markdown_path_is_inside_directory(active_path, relative_path)
        }) =>
        {
            // Failed refresh supplies no fallback note and therefore cannot reopen a deleted path.
            first_remaining_note.map(ToOwned::to_owned)
        }
        // Active-note rename/move/copy adopts the exact committed receipt destination.
        (
            NativeMarkdownFileMutation::RenameNote { relative_path, .. },
            NativeMarkdownFileMutationResult::NoteRenamed(renamed_path),
        )
        | (
            NativeMarkdownFileMutation::MoveNote { relative_path, .. },
            NativeMarkdownFileMutationResult::NoteMoved(renamed_path),
        )
        | (
            NativeMarkdownFileMutation::CopyNote { relative_path },
            NativeMarkdownFileMutationResult::NoteCopied(renamed_path),
        ) if active_relative_path == Some(relative_path.as_str()) => Some(renamed_path.clone()),
        // Deleting the selected note switches only to a newly confirmed remaining identity.
        (
            NativeMarkdownFileMutation::DeleteNote { relative_path },
            NativeMarkdownFileMutationResult::NoteDeleted,
        ) if active_relative_path == Some(relative_path.as_str()) => {
            first_remaining_note.map(ToOwned::to_owned)
        }
        // Mutations of other entries leave the active document identity unchanged.
        _ => active_relative_path.map(ToOwned::to_owned),
    }
}

/// Prevent concurrent note transitions and file mutations from sharing one foreground editor.
pub(super) fn native_markdown_operation_is_busy(
    note_switch_pending: bool,
    file_mutation_pending: bool,
) -> bool {
    // Both asynchronous transitions own the same editor/path mutation boundary.
    note_switch_pending || file_mutation_pending
}

/// Resolve a bounded row index without accepting stale out-of-range native metadata.
pub(super) fn native_markdown_sidebar_row_at(
    rows: &[NativeMarkdownSidebarRow],
    row_index: usize,
) -> Option<&NativeMarkdownSidebarRow> {
    // Native metadata can outlive a sidebar refresh; checked indexing refuses a removed row.
    rows.get(row_index)
}

/// Restrict each entry kind to its explicit product action set before presenting dialogs.
pub(super) fn native_markdown_sidebar_action_is_allowed(
    kind: NativeMarkdownSidebarEntryKind,
    action: NativeMarkdownSidebarAction,
) -> bool {
    match kind {
        // Root operations may add entries or reveal the vault, but never rename/delete the vault.
        NativeMarkdownSidebarEntryKind::Root => matches!(
            action,
            NativeMarkdownSidebarAction::CreateNote
                | NativeMarkdownSidebarAction::CreateDirectory
                | NativeMarkdownSidebarAction::RevealInFinder
        ),
        // Directories allow confined subtree actions but not note-only move/copy semantics.
        NativeMarkdownSidebarEntryKind::Directory => matches!(
            action,
            NativeMarkdownSidebarAction::CreateNote
                | NativeMarkdownSidebarAction::CreateDirectory
                | NativeMarkdownSidebarAction::Rename
                | NativeMarkdownSidebarAction::RevealInFinder
                | NativeMarkdownSidebarAction::Delete
        ),
        // Notes expose individual file actions without creating children beneath a file.
        NativeMarkdownSidebarEntryKind::Note => matches!(
            action,
            NativeMarkdownSidebarAction::Rename
                | NativeMarkdownSidebarAction::MoveNote
                | NativeMarkdownSidebarAction::CopyNote
                | NativeMarkdownSidebarAction::RevealInFinder
                | NativeMarkdownSidebarAction::Delete
        ),
    }
}

#[derive(Clone)]
/// A copied row and bounded directory list permit modal interaction without a borrowed surface.
pub(super) struct NativeMarkdownSidebarActionContext {
    pub(super) controller: Retained<NativeMarkdownController>,
    pub(super) row: NativeMarkdownSidebarRow,
    pub(super) directories: Vec<String>,
}

/// Give cancellation the standard Escape key independently of focused accessory controls.
pub(super) fn assign_escape_to_native_markdown_alert_cancel(cancel_button: &NSButton) {
    // No modifier is required, so Escape works while editing a name or selecting a destination.
    cancel_button.setKeyEquivalent(ns_string!("\u{1b}"));
    cancel_button.setKeyEquivalentModifierMask(NSEventModifierFlags::empty());
}

/// Collect a trimmed candidate name after explicit confirmation; filesystem validation occurs later.
pub(super) fn run_native_markdown_name_dialog(
    message: &str,
    informative_text: &str,
    initial_value: &str,
    placeholder: &str,
    action_title: &str,
    mtm: MainThreadMarker,
) -> Option<String> {
    // The synchronous modal graph is local; callers must release surface borrows before entering it.
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Informational);
    alert.setMessageText(&NSString::from_str(message));
    alert.setInformativeText(&NSString::from_str(informative_text));
    alert.addButtonWithTitle(&NSString::from_str(action_title));
    let cancel_button = alert.addButtonWithTitle(ns_string!("取消"));
    assign_escape_to_native_markdown_alert_cancel(&cancel_button);

    // Accessory ownership keeps the editable name field live throughout AppKit's modal event loop.
    let input = NSTextField::initWithFrame(
        NSTextField::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(360.0, 24.0)),
    );
    input.setEditable(true);
    input.setSelectable(true);
    input.setStringValue(&NSString::from_str(initial_value));
    input.setPlaceholderString(Some(&NSString::from_str(placeholder)));
    alert.setAccessoryView(Some(&input));
    alert.window().setInitialFirstResponder(Some(&input));
    // SAFETY: the alert retains this NSTextField accessory; selection occurs on its allocating
    // AppKit main thread before the synchronous modal loop, and the sender is intentionally nil.
    unsafe { input.selectText(None) };

    if alert.runModal() != NSAlertFirstButtonReturn {
        // Cancellation produces no candidate name and authorizes no filesystem mutation.
        return None;
    }
    // Return an owned trimmed candidate after the modal ends; domain validation still decides validity.
    let value = input.stringValue().to_string();
    Some(value.trim().to_owned())
}

/// Offers bounded directory suggestions while allowing any explicitly typed confined destination.
pub(super) fn run_native_markdown_move_destination_dialog(
    destination_directories: &[String],
    mtm: MainThreadMarker,
) -> Result<Option<String>, String> {
    // Directory suggestions come from the current metadata page. Editable input keeps directories
    // on later pages reachable without loading an unbounded full-vault destination catalog.
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Informational);
    alert.setMessageText(ns_string!("Move note"));
    alert.setInformativeText(ns_string!("Choose a suggested folder or type its full path relative to the vault. Leave the field empty for the vault root."));
    alert.addButtonWithTitle(ns_string!("Move"));
    assign_escape_to_native_markdown_alert_cancel(&alert.addButtonWithTitle(ns_string!("Cancel")));

    // The local alert owns its editable combo through modal tracking; no surface/document borrow escapes.
    let destinations = objc2_app_kit::NSComboBox::initWithFrame(
        objc2_app_kit::NSComboBox::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(420.0, 28.0)),
    );
    destinations.setEditable(true);
    destinations.setSelectable(true);
    destinations.setCompletes(true);
    destinations.setPlaceholderString(Some(ns_string!("Vault root, or relative folder/path")));
    destinations
        .setAccessibilityLabel(Some(ns_string!("Destination folder relative to the vault")));
    // Suggest only the already copied bounded page; root remains expressible as an empty value.
    for directory in destination_directories {
        if !directory.is_empty() {
            // An actual relative path can be selected unchanged; localized labels never become authority.
            let value = NSString::from_str(directory);
            // SAFETY: NSComboBox stores text entries and this live NSString has the expected object type.
            unsafe {
                destinations.addItemWithObjectValue(&value);
            }
        }
    }
    alert.setAccessoryView(Some(&destinations));
    alert.window().setInitialFirstResponder(Some(&destinations));
    if alert.runModal() != NSAlertFirstButtonReturn {
        // Cancellation authorizes no mutation, including when the typed destination is invalid.
        return Ok(None);
    }
    // Typed paths are candidates only; the core repeats path-component, existence, symlink and
    // collision checks after the modal ends and the active controller identity is revalidated.
    Ok(Some(
        destinations.stringValue().to_string().trim().to_owned(),
    ))
}

/// Authorize destructive deletion only for a note/directory and only after affirmative confirmation.
pub(super) fn run_native_markdown_delete_confirmation(
    row: &NativeMarkdownSidebarRow,
    mtm: MainThreadMarker,
) -> bool {
    // The synchronous modal graph is local; callers must release surface borrows before entering it.
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Warning);
    let (message, informative_text) = match row.kind {
        // Directory deletion must disclose recursive loss before requesting affirmative confirmation.
        NativeMarkdownSidebarEntryKind::Directory => (
            format!("删除目录“{}”？", row.title),
            "目录中的全部笔记和子目录都会被删除，此操作无法撤销。",
        ),
        // A note confirmation describes only the one selected Markdown file.
        NativeMarkdownSidebarEntryKind::Note => (
            format!("删除笔记“{}”？", row.title),
            "这篇笔记会从 OMNI 中删除，此操作无法撤销。",
        ),
        // The virtual vault root is never a legal destructive target, even through a stale action.
        NativeMarkdownSidebarEntryKind::Root => return false,
    };
    alert.setMessageText(&NSString::from_str(&message));
    alert.setInformativeText(&NSString::from_str(informative_text));
    alert.addButtonWithTitle(ns_string!("删除"));
    let cancel_button = alert.addButtonWithTitle(ns_string!("取消"));
    assign_escape_to_native_markdown_alert_cancel(&cancel_button);
    alert.runModal() == NSAlertFirstButtonReturn
}

/// A native button retains scalar row identity and kind, never a document or session owner.
pub(super) struct NativeMarkdownSidebarButtonIvars {
    pub(super) row_index: usize,
    pub(super) kind: NativeMarkdownSidebarEntryKind,
}

define_class!(
    /// A surface-owned row control resolves copied scalar actions through the deferred workspace state machine.
    // SAFETY: this main-thread NSButton subclass uses the exact declared action/menu selectors.
    // Its ivars contain scalar row metadata only; menus are created per event and are never
    // retained as an idle surface resource.
    #[unsafe(super = NSButton)]
    #[thread_kind = MainThreadOnly]
    #[ivars = NativeMarkdownSidebarButtonIvars]
    pub(super) struct NativeMarkdownSidebarButton;

    impl NativeMarkdownSidebarButton {
        #[unsafe(method(activateMarkdownSidebarRow:))]
        fn activate_markdown_sidebar_row(&self, _sender: &AnyObject) {
            // Bind the delayed action to this exact current row control, without retaining it.
            self.enqueue_checked(|row_index| {
                if let Err(error) = activate_native_markdown_sidebar_row(row_index) {
                    // Rejected metadata changes preserve the current editor and show bounded status.
                    present_native_markdown_error(&error);
                    eprintln!("Markdown sidebar activation failed: {error}");
                }
            });
        }

        #[unsafe(method_id(menuForEvent:))]
        fn menu_for_event(&self, _event: &NSEvent) -> Option<Retained<NSMenu>> {
            // AppKit invokes menu construction synchronously on the button's allocating main thread.
            let mtm = MainThreadMarker::new()
                .expect("native Markdown context menus are restricted to the AppKit main thread");
            if !native_markdown_sidebar_button_is_current(self) {
                // A menu event from a removed page cannot acquire authority over replacement rows.
                None
            } else {
                // The active row control owns this event's finite context menu.
                Some(native_markdown_sidebar_menu(self, self.ivars().kind, mtm))
            }
        }

        #[unsafe(method(createMarkdownNoteFromSidebar:))]
        fn create_markdown_note(&self, _sender: &AnyObject) {
            // Create under the selected root/directory after the shared confined action policy validates it.
            self.enqueue_action(NativeMarkdownSidebarAction::CreateNote);
        }

        #[unsafe(method(createMarkdownDirectoryFromSidebar:))]
        fn create_markdown_directory(&self, _sender: &AnyObject) {
            // Create a directory through the same deferred mutation boundary as note creation.
            self.enqueue_action(NativeMarkdownSidebarAction::CreateDirectory);
        }

        #[unsafe(method(renameMarkdownEntryFromSidebar:))]
        fn rename_markdown_entry(&self, _sender: &AnyObject) {
            // Resolve note versus directory rename later from current authoritative row metadata.
            self.enqueue_action(NativeMarkdownSidebarAction::Rename);
        }

        #[unsafe(method(moveMarkdownNoteFromSidebar:))]
        fn move_markdown_note(&self, _sender: &AnyObject) {
            // Present only supported confined destinations through the shared note action path.
            self.enqueue_action(NativeMarkdownSidebarAction::MoveNote);
        }

        #[unsafe(method(copyMarkdownNoteFromSidebar:))]
        fn copy_markdown_note(&self, _sender: &AnyObject) {
            // Request a non-overwriting duplicate using the authoritative mutation receipt.
            self.enqueue_action(NativeMarkdownSidebarAction::CopyNote);
        }

        #[unsafe(method(revealMarkdownEntryFromSidebar:))]
        fn reveal_markdown_entry(&self, _sender: &AnyObject) {
            // Reveal the revalidated row path without creating a competing document owner.
            self.enqueue_action(NativeMarkdownSidebarAction::RevealInFinder);
        }

        #[unsafe(method(deleteMarkdownEntryFromSidebar:))]
        fn delete_markdown_entry(&self, _sender: &AnyObject) {
            // Require the shared destructive confirmation before any confined deletion.
            self.enqueue_action(NativeMarkdownSidebarAction::Delete);
        }
    }
);

impl NativeMarkdownSidebarButton {
    /// Allocate one main-thread row button and install its self-target while the surface retains it.
    pub(super) fn new(
        row_index: usize,
        row: &NativeMarkdownSidebarRow,
        frame: NSRect,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        // Initialize immutable row metadata before NSButton can receive any declared selector.
        let button = Self::alloc(mtm).set_ivars(NativeMarkdownSidebarButtonIvars {
            row_index,
            kind: row.kind,
        });
        // SAFETY: Rust ivars are initialized before the exact superclass designated initializer;
        // allocation and all later accesses remain on the AppKit main thread.
        let button: Retained<Self> = unsafe { msg_send![super(button), initWithFrame: frame] };
        button.setTitle(&NSString::from_str(&native_markdown_sidebar_button_title(
            row,
        )));
        // SAFETY: this live button implements the fixed action selector and targets itself weakly;
        // the owning surface keeps the button alive until target/action are explicitly detached.
        unsafe {
            button.setTarget(Some(&*button));
            button.setAction(Some(sel!(activateMarkdownSidebarRow:)));
        }
        button
    }

    /// Releases native tracking before running work that still belongs to this exact sidebar page.
    fn enqueue_checked<F: Send + FnOnce(usize) + 'static>(&self, work: F) {
        let row_index = self.ivars().row_index;
        let weak = MainThreadBound::new(Weak::from(self), self.mtm());
        enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("sidebar actions execute on the main queue");
            let current = weak
                .get(mtm)
                .load()
                .is_some_and(|button| native_markdown_sidebar_button_is_current(&button));
            if !current {
                // Replaced/closed row controls cannot activate or mutate a same-index successor row.
                return;
            }
            work(row_index);
        });
    }

    /// Defer a copied row/action pair until the menu callback has returned before entering mutation state.
    pub(super) fn enqueue_action(&self, action: NativeMarkdownSidebarAction) {
        self.enqueue_checked(move |row_index| {
            if let Err(error) = request_native_markdown_sidebar_action(row_index, action) {
                // Failed mutations preserve the authoritative document and report only bounded status.
                present_native_markdown_error(&error);
                eprintln!("Markdown file action failed: {error}");
            }
        });
    }
}

/// Checks row-control identity in the current projection, independent of reused indices or file names.
fn native_markdown_sidebar_button_is_current(button: &NativeMarkdownSidebarButton) -> bool {
    ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
        active.as_ref().is_some_and(|surface| {
            surface
                .sidebar_buttons
                .iter()
                .any(|current| std::ptr::eq(&**current, button))
        })
    })
}

/// Construct an ephemeral native context menu containing only actions allowed for this row kind.
pub(super) fn native_markdown_sidebar_menu(
    target: &NativeMarkdownSidebarButton,
    kind: NativeMarkdownSidebarEntryKind,
    mtm: MainThreadMarker,
) -> Retained<NSMenu> {
    // Context menus are created only for the current event and released when native tracking ends.
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!(""));
    match kind {
        // The virtual root exposes creation and reveal only; its identity cannot be deleted/renamed.
        NativeMarkdownSidebarEntryKind::Root => {
            add_native_markdown_menu_item(
                &menu,
                target,
                "新建笔记…",
                sel!(createMarkdownNoteFromSidebar:),
                mtm,
            );
            add_native_markdown_menu_item(
                &menu,
                target,
                "新建目录…",
                sel!(createMarkdownDirectoryFromSidebar:),
                mtm,
            );
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            add_native_markdown_menu_item(
                &menu,
                target,
                "在 Finder 中显示",
                sel!(revealMarkdownEntryFromSidebar:),
                mtm,
            );
        }
        // Directory menus operate on a confined subtree and require explicit destructive confirmation.
        NativeMarkdownSidebarEntryKind::Directory => {
            add_native_markdown_menu_item(
                &menu,
                target,
                "新建笔记…",
                sel!(createMarkdownNoteFromSidebar:),
                mtm,
            );
            add_native_markdown_menu_item(
                &menu,
                target,
                "新建目录…",
                sel!(createMarkdownDirectoryFromSidebar:),
                mtm,
            );
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            add_native_markdown_menu_item(
                &menu,
                target,
                "重命名…",
                sel!(renameMarkdownEntryFromSidebar:),
                mtm,
            );
            add_native_markdown_menu_item(
                &menu,
                target,
                "在 Finder 中显示",
                sel!(revealMarkdownEntryFromSidebar:),
                mtm,
            );
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            add_native_markdown_menu_item(
                &menu,
                target,
                "删除目录…",
                sel!(deleteMarkdownEntryFromSidebar:),
                mtm,
            );
        }
        // Note menus expose exact-file operations and never offer child creation.
        NativeMarkdownSidebarEntryKind::Note => {
            add_native_markdown_menu_item(
                &menu,
                target,
                "重命名…",
                sel!(renameMarkdownEntryFromSidebar:),
                mtm,
            );
            add_native_markdown_menu_item(
                &menu,
                target,
                "移动到…",
                sel!(moveMarkdownNoteFromSidebar:),
                mtm,
            );
            add_native_markdown_menu_item(
                &menu,
                target,
                "创建副本",
                sel!(copyMarkdownNoteFromSidebar:),
                mtm,
            );
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            add_native_markdown_menu_item(
                &menu,
                target,
                "在 Finder 中显示",
                sel!(revealMarkdownEntryFromSidebar:),
                mtm,
            );
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            add_native_markdown_menu_item(
                &menu,
                target,
                "删除笔记…",
                sel!(deleteMarkdownEntryFromSidebar:),
                mtm,
            );
        }
    }
    menu
}

/// Install a fixed declared selector on a menu item whose target outlives the synchronous menu.
pub(super) fn add_native_markdown_menu_item(
    menu: &NSMenu,
    target: &NativeMarkdownSidebarButton,
    title: &str,
    action: objc2::runtime::Sel,
    mtm: MainThreadMarker,
) {
    // SAFETY: action is one of this module's exact declared NSMenuItem-compatible selectors;
    // the surface retains its main-thread button target throughout the synchronous menu.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            ns_string!(""),
        )
    };
    // SAFETY: the fixed selector belongs to this live target, which the foreground surface owns;
    // the temporary menu retains the item but does not become a second target owner.
    unsafe { item.setTarget(Some(target)) };
    menu.addItem(&item);
}

/// Chooses the fixed selected/unselected note-label palette without retaining row state.
pub(super) fn native_markdown_note_color(selected: bool) -> Retained<NSColor> {
    if selected {
        // The active relative note identity receives the foreground selection tint.
        NSColor::colorWithSRGBRed_green_blue_alpha(0.45, 0.66, 1.0, 1.0)
    } else {
        // Other note rows retain the subdued readable metadata tint.
        NSColor::colorWithSRGBRed_green_blue_alpha(0.72, 0.75, 0.82, 1.0)
    }
}

/// Builds one bounded page of explicit native row controls from relative workspace metadata.
pub(super) fn native_markdown_sidebar_buttons(
    rows: &[NativeMarkdownSidebarRow],
    selected_relative_path: Option<&str>,
    content_view: &NSView,
    mtm: MainThreadMarker,
) -> Vec<Retained<NativeMarkdownSidebarButton>> {
    let selected_note_color = native_markdown_note_color(true);
    let unselected_note_color = native_markdown_note_color(false);
    let directory_color = NSColor::colorWithSRGBRed_green_blue_alpha(0.58, 0.63, 0.72, 1.0);
    let root_color = NSColor::colorWithSRGBRed_green_blue_alpha(0.82, 0.85, 0.91, 1.0);
    let content_height = content_view.bounds().size.height;
    let mut buttons = Vec::with_capacity(rows.len());
    for (row_index, row) in rows.iter().enumerate() {
        // The already bounded metadata page yields one surface-owned control per source row.
        let row_y =
            content_height - NATIVE_MARKDOWN_HEADER_HEIGHT - 106.0 - row_index as f64 * 31.0;
        let button = NativeMarkdownSidebarButton::new(
            row_index,
            row,
            NSRect::new(
                NSPoint::new(18.0, row_y),
                NSSize::new(NATIVE_MARKDOWN_SIDEBAR_WIDTH - 36.0, 22.0),
            ),
            mtm,
        );
        button.setBordered(false);
        button.setAlignment(NSTextAlignment::Left);
        button.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        let color = match row.kind {
            // The root row is the stable refresh/navigation origin.
            NativeMarkdownSidebarEntryKind::Root => &root_color,
            // Directories expose collapse and confined filesystem actions without opening document bodies.
            NativeMarkdownSidebarEntryKind::Directory => &directory_color,
            // Only the exact active relative path is highlighted across paging/search results.
            NativeMarkdownSidebarEntryKind::Note
                if selected_relative_path == Some(row.relative_path.as_str()) =>
            {
                &selected_note_color
            }
            // Inactive note rows remain selectable but own no editor resources.
            NativeMarkdownSidebarEntryKind::Note => &unselected_note_color,
        };
        button.setContentTintColor(Some(color));
        button.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
        content_view.addSubview(&button);
        buttons.push(button);
    }
    buttons
}
