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
    /// 仅搜索结果具有副行；纯目录页不增加行高，也不持有任何笔记正文。
    pub(super) subtitle: Option<String>,
    pub(super) depth: usize,
    pub(super) is_collapsed: bool,
}

impl NativeMarkdownSidebarRow {
    /// 仅消费原搜索页已经提供的有限摘要；标题与文件身份不再拼进同一个截断字段。
    pub(super) fn from_search_result(result: omni_markdown_core::MarkdownSearchResult) -> Self {
        let subtitle = match result.snippet {
            // 原路径先呈现，重名标题仍可辨别目录；摘要只来自原页，不再读正文或自行截短。
            Some(snippet) => format!("{} · {snippet}", result.relative_path),
            None => result.relative_path.clone(), // 仅标题命中时仍保原路径，不能显示一条空副行。
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

    /// Tooltip保留原路径与完整的有界副行；显示截断不会改变文件动作或无障碍身份。
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
        // 根标签来自本会话已验证的实际目录，不能把合成库也伪装成正式 OMNI。
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
    // 根行永远为首；其余行必须与Core跨页选择使用完全相同的树比较器。
    rows[1..].sort_unstable_by(|left, right| {
        let kind = |row: &NativeMarkdownSidebarRow| match row.kind {
            // 根不在被排序的slice内；防御性映射为目录。
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
    /// 重名结果保留各自原路径与完整有界摘要；标题不再受摘要拼接和60scalar截短影响。
    fn search_rows_separate_title_and_subtitle_without_rewriting_file_identity() {
        let snippet = "中文😀命中摘要".repeat(12);
        for path in ["设计/同名.md", "归档/同名.md"] {
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
    /// 分组运输的目录/笔记必须还原同一树序；根行计入固定13行而非额外溢出。
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
    /// 目录选择只跟随真实成功收据；未打开正文和不在当前页都不改变路径含义。
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
        // 正文另有笔记的操作不应覆盖独立的B目录；实际打开新正文才另行选择其父目录。
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
    /// 删除选择所在子树后只回该子树真实父目录；同名前缀的兄弟和不匹配收据不迁移。
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
        // 目录自身没有后代后缀，直接保留真实收据原串；join空路径会产生多余尾部斜杠。
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
/// 原行菜单与目录头部的实际动作，仍由同一文件流程执行Core约束。
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
/// Core真实成功收据携带已提交路径，供正文恢复和独立目录选择使用。
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

/// 目录浏览与打开的正文独立；只有实际成功的目录收据能迁移这一选择。
fn native_markdown_directory_after_mutation(
    selected_directory: &str,
    mutation: &NativeMarkdownFileMutation,
    result: &NativeMarkdownFileMutationResult,
) -> Option<String> {
    match (mutation, result) {
        // 新目录使用Core返回的真实身份，不能由尚未确认的名称预先拼接。
        (
            NativeMarkdownFileMutation::CreateDirectory { .. },
            NativeMarkdownFileMutationResult::DirectoryCreated(path),
        ) => Some(path.clone()),
        // 重命名只迁移被改名子树中的当前选择，保留实际后代相对位置。
        (
            NativeMarkdownFileMutation::RenameDirectory { relative_path, .. },
            NativeMarkdownFileMutationResult::DirectoryRenamed(path),
        ) if native_markdown_path_is_inside_directory(selected_directory, relative_path) => {
            native_markdown_rewrite_directory_prefix(selected_directory, relative_path, path)
        }
        // 删除选中子树后回到其真实父目录；这是成功删除的结果，不是失效路径的根目录兜底。
        (
            NativeMarkdownFileMutation::DeleteDirectory { relative_path },
            NativeMarkdownFileMutationResult::DirectoryDeleted,
        ) if native_markdown_path_is_inside_directory(selected_directory, relative_path) => {
            Some(native_markdown_note_parent(relative_path))
        }
        // 笔记和无关目录操作不改变独立浏览选择；成功打开正文另行选择其真实父目录。
        _ => None,
    }
}

#[derive(Clone)]
/// 原右键行绑定页，头部绑定独立目录选择；两种来源共用同一列表和文件动作入口。
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

/// 名称对话框只返回明确确认的有界候选；取消与资源失败分别表达。
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
        // 全部显式角色先登记，再安装用户候选；失败不展示部分构造的modal。
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

        // 自有cell保留标准输入/IME，独占provider只弱借用本dialog历史与ledger。
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
                // 只有用户确认且正常辞任成功才复制候选，core仍负责最终名称/路径验证。
                native_markdown_modal_candidate(&input).map(Some)
            } else {
                // 取消不读取字段正文，更不授权文件系统mutation。
                Ok(None)
            }
        });
        let cleanup = finish_native_markdown_modal_input(&guard, &owner, &window, &input);
        detach_native_markdown_modal_alert(&alert, &window);
        // 只有正常辞任通过才能到达这里；资源错误阻止后继，候选不持有native借用。
        cleanup.and(candidate)
    });
    // guard晚于全部局部Retained与pool退出；末尾再复核borrowed公开事实。
    guard.complete(result)
}

/// 可编辑目的目录保留有界页面建议；输入结果仍由core重复confinement/revision校验。
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

        // 建议只复制当前sidebar页面，不扫描完整vault，也不以翻译标签替代路径authority。
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
                // 根目录以空字段表达；非空建议保持实际相对路径，不生成隐藏的全目录缓存。
                let value = NSString::from_str(directory);
                // SAFETY: 只向主线程自有combo添加活NSString，保留AppKit标准候选所有权。
                unsafe { destinations.addItemWithObjectValue(&value) };
            }
        }
        alert.setAccessoryView(Some(&destinations));
        window.setInitialFirstResponder(Some(&destinations));
        let response =
            run_native_markdown_modal_input_response(&alert, &window, &destinations, &owner);
        let candidate = response.and_then(|confirmed| {
            if confirmed {
                // 空候选合法表示vault root；其他候选不在UI层猜测存在性或绕过symlink检查。
                native_markdown_modal_candidate(&destinations).map(Some)
            } else {
                // 取消即使发生在无效输入中也不创建任何移动命令。
                Ok(None)
            }
        });
        let cleanup = finish_native_markdown_modal_input(&guard, &owner, &window, &destinations);
        // 只清自己安装的候选和completion配置，不触碰系统全局Find/clipboard状态。
        destinations.setCompletes(false);
        destinations.removeAllItems();
        detach_native_markdown_modal_alert(&alert, &window);
        cleanup.and(candidate)
    });
    guard.complete(result)
}

