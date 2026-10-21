//! Maps bounded vault metadata to sidebar rows and confined file actions.
//! Native menus borrow their target only while the surface owns that target.

use super::*;

mod list;
#[cfg(feature = "native-markdown-runtime-probe")]
pub(super) use list::verify_search_rows;
pub(super) use list::{NativeMarkdownSidebarList, NativeMarkdownSidebarTable};

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
    /// Only search results have subtitles; directory pages keep their row height and retain no document body.
    pub(super) subtitle: Option<String>,
    pub(super) depth: usize,
    pub(super) is_collapsed: bool,
}

impl NativeMarkdownSidebarRow {
    /// Consume the original page's bounded snippet, keeping title and file identity separate from truncation.
    pub(super) fn from_search_result(result: omni_markdown_core::MarkdownSearchResult) -> Self {
        let subtitle = match result.snippet {
            // Path comes first to distinguish equal titles; the original page supplies the snippet without another body read or truncation.
            Some(snippet) => format!("{} · {snippet}", result.relative_path),
            None => result.relative_path.clone(), // Title-only hits still show their actual path rather than an empty subtitle.
        };
        Self {
            kind: NativeMarkdownSidebarEntryKind::Note,
            relative_path: result.relative_path,
            title: result.title,
            subtitle: Some(subtitle),
            depth: 1,
            is_collapsed: false,
        }
    }

    /// Tooltip preserves the actual path and full bounded subtitle; display truncation changes neither file actions nor accessibility identity.
    fn search_description(&self) -> String {
        format!(
            "笔记：{}\n{}",
            self.title,
            self.subtitle.as_deref().expect("搜索行具有原副行")
        )
    }
}

/// Build the root and path-sorted metadata rows while preserving the bounded note snapshot.
pub(super) fn native_markdown_sidebar_rows(
    snapshot: MarkdownWorkspaceSnapshot,
    root_title: &str,
) -> (Vec<MarkdownNoteSummary>, Vec<NativeMarkdownSidebarRow>) {
    // The root always exists as a virtual row even when the bounded workspace contains no notes.
    let MarkdownWorkspaceSnapshot { directories, notes } = snapshot;
    let mut rows = Vec::with_capacity(directories.len() + notes.len() + 1);
    rows.push(NativeMarkdownSidebarRow {
        kind: NativeMarkdownSidebarEntryKind::Root,
        relative_path: String::new(),
        // Label the session's validated real root; a synthetic workspace must not masquerade as production OMNI.
        title: root_title.to_owned(),
        subtitle: None,
        depth: 0,
        is_collapsed: false,
    });
    rows.extend(directories.into_iter().map(|relative_path| {
        // Directory rows copy relative metadata only; no directory or document stays open.
        NativeMarkdownSidebarRow {
            kind: NativeMarkdownSidebarEntryKind::Directory,
            title: native_markdown_path_name(&relative_path),
            subtitle: None,
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
            subtitle: None,
            depth: Path::new(&note.relative_path).components().count(),
            is_collapsed: false,
        }
    }));
    // Root stays first; remaining rows must use the same tree comparator as Core's cross-page selection.
    rows[1..].sort_unstable_by(|left, right| {
        let kind = |row: &NativeMarkdownSidebarRow| match row.kind {
            // Root is outside this sorted slice; the shared kind mapping still treats it as a directory.
            NativeMarkdownSidebarEntryKind::Root | NativeMarkdownSidebarEntryKind::Directory => {
                omni_markdown_core::MarkdownWorkspaceEntryKind::Directory
            }
            NativeMarkdownSidebarEntryKind::Note => {
                omni_markdown_core::MarkdownWorkspaceEntryKind::Note
            }
        };
        omni_markdown_core::compare_markdown_workspace_entries(
            &left.relative_path,
            kind(left),
            &right.relative_path,
            kind(right),
        )
    });
    (notes, rows)
}

#[cfg(test)]
mod sorted_page_tests {
    use super::*;

