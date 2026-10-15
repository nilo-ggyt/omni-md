//! Translates current tree-row intent into confined typed filesystem commands.
//! Modal results are revalidated against their originating row/controller before save/release/mutation.

use super::*;

/// Executes one confined typed filesystem command after required save/revision/release checks.
pub(super) fn execute_native_markdown_file_mutation(
    session: &MarkdownSessionAccess,
    mutation: &NativeMarkdownFileMutation,
    expected_revision: Option<&str>,
) -> Result<NativeMarkdownFileMutationResult, String> {
    match mutation {
        // Exclusive creation returns the new confined note identity for subsequent navigation.
        NativeMarkdownFileMutation::CreateNote {
            parent_directory,
            title,
        } => session
            .create_note(parent_directory, title)
            .map(|created| NativeMarkdownFileMutationResult::NoteCreated(created.relative_path))
            .map_err(|error| error.to_string()),
        // Directory creation validates its relative parent/name without allocating any note body.
        NativeMarkdownFileMutation::CreateDirectory {
            parent_directory,
            name,
        } => session
            .create_directory(parent_directory, name)
            .map(|_| NativeMarkdownFileMutationResult::DirectoryCreated)
            .map_err(|error| error.to_string()),
        // Renaming a directory returns its authoritative new relative location.
        NativeMarkdownFileMutation::RenameDirectory {
            relative_path,
            name,
        } => session
            .rename_directory(relative_path, name)
            .map(NativeMarkdownFileMutationResult::DirectoryRenamed)
            .map_err(|error| error.to_string()),
        // Directory removal obeys the core empty/confinement rules after explicit user confirmation.
        NativeMarkdownFileMutation::DeleteDirectory { relative_path } => session
            .delete_directory(relative_path)
            .map(|()| NativeMarkdownFileMutationResult::DirectoryDeleted)
            .map_err(|error| error.to_string()),
        // Note rename checks the expected revision before moving the exclusive destination.
        NativeMarkdownFileMutation::RenameNote {
            relative_path,
            title,
        } => session
            .rename_note(relative_path, title, expected_revision)
            .map(|receipt| NativeMarkdownFileMutationResult::NoteRenamed(receipt.relative_path))
            .map_err(|error| error.to_string()),
        // A move uses the validated destination folder and the same optimistic source revision.
        NativeMarkdownFileMutation::MoveNote {
            relative_path,
            destination_directory,
        } => session
            .move_note(relative_path, destination_directory, expected_revision)
            .map(|receipt| NativeMarkdownFileMutationResult::NoteMoved(receipt.relative_path))
            .map_err(|error| error.to_string()),
        // A copy creates a separate collision-safe identity and preserves the source bytes.
        NativeMarkdownFileMutation::CopyNote { relative_path } => session
            .copy_note(relative_path, expected_revision)
            .map(|receipt| NativeMarkdownFileMutationResult::NoteCopied(receipt.relative_path))
            .map_err(|error| error.to_string()),
        // Note deletion cannot discard a changed external revision after the foreground save/release.
        NativeMarkdownFileMutation::DeleteNote { relative_path } => session
            .delete_note(relative_path, expected_revision)
            .map(|()| NativeMarkdownFileMutationResult::NoteDeleted)
            .map_err(|error| error.to_string()),
    }
}