/// 删除必须显式确认；资源错误不能压成bool取消，根目录仍无破坏权限。
pub(super) fn run_native_markdown_delete_confirmation(
    controller: &NativeMarkdownController,
    row: &NativeMarkdownSidebarRow,
    mtm: MainThreadMarker,
) -> Result<bool, NativeMarkdownModalError> {
    let (message, informative_text) = match row.kind {
        NativeMarkdownSidebarEntryKind::Directory => {
            // 与core现有递归删除语义一致，警告必须披露整个受限子树的损失。
            (
                format!("删除目录“{}”？", row.title),
                "目录中的全部笔记和子目录都会被删除，此操作无法撤销。",
            )
        }
        NativeMarkdownSidebarEntryKind::Note => {
            // 单笔记确认只授权这个已复制的身份，返回后caller仍需重核row/revision。
            (
                format!("删除笔记“{}”？", row.title),
                "这篇笔记会从 OMNI 中删除，此操作无法撤销。",
            )
        }
        NativeMarkdownSidebarEntryKind::Root => {
            // 虚拟根从不分配破坏性确认框，更不进入core删除接口。
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

/// 菜单目标固定原页与行号；列表重用行视图不会改变已打开菜单的文件身份。
pub(super) struct NativeMarkdownSidebarMenuTargetIvars {
    pub(super) row_index: usize,
    pub(super) revision: u64,
    pub(super) list: Weak<list::NativeMarkdownSidebarTable>,
}

define_class!(
    /// 临时菜单只持原列表的弱身份；所有文件动作仍进入既有工作区状态机。
    // SAFETY: 主线程NSObject仅实现下方固定action selector，不接收任意方法名。
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = NativeMarkdownSidebarMenuTargetIvars]
    pub(super) struct NativeMarkdownSidebarMenuTarget;

    impl NativeMarkdownSidebarMenuTarget {
        #[unsafe(method(createMarkdownNoteFromSidebar:))]
        fn create_markdown_note(&self, _sender: &AnyObject) {
            // 仍由原文件策略验证根/目录范围，再创建笔记；菜单不自行打开路径。
            self.enqueue_action(NativeMarkdownSidebarAction::CreateNote);
        }

        #[unsafe(method(createMarkdownDirectoryFromSidebar:))]
        fn create_markdown_directory(&self, _sender: &AnyObject) {
            // 新建目录复用同一延迟修改边界，不在菜单跟踪栈里改变目录页。
            self.enqueue_action(NativeMarkdownSidebarAction::CreateDirectory);
        }

        #[unsafe(method(renameMarkdownEntryFromSidebar:))]
        fn rename_markdown_entry(&self, _sender: &AnyObject) {
            // 执行时仍从原页身份区分笔记/目录重命名，不以显示文字拼路径。
            self.enqueue_action(NativeMarkdownSidebarAction::Rename);
        }

        #[unsafe(method(moveMarkdownNoteFromSidebar:))]
        fn move_markdown_note(&self, _sender: &AnyObject) {
            // 移动沿原受限目标选择流程，不增加文件系统旁路。
            self.enqueue_action(NativeMarkdownSidebarAction::MoveNote);
        }

        #[unsafe(method(copyMarkdownNoteFromSidebar:))]
        fn copy_markdown_note(&self, _sender: &AnyObject) {
            // 副本沿原不覆盖提交规则，菜单本身不分配第二份文档。
            self.enqueue_action(NativeMarkdownSidebarAction::CopyNote);
        }

        #[unsafe(method(revealMarkdownEntryFromSidebar:))]
        fn reveal_markdown_entry(&self, _sender: &AnyObject) {
            // Finder只接收原动作重新验证的路径，不获得正文所有权。
            self.enqueue_action(NativeMarkdownSidebarAction::RevealInFinder);
        }

        #[unsafe(method(deleteMarkdownEntryFromSidebar:))]
        fn delete_markdown_entry(&self, _sender: &AnyObject) {
            // 删除仍要求原确认与保存/版本检查，不因列表重构跳过确认。
            self.enqueue_action(NativeMarkdownSidebarAction::Delete);
        }
    }
);

impl NativeMarkdownSidebarMenuTarget {
    /// 每个右键事件构造一次目标；NSMenuItem的representedObject在跟踪期间明确持有它。
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
        // SAFETY: ivars已经完整初始化；NSObject designated initializer不引入另一所有者。
        unsafe { msg_send![super(target), init] }
    }

    /// 菜单回调返回后再执行原行/动作；页身份在排队前后都必须一致。
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

/// 头部按钮和原右键菜单只排同一份原来源；排队不保活列表、正文或笔记库。
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
            return; // 原列表已释放，不能转投后继会话的同名目录。
        };
        if !list.permits_origin(&origin) {
            return; // 原行换页、目录选择改变或会话关闭撤销对应来源，不领取后继权限。
        }
        if let Err(error) = request_native_markdown_sidebar_action(&list, &origin, action) {
            // 只显示既有文件流程的真实失败，不重放操作或推断磁盘尚未改变。
            present_native_markdown_error(&error);
            eprintln!("Markdown file action failed: {error}");
        }
    });
}