    #[test]
    /// Equal titles preserve distinct original paths and full bounded snippets without concatenation-driven title truncation.
    fn search_rows_separate_title_and_subtitle_without_rewriting_file_identity() {
        let snippet = "中文😀命中摘要".repeat(12);
        for path in ["设计/同名.md", "归档/同名.md"] {
            // The same visible title must remain distinguishable by its independent confined path and snippet.
            let row = NativeMarkdownSidebarRow::from_search_result(
                omni_markdown_core::MarkdownSearchResult {
                    relative_path: path.into(),
                    title: "同名".into(),
                    snippet: Some(snippet.clone()),
                },
            );
            assert_eq!(row.relative_path, path);
            assert_eq!(row.title, "同名");
            assert_eq!(
                row.subtitle.as_deref(),
                Some(format!("{path} · {snippet}").as_str())
            );
            assert!(row.search_description().contains(path));
            assert!(row.search_description().contains(&snippet));
        }
        let row = NativeMarkdownSidebarRow::from_search_result(
            omni_markdown_core::MarkdownSearchResult {
                relative_path: "  原目录 /原题.md".into(),
                title: " 原题 ".into(),
                snippet: None,
            },
        );
        assert_eq!(row.title, " 原题 ");
        assert_eq!(row.subtitle.as_deref(), Some("  原目录 /原题.md"));
    }

    #[test]
    /// Separately transported directory/note metadata restores one tree order, with root inside the fixed 13-row budget.
    fn metadata_page_keeps_tree_order_and_counts_root_inside_visible_budget() {
        let notes = ["z.md", "a/child.md", "a/Z/leaf.md", "A.md"]
            .into_iter()
            .map(|path| MarkdownNoteSummary {
                relative_path: path.to_owned(),
                title: native_markdown_path_name(path),
                byte_count: 0,
                modified_at_ms: 0,
            })
            .collect();
        let (_, rows) = native_markdown_sidebar_rows(
            omni_markdown_core::MarkdownWorkspaceSnapshot {
                directories: vec!["a/Z".into(), "a".into()],
                notes,
            },
            "OMNI",
        );
        assert_eq!(
            rows.iter()
                .map(|row| row.relative_path.as_str())
                .collect::<Vec<_>>(),
            ["", "a", "a/Z", "a/Z/leaf.md", "a/child.md", "A.md", "z.md"]
        );
        let notes = (0..12)
            .map(|index| MarkdownNoteSummary {
                relative_path: format!("note-{index:02}.md"),
                title: format!("note-{index:02}"),
                byte_count: 0,
                modified_at_ms: 0,
            })
            .collect();
        let (_, full) = native_markdown_sidebar_rows(
            omni_markdown_core::MarkdownWorkspaceSnapshot {
                directories: Vec::new(),
                notes,
            },
            "OMNI",
        );
        assert_eq!(full.len(), sidebar_geometry::MINIMUM_PAGE_ROWS);
        assert_eq!(
            full.first().unwrap().kind,
            NativeMarkdownSidebarEntryKind::Root
        );
    }

    #[test]
    /// Browse selection follows successful directory receipts, independent of open body or current-page membership.
    fn successful_directory_receipts_preserve_selected_subtree_and_unrelated_browse_directory() {
        let created = NativeMarkdownFileMutation::CreateDirectory {
            parent_directory: "B".into(),
            name: "新目录".into(),
        };
        assert_eq!(
            native_markdown_directory_after_mutation(
                "B",
                &created,
                &NativeMarkdownFileMutationResult::DirectoryCreated("B/新目录".into()),
            ),
            Some("B/新目录".into()),
        );
        let renamed = NativeMarkdownFileMutation::RenameDirectory {
            relative_path: "B".into(),
            name: "新位置".into(),
        };
        let receipt = NativeMarkdownFileMutationResult::DirectoryRenamed("新位置".into());
        assert_eq!(
            native_markdown_directory_after_mutation("B", &renamed, &receipt),
            Some("新位置".into()),
        );
        assert_eq!(
            native_markdown_directory_after_mutation("B/子目录", &renamed, &receipt),
            Some("新位置/子目录".into()),
        );
        assert_eq!(
            native_markdown_directory_after_mutation("Backup/子目录", &renamed, &receipt),
            None,
        );
        // Mutating another open note must preserve independent browse directory B; opening its successor separately chooses the parent.
        assert_eq!(
            native_markdown_directory_after_mutation(
                "B",
                &NativeMarkdownFileMutation::RenameNote {
                    relative_path: "A/原.md".into(),
                    title: "新".into()
                },
                &NativeMarkdownFileMutationResult::NoteRenamed("A/新.md".into()),
            ),
            None,
        );
    }