/// Snapshots allowed row and destination metadata before any modal UI can reenter lifecycle state.
pub(super) fn native_markdown_sidebar_action_context(
    row_index: usize,
    action: NativeMarkdownSidebarAction,
) -> Result<NativeMarkdownSidebarActionContext, String> {
    if CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_some()) {
        // A closing surface has relinquished row actions, even if its native buttons still survive this event.
        return Err("原生 Markdown 窗口正在关闭".to_owned());
    }
    ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active_surface| {
        let surface = active_surface
            .as_ref()
            .ok_or_else(|| "原生 Markdown 窗口已经关闭".to_owned())?;
        if native_markdown_operation_is_busy(
            surface.pending_note_switch.is_some(),
            surface.pending_file_mutation.is_some(),
        ) {
            // A pending switch/mutation already owns the document and expected filesystem revision.
            return Err("另一项 Markdown 操作仍在完成中".to_owned());
        }
        let row = native_markdown_sidebar_row_at(&surface.sidebar_rows, row_index)
            .cloned()
            .ok_or_else(|| "所选侧栏条目已经不可用".to_owned())?;
        if !native_markdown_sidebar_action_is_allowed(row.kind, action) {
            // Root, directory and note rows intentionally expose different confined capabilities.
            return Err("这个侧栏条目不支持所选操作".to_owned());
        }

        let directories = if action == NativeMarkdownSidebarAction::MoveNote {
            // This bounded chooser offers discovered destinations; its editable path field can name another confined folder.
            let current_parent = native_markdown_note_parent(&row.relative_path);
            std::iter::once(String::new())
                .chain(
                    surface
                        .sidebar_rows
                        .iter()
                        .filter(|candidate| {
                            candidate.kind == NativeMarkdownSidebarEntryKind::Directory
                        })
                        .map(|candidate| candidate.relative_path.clone()),
                )
                .filter(|directory| directory != &current_parent)
                .collect()
        } else {
            // Other operations need no destination catalog or traversal work.
            Vec::new()
        };
        Ok(NativeMarkdownSidebarActionContext {
            controller: surface.controller.clone(),
            row,
            directories,
        })
    })
}

/// Routes current row activation to note switching, directory collapse or root refresh.
pub(super) fn activate_native_markdown_sidebar_row(row_index: usize) -> Result<(), String> {
    MainThreadMarker::new()
        .ok_or_else(|| "原生 Markdown 侧栏必须在 AppKit 主线程激活".to_owned())?;
    if CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_some()) {
        // Old row clicks cannot create a new foreground transition after an accepted close.
        return Err("原生 Markdown 窗口正在关闭".to_owned());
    }
    let row = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active_surface| {
        let surface = active_surface
            .as_ref()
            .ok_or_else(|| "原生 Markdown 窗口已经关闭".to_owned())?;
        native_markdown_sidebar_row_at(&surface.sidebar_rows, row_index)
            .cloned()
            .ok_or_else(|| "所选侧栏条目已经不可用".to_owned())
    })?;
    match row.kind {
        // Selecting a note retains the existing save/release/switch contract.
        NativeMarkdownSidebarEntryKind::Note => switch_native_markdown_note(&row.relative_path)?,
        // Directory selection toggles a subtree projection and restarts its bounded traversal.
        NativeMarkdownSidebarEntryKind::Directory => {
            with_active_native_markdown_surface(|surface| {
                if !surface.collapsed_directories.remove(&row.relative_path) {
                    // First toggle hides descendants; a second removes the collapsed marker and expands them.
                    surface
                        .collapsed_directories
                        .insert(row.relative_path.clone());
                }
                let mtm =
                    MainThreadMarker::new().expect("sidebar activation runs on the main thread");
                surface.refresh_workspace_sidebar(mtm)
            })?
        }
        // The virtual root is the stable return-to-first-page navigation target.
        NativeMarkdownSidebarEntryKind::Root => {
            perform_native_markdown_workspace_action(9)?;
        }
    }
    Ok(())
}

/// Revalidates current relative identity before handing a confined path to Finder.
pub(super) fn reveal_native_markdown_sidebar_row(
    row_index: usize,
    expected_row: &NativeMarkdownSidebarRow,
) -> Result<(), String> {
    let session = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active_surface| {
        let surface = active_surface
            .as_ref()
            .ok_or_else(|| "原生 Markdown 窗口已经关闭".to_owned())?;
        let current_row = native_markdown_sidebar_row_at(&surface.sidebar_rows, row_index)
            .ok_or_else(|| "所选侧栏条目已经不可用".to_owned())?;
        if current_row != expected_row {
            // Pagination/search may reuse an index; only the originally accepted metadata identity is valid.
            return Err("侧栏已经变化，请重新选择条目".to_owned());
        }
        Ok(surface.session.clone())
    })?;
    let path = match expected_row.kind {
        // Both the virtual root and real directories must resolve through the directory confinement contract.
        NativeMarkdownSidebarEntryKind::Root | NativeMarkdownSidebarEntryKind::Directory => session
            .resolve_directory_for_system_action(&expected_row.relative_path)
            .map_err(|error| error.to_string())?,
        // A note additionally requires the core's regular Markdown-file validation before Finder receives it.
        NativeMarkdownSidebarEntryKind::Note => session
            .resolve_note_for_system_action(&expected_row.relative_path)
            .map_err(|error| error.to_string())?,
    };
    reveal_markdown_entry_in_finder(&path)
}

