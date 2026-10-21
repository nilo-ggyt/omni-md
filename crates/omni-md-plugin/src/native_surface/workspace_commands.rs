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
        // Creation returns only the confined mutation receipt; navigation opens the
        // paged document separately instead of receiving a second owned text model.
        NativeMarkdownFileMutation::CreateNote {
            parent_directory,
            title,
        } => session
            .create_note(parent_directory, title)
            .map(|receipt| NativeMarkdownFileMutationResult::NoteCreated(receipt.relative_path))
            .map_err(|error| error.to_string()),
        // Directory creation validates its relative parent/name without allocating any note body.
        NativeMarkdownFileMutation::CreateDirectory {
            parent_directory,
            name,
        } => session
            .create_directory(parent_directory, name)
            .map(NativeMarkdownFileMutationResult::DirectoryCreated)
            .map_err(|error| error.to_string()),
        // Renaming a directory returns its authoritative new relative location.
        NativeMarkdownFileMutation::RenameDirectory {
            relative_path,
            name,
        } => session
            .rename_directory(relative_path, name)
            .map(NativeMarkdownFileMutationResult::DirectoryRenamed)
            .map_err(|error| error.to_string()),
        // Directory removal obeys Core confinement and reserved-directory rules after explicit user confirmation.
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

/// Copy bounded original-row or selected-directory context before modal UI, without borrowing surface/body across the dialog.
pub(super) fn native_markdown_sidebar_action_context(
    list: &NativeMarkdownSidebarTable,
    origin: &NativeMarkdownSidebarActionOrigin,
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
        if !list.origin_matches_surface(surface, origin) {
            // Original projection/directory authority is required before copying any modal context.
            return Err("侧栏或所选目录已经变化，请重试".to_owned());
        }
        let row = match origin {
            NativeMarkdownSidebarActionOrigin::Row { row_index, .. } => {
                // Row menus consume their original row; note rows cannot acquire child-creation authority from header actions.
                let row = native_markdown_sidebar_row_at(&surface.sidebar_rows, *row_index)
                    .cloned()
                    .ok_or_else(|| "所选侧栏条目已经不可用".to_owned())?;
                if !native_markdown_sidebar_action_is_allowed(row.kind, action) {
                    // A row-kind mismatch cannot authorize filesystem work from the same numeric action tag.
                    return Err("这个侧栏条目不支持所选操作".to_owned());
                }
                Some(row)
            }
            NativeMarkdownSidebarActionOrigin::SelectedDirectory { .. } => {
                // Headers permit existing create/import only, without fabricated directory rows or row-only rename/delete authority.
                if !matches!(
                    action,
                    NativeMarkdownSidebarAction::CreateNote
                        | NativeMarkdownSidebarAction::CreateDirectory
                        | NativeMarkdownSidebarAction::ImportNote
                ) {
                    return Err("所选目录不支持这个头部操作".to_owned());
                }
                None
            }
        };

        let directories = if action == NativeMarkdownSidebarAction::MoveNote {
            // This bounded chooser offers discovered destinations; its editable path field can name another confined folder.
            let current_parent = native_markdown_note_parent(
                &row.as_ref()
                    .expect("移动笔记只来自已验证的Note行")
                    .relative_path,
            );
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
        NativeMarkdownSidebarEntryKind::Note => {
            switch_native_markdown_note(&row.relative_path, None)?
        }
        // Directory selection toggles a subtree projection and restarts its bounded traversal.
        NativeMarkdownSidebarEntryKind::Directory => {
            with_active_native_markdown_surface(|surface| {
                // Browse directory is independent of the open note; projection refresh cannot erase explicit directory selection.
                surface.sidebar_list.select_directory(&row.relative_path);
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
            with_active_native_markdown_surface(|surface| {
                // Only an explicit root click changes destination to root; a directory absent from this page cannot imply root selection.
                surface.sidebar_list.select_directory("");
                Ok::<(), String>(())
            })?;
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

/// Row menus and header actions share name collection, original-origin checks, and the sole mutation workflow.
pub(super) fn request_native_markdown_sidebar_action(
    list: &NativeMarkdownSidebarTable,
    origin: &NativeMarkdownSidebarActionOrigin,
    action: NativeMarkdownSidebarAction,
) -> Result<(), String> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| "Markdown 文件操作必须在 AppKit 主线程执行".to_owned())?;
    // Check modal admission before leasing Active, copying candidates, or touching parent input.
    preflight_native_markdown_modal()?;
    let context = native_markdown_sidebar_action_context(list, origin, action)?;
    if action == NativeMarkdownSidebarAction::ImportNote {
        // Header/document menus share import; the original directory ticket chooses destination, not the open note's parent.
        return import_native_markdown_note_to_directory(list, origin);
    }
    if action == NativeMarkdownSidebarAction::RevealInFinder {
        // Reveal performs no document mutation or save/release handoff, but still revalidates row identity.
        let NativeMarkdownSidebarActionOrigin::Row { row_index, .. } = origin else {
            unreachable!("Finder动作只来自已验证的真实行");
        };
        return reveal_native_markdown_sidebar_row(
            *row_index,
            context.row.as_ref().expect("Finder动作具有真实行"),
        );
    }

    let mutation = autoreleasepool(|_| {
        if matches!(
            action,
            NativeMarkdownSidebarAction::CreateNote | NativeMarkdownSidebarAction::CreateDirectory
        ) {
            // Creation uses the original parent directory, which need not remain in the current bounded page.
            let parent_directory = match origin {
                NativeMarkdownSidebarActionOrigin::Row { .. } => {
                    &context
                        .row
                        .as_ref()
                        .expect("原行创建具有真实目录行")
                        .relative_path
                }
                NativeMarkdownSidebarActionOrigin::SelectedDirectory { relative_path, .. } => {
                    relative_path
                }
            };
            return collect_native_markdown_sidebar_creation(
                &context.controller,
                parent_directory,
                action,
                mtm,
            );
        }
        // Remaining operations passed real row-kind/action admission; header origins cannot enter these branches.
        let row = context.row.as_ref().expect("非创建的文件动作具有真实行");
        match (row.kind, action) {
            (NativeMarkdownSidebarEntryKind::Directory, NativeMarkdownSidebarAction::Rename) => {
                // Retain the exact selected directory identity while collecting only its replacement name.
                let name = run_native_markdown_name_dialog(
                    &context.controller,
                    "重命名目录",
                    "输入目录的新名称。",
                    &row.title,
                    "目录名称",
                    "重命名",
                    mtm,
                )
                .map_err(|error| error.to_string())?;
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
                    &context.controller,
                    "重命名笔记",
                    "输入笔记的新标题。",
                    &row.title,
                    "笔记标题",
                    "重命名",
                    mtm,
                )
                .map_err(|error| error.to_string())?;
                Ok(title.map(|title| NativeMarkdownFileMutation::RenameNote {
                    relative_path: row.relative_path.clone(),
                    title,
                }))
            }
            (NativeMarkdownSidebarEntryKind::Note, NativeMarkdownSidebarAction::MoveNote) => {
                // The chooser accepts a confined relative directory rather than an unchecked absolute path.
                run_native_markdown_move_destination_dialog(
                    &context.controller,
                    &context.directories,
                    mtm,
                )
                .map_err(|error| error.to_string())
                .map(|destination_directory| {
                    destination_directory.map(|destination_directory| {
                        // A move uses the validated destination folder and the same optimistic source revision.
                        NativeMarkdownFileMutation::MoveNote {
                            relative_path: row.relative_path.clone(),
                            destination_directory,
                        }
                    })
                })
            }
            (NativeMarkdownSidebarEntryKind::Note, NativeMarkdownSidebarAction::CopyNote) => {
                // Copy preserves the source identity and lets the core choose a collision-safe destination.
                Ok(Some(NativeMarkdownFileMutation::CopyNote {
                    relative_path: row.relative_path.clone(),
                }))
            }
            (NativeMarkdownSidebarEntryKind::Directory, NativeMarkdownSidebarAction::Delete) => {
                // Confirmation authorizes Core-confined recursive deletion, never the vault root or reserved directories.
                Ok(
                    run_native_markdown_delete_confirmation(&context.controller, row, mtm)
                        .map_err(|error| error.to_string())?
                        .then(|| NativeMarkdownFileMutation::DeleteDirectory {
                            relative_path: row.relative_path.clone(),
                        }),
                )
            }
            (NativeMarkdownSidebarEntryKind::Note, NativeMarkdownSidebarAction::Delete) => {
                // The confirmed note is still subject to save/revision and complete editor release checks.
                Ok(
                    run_native_markdown_delete_confirmation(&context.controller, row, mtm)
                        .map_err(|error| error.to_string())?
                        .then(|| NativeMarkdownFileMutation::DeleteNote {
                            relative_path: row.relative_path.clone(),
                        }),
                )
            }
            // Unlisted row/action pairs cannot synthesize a filesystem command from arbitrary native tags.
            _ => Err("这个侧栏条目不支持所选操作".to_owned()),
        }
    })?;
    if !native_markdown_controller_is_current(&context.controller) || !list.permits_origin(origin) {
        // A dialog result cannot mutate a newly activated session with coincidentally equal row metadata.
        return Err("对话框打开期间笔记会话已变更".into());
    }
    let Some(mutation) = mutation else {
        // Cancelling a confirmation/name/destination dialog leaves vault and active editor untouched.
        return Ok(());
    };
    begin_native_markdown_file_mutation(list, origin, context.row.as_ref(), mutation)
}

/// Context-menu/header creation collects names only; the sole mutation workflow owns actual Core publication.
fn collect_native_markdown_sidebar_creation(
    controller: &NativeMarkdownController,
    parent_directory: &str,
    action: NativeMarkdownSidebarAction,
    mtm: MainThreadMarker,
) -> Result<Option<NativeMarkdownFileMutation>, String> {
    match action {
        // Preserve relative parent identity; Core retains name collision, reserved-directory, and confinement policy.
        NativeMarkdownSidebarAction::CreateNote => run_native_markdown_name_dialog(
            controller,
            "新建笔记",
            "输入新笔记的标题。",
            "未命名笔记",
            "笔记标题",
            "创建",
            mtm,
        )
        .map_err(|error| error.to_string())
        .map(|title| {
            title.map(|title| NativeMarkdownFileMutation::CreateNote {
                parent_directory: parent_directory.to_owned(),
                title,
            })
        }),
        // Directory creation shares the dialog lifecycle, without allocating body text or prematurely changing browse selection.
        NativeMarkdownSidebarAction::CreateDirectory => run_native_markdown_name_dialog(
            controller,
            "新建目录",
            "输入新目录的名称。",
            "新目录",
            "目录名称",
            "创建",
            mtm,
        )
        .map_err(|error| error.to_string())
        .map(|name| {
            name.map(|name| NativeMarkdownFileMutation::CreateDirectory {
                parent_directory: parent_directory.to_owned(),
                name,
            })
        }),
        _ => unreachable!("名称收集只处理已接纳的两个创建动作"),
    }
}

/// Mutation admission preserves original page/row or directory selection; the original surface owns save and actual release.
pub(super) fn begin_native_markdown_file_mutation(
    list: &NativeMarkdownSidebarTable,
    origin: &NativeMarkdownSidebarActionOrigin,
    expected_row: Option<&NativeMarkdownSidebarRow>,
    mutation: NativeMarkdownFileMutation,
) -> Result<(), String> {
    MainThreadMarker::new()
        .ok_or_else(|| "Markdown 文件操作必须在 AppKit 主线程开始".to_owned())?;
    if CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(|surface| surface.is_some()) {
        // Modal reentry may have accepted close; never stage a command into its retiring surface.
        return Err("原生 Markdown 窗口正在关闭".to_owned());
    }
    // Refuse an active modal stack before taking Active or starting body save/release.
    preflight_native_markdown_modal()?;
    let controller = autoreleasepool(|_| {
        with_active_native_markdown_surface(|surface| {
            if !list.origin_matches_surface(surface, origin) {
                // A modal result cannot borrow a replacement projection or changed selected directory.
                return Err("侧栏或所选目录已经变化，请重新选择".to_owned());
            }
            if let NativeMarkdownSidebarActionOrigin::Row { row_index, .. } = origin {
                let current_row = native_markdown_sidebar_row_at(&surface.sidebar_rows, *row_index);
                if current_row != expected_row {
                    // Original page and row identity survive modal return; a reused row index cannot authorize another file.
                    return Err("侧栏已经变化，请重新选择条目".to_owned());
                }
            }
            surface.begin_file_mutation(mutation)?;
            Ok::<_, String>(surface.controller.clone())
        })
    })?;
    with_active_native_markdown_surface(|surface| {
        let pending = surface
            .pending_file_mutation
            .as_ref()
            .ok_or("当前没有等待执行的笔记文件操作")?;
        controller.enqueue_owned_after_release(pending.release_barrier.as_ref(), || {
            if let Err(error) = finish_native_markdown_file_mutation() {
                // Failure may follow disk mutation; preserve the first cause without implicit retry or a guessed path.
                eprintln!("笔记文件操作失败：{error}");
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
            // The begin ticket survives asynchronous release; finish cannot recapture input created while waiting.
            // Feedback advances only through this action's actual cancellation successor and retains no business payload.
            let mut feedback_intent = surface
                .pending_file_mutation
                .as_ref()
                .map(|pending| pending.feedback_intent);
            let mutation_result = surface.finish_file_mutation(mtm, &mut feedback_intent);
            if let Err(error) = &mutation_result
                && feedback_intent
                    .is_some_and(|intent| surface.workspace_refresh_intent_is_current(intent))
            {
                // Publication may already be partial or complete; do not claim unchanged disk or overwrite newer input/close status.
                surface.controller.present_error(error);
            }
            mutation_result
        })
    })
}