    #[test]
    /// Deleted selected subtrees return to their actual parent; equal-prefix siblings and mismatched receipts keep selection intact.
    fn directory_deletion_uses_real_parent_and_does_not_accept_a_mismatched_receipt() {
        let deleted = NativeMarkdownFileMutation::DeleteDirectory {
            relative_path: "B/子目录".into(),
        };
        assert_eq!(
            native_markdown_directory_after_mutation(
                "B/子目录/后代",
                &deleted,
                &NativeMarkdownFileMutationResult::DirectoryDeleted
            ),
            Some("B".into()),
        );
        assert_eq!(
            native_markdown_directory_after_mutation(
                "B/子目录备份",
                &deleted,
                &NativeMarkdownFileMutationResult::DirectoryDeleted
            ),
            None,
        );
        assert_eq!(
            native_markdown_directory_after_mutation(
                "B/子目录",
                &deleted,
                &NativeMarkdownFileMutationResult::DirectoryRenamed("错误来源".into())
            ),
            None,
        );
    }
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
    if suffix.as_os_str().is_empty() {
        // The directory itself has no descendant suffix; preserve the exact receipt rather than joining an empty path and adding a slash.
        return Some(new_directory.to_owned());
    }
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
/// Actual row-menu and directory-header actions share the existing filesystem workflow and Core constraints.
pub(super) enum NativeMarkdownSidebarAction {
    CreateNote,
    CreateDirectory,
    ImportNote,
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
/// Successful Core receipts carry committed paths for body restoration and independent browse-directory selection.
pub(super) enum NativeMarkdownFileMutationResult {
    NoteCreated(String),
    DirectoryCreated(String),
    DirectoryRenamed(String),
    DirectoryDeleted,
    NoteRenamed(String),
    NoteMoved(String),
    NoteCopied(String),
    NoteDeleted,
}

/// Browse directory is independent of the open body; only a successful directory receipt can move this selection.
fn native_markdown_directory_after_mutation(
    selected_directory: &str,
    mutation: &NativeMarkdownFileMutation,
    result: &NativeMarkdownFileMutationResult,
) -> Option<String> {
    match (mutation, result) {
        // New directories use Core's actual returned identity, never a path assembled from an unconfirmed name.
        (
            NativeMarkdownFileMutation::CreateDirectory { .. },
            NativeMarkdownFileMutationResult::DirectoryCreated(path),
        ) => Some(path.clone()),
        // Rename rebases only selection inside the renamed subtree, preserving its actual descendant suffix.
        (
            NativeMarkdownFileMutation::RenameDirectory { relative_path, .. },
            NativeMarkdownFileMutationResult::DirectoryRenamed(path),
        ) if native_markdown_path_is_inside_directory(selected_directory, relative_path) => {
            native_markdown_rewrite_directory_prefix(selected_directory, relative_path, path)
        }
        // Successful deletion returns the selected subtree to its real parent, without treating an invalid path as a root fallback.
        (
            NativeMarkdownFileMutation::DeleteDirectory { relative_path },
            NativeMarkdownFileMutationResult::DirectoryDeleted,
        ) if native_markdown_path_is_inside_directory(selected_directory, relative_path) => {
            Some(native_markdown_note_parent(relative_path))
        }
        // Note/unrelated-directory operations preserve independent browse selection; successful body opening separately chooses its real parent.
        _ => None,
    }
}

#[derive(Clone)]
/// Context rows bind their page while headers bind independent directory selection; both share the same list/action boundary.
pub(super) enum NativeMarkdownSidebarActionOrigin {
    Row {
        row_index: usize,
        revision: u64,
    },
    SelectedDirectory {
        relative_path: String,
        selection_revision: u64,
    },
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
    pub(super) row: Option<NativeMarkdownSidebarRow>,
    pub(super) directories: Vec<String>,
}

/// Give cancellation the standard Escape key independently of focused accessory controls.
pub(super) fn assign_escape_to_native_markdown_alert_cancel(cancel_button: &NSButton) {
    // No modifier is required, so Escape works while editing a name or selecting a destination.
    cancel_button.setKeyEquivalent(ns_string!("\u{1b}"));
    cancel_button.setKeyEquivalentModifierMask(NSEventModifierFlags::empty());
}

/// Name dialogs return only explicitly confirmed bounded candidates, distinguishing cancellation from resource failure.
pub(super) fn run_native_markdown_name_dialog(
    controller: &NativeMarkdownController,
    message: &str,
    informative_text: &str,
    initial_value: &str,
    placeholder: &str,
    action_title: &str,
    mtm: MainThreadMarker,
) -> Result<Option<String>, NativeMarkdownModalError> {
    let guard = NativeMarkdownModalUseGuard::enter(controller)?;
    let result = autoreleasepool(|_| {
        // Register every owned role before installing candidate text; acquisition failure never presents a partially built modal.
        let alert = NSAlert::new(mtm);
        alert.setAlertStyle(NSAlertStyle::Informational);
        alert.addButtonWithTitle(&NSString::from_str(action_title));
        let cancel = alert.addButtonWithTitle(ns_string!("取消"));
        assign_escape_to_native_markdown_alert_cancel(&cancel);
        let window = register_native_markdown_modal_alert(&guard, &alert)?;
        let input = NSTextField::initWithFrame(
            NSTextField::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(360.0, 24.0)),
        );
        let owner = install_native_markdown_modal_name_input(&guard, &input, &window, mtm)?;

        // Owned cells retain native input/IME; the exclusive provider weakly borrows only this dialog's history and ledger.
        alert.setMessageText(&NSString::from_str(message));
        alert.setInformativeText(&NSString::from_str(informative_text));
        input.setEditable(true);
        input.setSelectable(true);
        input.setStringValue(&NSString::from_str(initial_value));
        input.setPlaceholderString(Some(&NSString::from_str(placeholder)));
        alert.setAccessoryView(Some(&input));
        window.setInitialFirstResponder(Some(&input));
        let response = run_native_markdown_modal_input_response(&alert, &window, &input, &owner);
        let candidate = response.and_then(|confirmed| {
            if confirmed {
                // Copy only after confirmation and normal resignation; Core still validates the final name/path.
                native_markdown_modal_candidate(&input).map(Some)
            } else {
                // Cancellation neither reads the field body nor authorizes filesystem mutation.
                Ok(None)
            }
        });
        let cleanup = finish_native_markdown_modal_input(&guard, &owner, &window, &input);
        detach_native_markdown_modal_alert(&alert, &window);
        // Cleanup failure blocks successor work; the candidate owns text only and carries no native borrow.
        cleanup.and(candidate)
    });
    // The guard outlives all local Retained claims and the pool, then rechecks public borrowed-resource facts.
    guard.complete(result)
}

/// Editable destination suggestions come from the bounded page; Core still checks confinement and revision on execution.
pub(super) fn run_native_markdown_move_destination_dialog(
    controller: &NativeMarkdownController,
    destination_directories: &[String],
    mtm: MainThreadMarker,
) -> Result<Option<String>, NativeMarkdownModalError> {
    let guard = NativeMarkdownModalUseGuard::enter(controller)?;
    let result = autoreleasepool(|_| {
        let alert = NSAlert::new(mtm);
        alert.setAlertStyle(NSAlertStyle::Informational);
        alert.addButtonWithTitle(ns_string!("移动"));
        assign_escape_to_native_markdown_alert_cancel(
            &alert.addButtonWithTitle(ns_string!("取消")),
        );
        let window = register_native_markdown_modal_alert(&guard, &alert)?;
        let destinations = objc2_app_kit::NSComboBox::initWithFrame(
            objc2_app_kit::NSComboBox::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(420.0, 28.0)),
        );
        let owner = install_native_markdown_modal_move_input(&guard, &destinations, &window, mtm)?;

        // Copy suggestions from this sidebar page only, without a vault scan or replacing path authority with translated labels.
        alert.setMessageText(ns_string!("移动笔记"));
        alert.setInformativeText(ns_string!(
            "选择建议文件夹，或输入相对于笔记库的完整路径。留空表示笔记库根目录。"
        ));
        destinations.setEditable(true);
        destinations.setSelectable(true);
        destinations.setCompletes(true);
        destinations.setPlaceholderString(Some(ns_string!("留空选择根目录，或输入相对文件夹路径")));
        destinations.setAccessibilityLabel(Some(ns_string!("目标文件夹，相对于笔记库")));
        for directory in destination_directories {
            if !directory.is_empty() {
                // Empty input represents root; nonempty suggestions keep real relative paths without a hidden whole-directory cache.
                let value = NSString::from_str(directory);
                // SAFETY: Add a live NSString to this main-thread owned combo, preserving AppKit's standard item ownership.
                unsafe { destinations.addItemWithObjectValue(&value) };
            }
        }
        alert.setAccessoryView(Some(&destinations));
        window.setInitialFirstResponder(Some(&destinations));
        let response =
            run_native_markdown_modal_input_response(&alert, &window, &destinations, &owner);
        let candidate = response.and_then(|confirmed| {
            if confirmed {
                // Empty candidate validly names root; the UI never guesses existence or bypasses symlink checks for another path.
                native_markdown_modal_candidate(&destinations).map(Some)
            } else {
                // Even cancellation during invalid input creates no move command.
                Ok(None)
            }
        });
        let cleanup = finish_native_markdown_modal_input(&guard, &owner, &window, &destinations);
        // Clear only installed suggestions/completion, without touching global Find or clipboard state.
        destinations.setCompletes(false);
        destinations.removeAllItems();
        detach_native_markdown_modal_alert(&alert, &window);
        cleanup.and(candidate)
    });
    guard.complete(result)
}