/// 右键事件临时构造对应条目允许的动作；菜单结束后不保留一份闲置菜单。
pub(super) fn native_markdown_sidebar_menu(
    target: &NativeMarkdownSidebarMenuTarget,
    kind: NativeMarkdownSidebarEntryKind,
    mtm: MainThreadMarker,
) -> Retained<NSMenu> {
    // 本次菜单只由AppKit跟踪和局部栈持有，动作目标由菜单项明确保活。
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!(""));
    match kind {
        // 根行只允许新建和Finder显示，不能重命名或删除整个笔记库。
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
        // 目录动作限制在原子树内，破坏性操作仍通过原确认边界。
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
        // 笔记动作只作用于该文件，不向笔记伪装出子目录操作。
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

/// 菜单项只绑定本模块已声明的动作，跟踪期间明确保活一次性目标。
pub(super) fn add_native_markdown_menu_item(
    menu: &NSMenu,
    target: &NativeMarkdownSidebarMenuTarget,
    title: &str,
    action: objc2::runtime::Sel,
    mtm: MainThreadMarker,
) {
    // SAFETY: action只来自上方固定NSMenuItem兼容selector，target在主线程构造并由菜单项保活。
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            ns_string!(""),
        )
    };
    // SAFETY: target实现固定selector；representedObject的强引用随原菜单项释放，没有回指菜单的环。
    unsafe {
        item.setTarget(Some(target));
        // action target本身是弱引用；菜单项明确持有本次临时目标，菜单结束即一起归还。
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

/// 搜索结果和目录有明确分区标题；刷新按钮沿既有动作返回目录，不构造新控件。
impl NativeMarkdownWindowSurface {
    pub(super) fn update_sidebar_mode_label(&self, searching: bool, intent: u64) {
        if !self.workspace_refresh_intent_is_current(intent) {
            // 新查询/关闭撤销旧结果的文字发布权限。
            return;
        }
        self.sidebar_title.setStringValue(if searching {
            // 搜索页明确区别于当前目录，避免用户把有限结果误当全部笔记。
            ns_string!("搜索结果")
        } else {
            // 正常目录沿原分页/折叠合同呈现，不按标题文字推断模式。
            ns_string!("笔记目录")
        });
        if !self.workspace_refresh_intent_is_current(intent) {
            // 标题setter可重入，不能随后以旧模式更改新查询的返回按钮。
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
                // 复用原刷新动作取消查询并回目录，不增加第二套导航。
                ns_string!("返回目录")
            } else {
                // 目录模式保留显式刷新入口。
                ns_string!("刷新目录")
            });
        }
    }
}