/// Reveals one authoritative file URL through the public system workspace service.
pub(super) fn reveal_markdown_entry_in_finder(path: &Path) -> Result<(), String> {
    let path = path
        .to_str()
        .ok_or_else(|| "Markdown 文件名无法交给 Finder".to_owned())?;

    autoreleasepool(|_| {
        let path = NSString::from_str(path);
        let url = NSURL::fileURLWithPath(&path);
        let urls = NSArray::from_retained_slice(&[url]);
        NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&urls);
    });
    Ok(())
}

/// Collects a confirmed typed row mutation without retaining a document across modal dialogs.
pub(super) fn request_native_markdown_sidebar_action(
    row_index: usize,
    action: NativeMarkdownSidebarAction,
) -> Result<(), String> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| "Markdown 文件操作必须在 AppKit 主线程执行".to_owned())?;
    let context = native_markdown_sidebar_action_context(row_index, action)?;
    if action == NativeMarkdownSidebarAction::RevealInFinder {
        // Reveal performs no document mutation or save/release handoff, but still revalidates row identity.
        return reveal_native_markdown_sidebar_row(row_index, &context.row);
    }

    let mutation = autoreleasepool(|_| {
        let row = &context.row;
        match (row.kind, action) {
            // A root/directory contributes only the relative parent; the core creates an exclusive note name.
            (
                NativeMarkdownSidebarEntryKind::Root | NativeMarkdownSidebarEntryKind::Directory,
                NativeMarkdownSidebarAction::CreateNote,
            ) => {
                let title = run_native_markdown_name_dialog(
                    "新建笔记",
                    "输入新笔记的标题。",
                    "",
                    "笔记标题",
                    "创建",
                    mtm,
                );
                Ok(title.map(|title| NativeMarkdownFileMutation::CreateNote {
                    parent_directory: row.relative_path.clone(),
                    title,
                }))
            }
            // Directory creation uses the same parent authority without opening any document body.
            (
                NativeMarkdownSidebarEntryKind::Root | NativeMarkdownSidebarEntryKind::Directory,
                NativeMarkdownSidebarAction::CreateDirectory,
            ) => {
                let name = run_native_markdown_name_dialog(
                    "新建目录",
                    "输入新目录的名称。",
                    "",
                    "目录名称",
                    "创建",
                    mtm,
                );
                Ok(
                    name.map(|name| NativeMarkdownFileMutation::CreateDirectory {
                        parent_directory: row.relative_path.clone(),
                        name,
                    }),
                )
            }
            (NativeMarkdownSidebarEntryKind::Directory, NativeMarkdownSidebarAction::Rename) => {
                // Retain the exact selected directory identity while collecting only its replacement name.
                let name = run_native_markdown_name_dialog(
                    "重命名目录",
                    "输入目录的新名称。",
                    &row.title,
                    "目录名称",
                    "重命名",
                    mtm,
                );
                Ok(
                    name.map(|name| NativeMarkdownFileMutation::RenameDirectory {
                        relative_path: row.relative_path.clone(),
                        name,
                    }),
                )
            }
            (NativeMarkdownSidebarEntryKind::Note, NativeMarkdownSidebarAction::Rename) => {
                // The note's current title is a suggestion; revision validation occurs after the modal returns.
                let title = run_native_markdown_name_dialog(
                    "重命名笔记",
                    "输入笔记的新标题。",
                    &row.title,
                    "笔记标题",
                    "重命名",
                    mtm,
                );
                Ok(title.map(|title| NativeMarkdownFileMutation::RenameNote {
                    relative_path: row.relative_path.clone(),
                    title,
                }))
            }
            (NativeMarkdownSidebarEntryKind::Note, NativeMarkdownSidebarAction::MoveNote) => {
                // The chooser accepts a confined relative directory rather than an unchecked absolute path.
                run_native_markdown_move_destination_dialog(&context.directories, mtm).map(
                    |destination_directory| {
                        destination_directory.map(|destination_directory| {
                            // A move uses the validated destination folder and the same optimistic source revision.
                            NativeMarkdownFileMutation::MoveNote {
                                relative_path: row.relative_path.clone(),
                                destination_directory,
                            }
                        })
                    },
                )
            }
            (NativeMarkdownSidebarEntryKind::Note, NativeMarkdownSidebarAction::CopyNote) => {
                // Copy preserves the source identity and lets the core choose a collision-safe destination.
                Ok(Some(NativeMarkdownFileMutation::CopyNote {
                    relative_path: row.relative_path.clone(),
                }))
            }
            (NativeMarkdownSidebarEntryKind::Directory, NativeMarkdownSidebarAction::Delete) => {
                // Explicit confirmation authorizes only this empty-directory removal, never recursive deletion.
                Ok(run_native_markdown_delete_confirmation(row, mtm).then(|| {
                    NativeMarkdownFileMutation::DeleteDirectory {
                        relative_path: row.relative_path.clone(),
                    }
                }))
            }
            (NativeMarkdownSidebarEntryKind::Note, NativeMarkdownSidebarAction::Delete) => {
                // The confirmed note is still subject to save/revision and complete editor release checks.
                Ok(run_native_markdown_delete_confirmation(row, mtm).then(|| {
                    NativeMarkdownFileMutation::DeleteNote {
                        relative_path: row.relative_path.clone(),
                    }
                }))
            }
            // Unlisted row/action pairs cannot synthesize a filesystem command from arbitrary native tags.
            _ => Err("这个侧栏条目不支持所选操作".to_owned()),
        }
    })?;
    if !native_markdown_controller_is_current(&context.controller) {
        // A dialog result cannot mutate a newly activated session with coincidentally equal row metadata.
        return Err("The Markdown session changed while the dialog was open".into());
    }
    let Some(mutation) = mutation else {
        // Cancelling a confirmation/name/destination dialog leaves vault and active editor untouched.
        return Ok(());
    };
    begin_native_markdown_file_mutation(row_index, &context.row, mutation)
}