/// Deletion requires explicit confirmation; resource errors cannot collapse into boolean cancellation, and root remains protected.
pub(super) fn run_native_markdown_delete_confirmation(
    controller: &NativeMarkdownController,
    row: &NativeMarkdownSidebarRow,
    mtm: MainThreadMarker,
) -> Result<bool, NativeMarkdownModalError> {
    let (message, informative_text) = match row.kind {
        NativeMarkdownSidebarEntryKind::Directory => {
            // Match Core's recursive deletion semantics by disclosing loss of the entire confined subtree.
            (
                format!("删除目录“{}”？", row.title),
                "目录中的全部笔记和子目录都会被删除，此操作无法撤销。",
            )
        }
        NativeMarkdownSidebarEntryKind::Note => {
            // Note confirmation authorizes this copied identity only; callers still recheck row/revision afterward.
            (
                format!("删除笔记“{}”？", row.title),
                "这篇笔记会从 OMNI 中删除，此操作无法撤销。",
            )
        }
        NativeMarkdownSidebarEntryKind::Root => {
            // Virtual root never creates a destructive confirmation or enters Core deletion.
            return Ok(false);
        }
    };
    let guard = NativeMarkdownModalUseGuard::enter(controller)?;
    let result = autoreleasepool(|_| {
        let alert = NSAlert::new(mtm);
        alert.setAlertStyle(NSAlertStyle::Warning);
        alert.addButtonWithTitle(ns_string!("删除"));
        assign_escape_to_native_markdown_alert_cancel(
            &alert.addButtonWithTitle(ns_string!("取消")),
        );
        let window = register_native_markdown_modal_alert(&guard, &alert)?;
        alert.setMessageText(&NSString::from_str(&message));
        alert.setInformativeText(&NSString::from_str(informative_text));
        let confirmed = native_markdown_alert_response(alert.runModal(), true);
        detach_native_markdown_modal_alert(&alert, &window);
        confirmed
    });
    guard.complete(result)
}

