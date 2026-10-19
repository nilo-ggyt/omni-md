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

/// 模态前复制原行或真实浏览目录的有限上下文，不跨对话框借用surface或正文。
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
            return Err("侧栏或所选目录已经变化，请重试".to_owned());
        }
        let row = match origin {
            NativeMarkdownSidebarActionOrigin::Row { row_index, .. } => {
                // 原菜单仍只消费自己的真实行，Note行不会凭头部按钮获得创建子项权限。
                let row = native_markdown_sidebar_row_at(&surface.sidebar_rows, *row_index)
                    .cloned()
                    .ok_or_else(|| "所选侧栏条目已经不可用".to_owned())?;
                if !native_markdown_sidebar_action_is_allowed(row.kind, action) {
                    return Err("这个侧栏条目不支持所选操作".to_owned());
                }
                Some(row)
            }
            NativeMarkdownSidebarActionOrigin::SelectedDirectory { .. } => {
                // 头部只恢复原有创建/导入，不伪造目录行或开放重命名、删除等行菜单操作。
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
        NativeMarkdownSidebarEntryKind::Note => switch_native_markdown_note(&row.relative_path)?,
        // Directory selection toggles a subtree projection and restarts its bounded traversal.
        NativeMarkdownSidebarEntryKind::Directory => {
            with_active_native_markdown_surface(|surface| {
                // 浏览目录与当前正文独立；折叠/刷新只能改变投影，不能抹掉这次明确选择。
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
                // 只有明确点击根行才把目的地切回根，不把缺失的页内目录当成根。
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

/// 原行菜单与恢复的头部入口共用名称收集、原来源复核和唯一文件mutation流程。
pub(super) fn request_native_markdown_sidebar_action(
    list: &NativeMarkdownSidebarTable,
    origin: &NativeMarkdownSidebarActionOrigin,
    action: NativeMarkdownSidebarAction,
) -> Result<(), String> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| "Markdown 文件操作必须在 AppKit 主线程执行".to_owned())?;
    // 先纯检查modal，不借出Active、不复制候选或触及父输入。
    preflight_native_markdown_modal()?;
    let context = native_markdown_sidebar_action_context(list, origin, action)?;
    if action == NativeMarkdownSidebarAction::ImportNote {
        // 头部和文档菜单共用一个导入实现，目的地来自原目录票而非当前正文的父目录。
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
            // 同一中文创建面板接收真实来源提供的父目录，不能要求该目录仍在当前有限页。
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
        // 其余操作已由真实行kind/action检查接纳；头部来源不能进入这些分支。
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
                // 显式确认授权受core confinement保护的递归目录删除，不扩大到vault root或保留目录。
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

/// 原右键创建与恢复的头部创建只收集名称；真实Core写入仍由唯一mutation流程负责。
fn collect_native_markdown_sidebar_creation(
    controller: &NativeMarkdownController,
    parent_directory: &str,
    action: NativeMarkdownSidebarAction,
    mtm: MainThreadMarker,
) -> Result<Option<NativeMarkdownFileMutation>, String> {
    match action {
        // 父目录保留原相对身份，名称的冲突、保留目录和路径约束继续由Core执行。
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
        // 目录创建沿同一对话框生命周期，不分配正文或提前更新浏览目录。
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

/// 文件接纳点保留原页/行或目录选择；保存与实际释放继续由原surface负责。
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
    // 在take Active owner或正文save/release之前拒绝仍在栈中的modal。
    preflight_native_markdown_modal()?;
    let controller = autoreleasepool(|_| {
        with_active_native_markdown_surface(|surface| {
            if !list.origin_matches_surface(surface, origin) {
                return Err("侧栏或所选目录已经变化，请重新选择".to_owned());
            }
            if let NativeMarkdownSidebarActionOrigin::Row { row_index, .. } = origin {
                let current_row = native_markdown_sidebar_row_at(&surface.sidebar_rows, *row_index);
                if current_row != expected_row {
                    // 页与行内容都属于原菜单，modal返回不能借同号行操作后继文件。
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
                // 失败可能已改变磁盘，保留原首因；不调度隐式重试或猜测新路径。
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
            // 原ticket来自begin且跨release异步保存；finish不能领取等待期间新B的current代次。
            // 反馈只更新自己实际cancel推进的预期代次，不持任何业务payload。
            let mut feedback_intent = surface
                .pending_file_mutation
                .as_ref()
                .map(|pending| pending.feedback_intent);
            let mutation_result = surface.finish_file_mutation(mtm, &mut feedback_intent);
            if let Err(error) = &mutation_result
                && feedback_intent
                    .is_some_and(|intent| surface.workspace_refresh_intent_is_current(intent))
            {
                // 操作可能已发布或部分完成；不声称磁盘未变，旧意图也不能覆盖新输入/关闭。
                surface.controller.present_error(error);
            }
            mutation_result
        })
    })
}