/// Stages one typed row mutation after validating its unchanged foreground identity.
pub(super) fn begin_native_markdown_file_mutation(
    row_index: usize,
    expected_row: &NativeMarkdownSidebarRow,
    mutation: NativeMarkdownFileMutation,
) -> Result<(), String> {
    MainThreadMarker::new()
        .ok_or_else(|| "Markdown 文件操作必须在 AppKit 主线程开始".to_owned())?;
    if CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_some()) {
        // Modal reentry may have accepted close; never stage a command into its retiring surface.
        return Err("原生 Markdown 窗口正在关闭".to_owned());
    }
    let controller = autoreleasepool(|_| {
        with_active_native_markdown_surface(|surface| {
            let current_row = native_markdown_sidebar_row_at(&surface.sidebar_rows, row_index)
                .ok_or_else(|| "所选侧栏条目已经不可用".to_owned())?;
            if current_row != expected_row {
                // A modal result cannot reuse a changed page index for a different filesystem target.
                return Err("侧栏已经变化，请重新选择条目".to_owned());
            }
            surface.begin_file_mutation(mutation)?;
            Ok::<_, String>(surface.controller.clone())
        })
    })?;
    with_active_native_markdown_surface(|surface| {
        let pending = surface
            .pending_file_mutation
            .as_ref()
            .ok_or("No Markdown file mutation is pending")?;
        controller.enqueue_owned_after_release(pending.release_barrier.as_ref(), || {
            if let Err(error) = finish_native_markdown_file_mutation() {
                // The original saved authority stays recoverable; no failure schedules a hidden retry.
                eprintln!("Markdown file mutation failed: {error}");
            }
        });
        Ok(())
    })
}

/// Consumes a pending confined mutation only after its native release handoff.
pub(super) fn finish_native_markdown_file_mutation() -> Result<(), String> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| "Markdown 文件操作必须在 AppKit 主线程完成".to_owned())?;
    if CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_some()) {
        // Closing has superseded this old callback without granting it permission to mutate the vault.
        return Ok(());
    }
    autoreleasepool(|_| {
        with_active_native_markdown_surface(|surface| {
            let mutation_result = surface.finish_file_mutation(mtm);
            if let Err(error) = &mutation_result {
                // A failed confined mutation retains its recoverable foreground status and original disk data.
                surface.controller.present_error(error);
            }
            mutation_result
        })
    })
}