/// Targets fix the original page/row; reused row views cannot change the file identity of an already open menu.
pub(super) struct NativeMarkdownSidebarMenuTargetIvars {
    pub(super) row_index: usize,
    pub(super) revision: u64,
    pub(super) list: Weak<list::NativeMarkdownSidebarTable>,
}

define_class!(
    /// Temporary menus retain only original-list weak identity; actions still enter the existing workspace workflow.
    // SAFETY: This main-thread NSObject implements only the fixed action selectors below, accepting no arbitrary method names.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = NativeMarkdownSidebarMenuTargetIvars]
    pub(super) struct NativeMarkdownSidebarMenuTarget;

    impl NativeMarkdownSidebarMenuTarget {
        #[unsafe(method(createMarkdownNoteFromSidebar:))]
        fn create_markdown_note(&self, _sender: &AnyObject) {
            // Existing file policy validates root/directory scope before creation; the menu never opens paths itself.
            self.enqueue_action(NativeMarkdownSidebarAction::CreateNote);
        }

        #[unsafe(method(createMarkdownDirectoryFromSidebar:))]
        fn create_markdown_directory(&self, _sender: &AnyObject) {
            // Directory creation reuses the deferred mutation boundary, without changing the page on the menu-tracking stack.
            self.enqueue_action(NativeMarkdownSidebarAction::CreateDirectory);
        }

        #[unsafe(method(renameMarkdownEntryFromSidebar:))]
        fn rename_markdown_entry(&self, _sender: &AnyObject) {
            // Original page identity distinguishes note/directory rename at execution, without constructing paths from display text.
            self.enqueue_action(NativeMarkdownSidebarAction::Rename);
        }

        #[unsafe(method(moveMarkdownNoteFromSidebar:))]
        fn move_markdown_note(&self, _sender: &AnyObject) {
            // Move retains the confined destination workflow, without another filesystem entry point.
            self.enqueue_action(NativeMarkdownSidebarAction::MoveNote);
        }

        #[unsafe(method(copyMarkdownNoteFromSidebar:))]
        fn copy_markdown_note(&self, _sender: &AnyObject) {
            // Copy keeps collision-safe publication; the menu itself allocates no second document.
            self.enqueue_action(NativeMarkdownSidebarAction::CopyNote);
        }

        #[unsafe(method(revealMarkdownEntryFromSidebar:))]
        fn reveal_markdown_entry(&self, _sender: &AnyObject) {
            // Finder receives only the revalidated path, without acquiring document-body ownership.
            self.enqueue_action(NativeMarkdownSidebarAction::RevealInFinder);
        }

        #[unsafe(method(deleteMarkdownEntryFromSidebar:))]
        fn delete_markdown_entry(&self, _sender: &AnyObject) {
            // Delete still requires confirmation and save/revision checks; list refactoring cannot bypass them.
            self.enqueue_action(NativeMarkdownSidebarAction::Delete);
        }
    }
);

impl NativeMarkdownSidebarMenuTarget {
    /// Create one target per context event; NSMenuItem.representedObject retains it throughout menu tracking.
    pub(super) fn new(
        row_index: usize,
        revision: u64,
        list: &list::NativeMarkdownSidebarTable,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let target = Self::alloc(mtm).set_ivars(NativeMarkdownSidebarMenuTargetIvars {
            row_index,
            revision,
            list: Weak::new(list),
        });
        // SAFETY: All ivars are initialized; NSObject's designated initializer consumes this allocation without another owner.
        unsafe { msg_send![super(target), init] }
    }

    /// Execute original row/action after the menu callback returns; page identity must agree before and after queuing.
    pub(super) fn enqueue_action(&self, action: NativeMarkdownSidebarAction) {
        enqueue_native_markdown_sidebar_action(
            self.ivars().list.clone(),
            NativeMarkdownSidebarActionOrigin::Row {
                row_index: self.ivars().row_index,
                revision: self.ivars().revision,
            },
            action,
            self.mtm(),
        );
    }
}

/// Header/context menus queue the same original origin without keeping the list, body, or workspace alive.
fn enqueue_native_markdown_sidebar_action(
    list: Weak<list::NativeMarkdownSidebarTable>,
    origin: NativeMarkdownSidebarActionOrigin,
    action: NativeMarkdownSidebarAction,
    mtm: MainThreadMarker,
) {
    let weak = MainThreadBound::new(list, mtm);
    enqueue_native_markdown_main_task(move || {
        let mtm = MainThreadMarker::new().expect("侧栏文件动作在AppKit主队列执行");
        let Some(list) = weak.get(mtm).load() else {
            return; // A released original list cannot redirect this action to a successor session's equally named directory.
        };
        if !list.permits_origin(&origin) {
            return; // Changed page/directory or closed session revokes original authority, without acquiring its successor's intent.
        }
        if let Err(error) = request_native_markdown_sidebar_action(&list, &origin, action) {
            // Report the existing file workflow's actual failure, without replaying or inferring unchanged disk state.
            present_native_markdown_error(&error);
            eprintln!("Markdown file action failed: {error}");
        }
    });
}

/// Build allowed actions for this context event only, retaining no idle menu after tracking ends.
pub(super) fn native_markdown_sidebar_menu(
    target: &NativeMarkdownSidebarMenuTarget,
    kind: NativeMarkdownSidebarEntryKind,
    mtm: MainThreadMarker,
) -> Retained<NSMenu> {
    // AppKit tracking/local scope owns this menu, while its items explicitly retain the temporary action target.
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!(""));
    match kind {
        // Root permits creation and Finder reveal, never renaming or deleting the entire workspace.
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
                "在访达中显示",
                sel!(revealMarkdownEntryFromSidebar:),
                mtm,
            );
        }
        // Directory actions stay within the original subtree; destructive operations retain the existing confirmation boundary.
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
                "在访达中显示",
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
        // Note actions target that file only, without fabricated child-directory operations beneath a note.
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
                "在访达中显示",
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

/// Bind only actions declared here and explicitly retain their one-shot target during menu tracking.
pub(super) fn add_native_markdown_menu_item(
    menu: &NSMenu,
    target: &NativeMarkdownSidebarMenuTarget,
    title: &str,
    action: objc2::runtime::Sel,
    mtm: MainThreadMarker,
) {
    // SAFETY: Action comes from the fixed NSMenuItem-compatible selectors above; a menu item retains the main-thread target.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            ns_string!(""),
        )
    };
    // SAFETY: Target implements the selector; representedObject releases with the item and has no reverse menu retain cycle.
    unsafe {
        item.setTarget(Some(target));
        // Action target is weak; the item separately owns this temporary target and returns it when tracking releases the menu.
        item.setRepresentedObject(Some(target));
    }
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

/// Distinguish search/directory headings; the existing refresh action returns to directory without constructing new controls.
impl NativeMarkdownWindowSurface {
    /// Publish labels only for the same captured query intent, rechecking ownership after the native title setter.
    pub(super) fn update_sidebar_mode_label(&self, searching: bool, intent: u64) {
        if !self.workspace_refresh_intent_is_current(intent) {
            // A replacement query or close revokes old results' authority to publish labels.
            return;
        }
        self.sidebar_title.setStringValue(if searching {
            // Distinguish bounded search results from the directory so the page cannot imply a complete note inventory.
            ns_string!("搜索结果")
        } else {
            // Directory keeps its paging/collapse contract; mode is not inferred from the displayed label.
            ns_string!("笔记目录")
        });
        if !self.workspace_refresh_intent_is_current(intent) {
            // Title setter can reenter; old mode cannot then change a replacement query's return button.
            return;
        }
        if let Some(button) = self
            .controller
            .ivars()
            .workspace_controls
            .get()
            .and_then(|controls| controls.get(2))
            .and_then(|control| control.downcast_ref::<NSButton>())
        {
            button.setTitle(if searching {
                // Reuse refresh to cancel the query and return to directory, without another navigation workflow.
                ns_string!("返回目录")
            } else {
                // Directory mode preserves its explicit refresh entry.
                ns_string!("刷新目录")
            });
        }
    }
}
