//! Group document commands by fixed responsibility; menus retain static labels/bounded tags, never body text, paths, or another controller.

use super::*;

/// Five existing current-note actions use no sidebar row index and remain separate from format groups.
pub(super) const CURRENT_NOTE_ACTIONS: &[(isize, &str)] = &[
    (30, "重命名…"),
    (31, "移动…"),
    (32, "创建副本"),
    (33, "在访达中显示"),
    (34, "删除…"),
];

/// One fixed two-level declaration drives actual construction and retained-feature checks.
pub(super) struct WorkspaceMenuGroup {
    pub(super) title: &'static str,
    pub(super) items: &'static [(isize, &'static str)],
}

pub(super) const GROUPS: &[WorkspaceMenuGroup] = &[
    WorkspaceMenuGroup {
        title: "文件",
        items: &[
            (2, "导入笔记…"),
            (3, "导入附件…"),
            (4, "从磁盘重新载入…"),
            (5, "将草稿另存为新笔记…"),
            (6, "文档信息"),
        ],
    },
    WorkspaceMenuGroup {
        title: "查找与折叠",
        items: &[
            (7, "查找…"),
            (8, "查找与替换…"),
            (20, "折叠或展开当前块"),
            (21, "展开所有折叠块"),
        ],
    },
    WorkspaceMenuGroup {
        title: "标题",
        items: &[(100, "一级标题"), (101, "二级标题"), (102, "三级标题")],
    },
    WorkspaceMenuGroup {
        title: "文字格式",
        items: &[
            (103, "粗体"),
            (104, "斜体"),
            (105, "下划线"),
            (106, "删除线"),
            (107, "行内代码"),
            (108, "链接"),
        ],
    },
    WorkspaceMenuGroup {
        title: "段落与插入",
        items: &[
            (109, "引用"),
            (110, "无序列表"),
            (111, "有序列表"),
            (112, "任务列表"),
            (115, "分隔线"),
            (116, "代码块"),
            (118, "表格"),
        ],
    },
];

/// Supply document prerequisites only; import stays available and table insertion keeps its separate existing admission owner.
/// Folding consumes the accepted source mode, rather than becoming available in preview merely because a document exists.
fn document_menu_action_available(
    tag: isize,
    has_document: bool,
    mode: MarkdownPresentationMode,
) -> Option<bool> {
    match tag {
        20 | 21 => Some(has_document && mode == MarkdownPresentationMode::Source), // Folding is source-only.
        1 | 3..=8 | 30..=34 | 100..=112 | 115..=116 => Some(has_document), // These require a real document.
        _ => None, // Import, grouping, copy, and table retain their own availability rules.
    }
}

/// Create one actual leaf borrowing the same controller as target; the parent menu owns it through accepted-close cleanup.
pub(super) fn workspace_menu_item(
    title: &str,
    tag: isize,
    controller: &NativeMarkdownController,
    has_document: bool,
    mtm: MainThreadMarker,
) -> Retained<NSMenuItem> {
    // SAFETY: The fixed selector uses the NSMenuItem action ABI; the surface retains controller through detachment.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(sel!(performMarkdownWorkspaceAction:)),
            ns_string!(""),
        )
    };
    item.setTag(tag);
    // SAFETY: Target is nonowning; accepted close clears every created item's target before controller release.
    unsafe {
        item.setTarget(Some(controller));
    }
    // Empty/open-note states share the rule; disabled automatic validation cannot rely on external responders to fill it.
    if let Some(available) = document_menu_action_available(
        tag,
        has_document,
        controller.ivars().published_presentation_mode.get(),
    ) {
        item.setEnabled(available);
    }
    item
}

/// Preserve existing file/format commands; visible toolbar actions reuse the original selectors and tags.
pub(super) fn install_native_markdown_workspace_menu(
    actions: &NSPopUpButton,
    controller: &NativeMarkdownController,
    ledger: &NativeMarkdownOwnedLedger,
    has_document: bool,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    let root = actions.menu().ok_or("文档菜单不可用")?;
    // Set leaf availability explicitly instead of letting AppKit infer it from undeclared external responders.
    root.setAutoenablesItems(false);
    let save = workspace_menu_item("保存", 1, controller, has_document, mtm);
    save.setKeyEquivalent(ns_string!("s"));
    root.addItem(&save);
    // SAFETY: Copy uses the existing selector and explicit clipboard transaction, without another command or body mirror.
    let copy = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            ns_string!("复制全文"),
            Some(sel!(copyEntireMarkdownDocument:)),
            ns_string!(""),
        )
    };
    // SAFETY: The surface retains controller; this module detaches its nonowning target/action on accepted close.
    unsafe {
        copy.setTarget(Some(controller));
    }
    copy.setEnabled(has_document);
    root.addItem(&copy);
    assert!(controller.ivars().copy_document_menu_item.set(copy).is_ok());
    root.addItem(&NSMenuItem::separatorItem(mtm));
    for &(tag, title) in CURRENT_NOTE_ACTIONS {
        // Current-note actions work outside the bounded sidebar page; empty state supplies no fabricated note target.
        let item = workspace_menu_item(title, tag, controller, has_document, mtm);
        root.addItem(&item);
    }
    root.addItem(&NSMenuItem::separatorItem(mtm));
    for group in GROUPS {
        // Five fixed groups and at most eight children need no full-workspace/body scan or unbounded hierarchy.
        let submenu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(group.title));
        submenu.setAutoenablesItems(false);
        for &(tag, title) in group.items {
            // Retained daily format/file actions keep original tags; removed features have no executable leaf.
            let item = workspace_menu_item(title, tag, controller, has_document, mtm);
            if tag == 118 {
                // Table requires a real document; later install/empty publication updates this same item through controller.
                item.setEnabled(has_document);
                assert!(controller.ivars().table_menu_item.set(item.clone()).is_ok());
            }
            submenu.addItem(&item);
        }
        // Register each menu and its items independently; tracking only the root would omit submenu resources.
        ledger.include_menu("workspace.actions.submenu", &submenu)?;
        // SAFETY: A grouping item has no action/target and cannot execute as a command leaf.
        let section = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(group.title),
                None,
                ns_string!(""),
            )
        };
        section.setSubmenu(Some(&submenu));
        root.addItem(&section);
    }
    ledger.include_menu("workspace.actions.menu", &root)
}

/// Synchronize the same two-level leaves for empty workspace, successful creation, and final-note deletion.
/// Table retains its own owner; native getters/setters recheck close, document availability, and accepted mode.
pub(super) fn set_current_note_actions_available(
    controller: &NativeMarkdownController,
    available: bool,
) {
    let mode = controller.ivars().published_presentation_mode.get();
    let current = || {
        controller.ivars().document_actions_available.get() == available
            && controller.ivars().published_presentation_mode.get() == mode
            && native_markdown_controller_has_foreground_authority(controller)
    };
    if !current() {
        return; // Construction already sets availability; unpublished or closed windows cannot reconstruct menus here.
    }
    let controls = controller
        .ivars()
        .workspace_controls
        .get()
        .expect("当前窗口已安装原工作区控件");
    let popup = controls[1]
        .downcast_ref::<NSPopUpButton>()
        .expect("工作区第二项是原文档菜单");
    if !current() {
        return;
    }
    let Some(menu) = popup.menu() else {
        return; // A detached menu grants no authority to update a successor menu.
    };
    if !current() {
        return;
    }
    let items = menu.itemArray();
    if !current() {
        return;
    }
    for index in 0..items.count() {
        // Each native read/setter must preserve this captured availability and mode, without recapturing new document state.
        if !current() {
            return;
        }
        let item = items.objectAtIndex(index);
        if !current() {
            return;
        }
        let tag = item.tag();
        if !current() {
            return;
        }
        if let Some(enabled) = document_menu_action_available(tag, available, mode) {
            item.setEnabled(enabled);
        }
        if !current() {
            return; // Top-level setters may reenter; old document state cannot continue updating replacement submenus.
        }
        let submenu = item.submenu();
        if !current() {
            return;
        }
        let Some(submenu) = submenu else { continue };
        let children = submenu.itemArray();
        if !current() {
            return;
        }
        // Visit only this item's actual second level, without recursing through system menus or creating another state inventory.
        for child_index in 0..children.count() {
            if !current() {
                return;
            }
            let child = children.objectAtIndex(child_index);
            if !current() {
                return;
            }
            let tag = child.tag();
            if !current() {
                return;
            }
            if let Some(enabled) = document_menu_action_available(tag, available, mode) {
                child.setEnabled(enabled);
            }
        }
    }
}

/// Only current owned toolbar buttons or actual menu leaves supply tags; NSPopUpButton itself cannot issue a duplicate action.
pub(super) fn native_markdown_workspace_menu_action(
    controller: &NativeMarkdownController,
    sender: &AnyObject,
) -> Option<isize> {
    if !native_markdown_controller_is_current(controller)
        || controller.ivars().close_finalizer_scheduled.get()
    {
        // Framework menu retention cannot restore authority to a late or foreign controller.
        return None;
    }
    let controls = controller.ivars().workspace_controls.get()?;
    if let Some(button) = sender.downcast_ref::<NSButton>() {
        let owned = controls
            .iter()
            .skip(NATIVE_MARKDOWN_TOOLBAR_START)
            .any(|control| std::ptr::eq(&**control, button.as_super()));
        let tag = button.tag();
        // Only installed action buttons may dispatch these tags; a changed header tag cannot authorize arbitrary file commands.
        return (owned
            && matches!(tag, 3 | 6 | 7 | 22 | 23 | 100..=112 | 115..=116 | 118)
            && native_markdown_controller_is_current(controller)
            && !controller.ivars().close_finalizer_scheduled.get())
        .then_some(tag);
    }
    let item = sender.downcast_ref::<NSMenuItem>()?;
    let mut owned = false;
    for control in controls {
        let Some(popup) = control.downcast_ref::<NSPopUpButton>() else {
            continue; // Buttons/search fields grant no menu authority; do not search unrelated system hierarchies for tags.
        };
        let Some(menu) = popup.menu() else { continue };
        let items = menu.itemArray();
        for index in 0..items.count() {
            // Inspect only the configured two-level menu, without discovering commands through system responders.
            let candidate = items.objectAtIndex(index);
            if std::ptr::eq(&*candidate, item) {
                owned = true;
                break;
            }
            if let Some(submenu) = candidate.submenu() {
                let children = submenu.itemArray();
                for child in 0..children.count() {
                    // Compare live identities, never guessing ownership from title or tag.
                    if std::ptr::eq(&*children.objectAtIndex(child), item) {
                        owned = true;
                        break;
                    }
                }
            }
        }
        if owned {
            break;
        }
    }
    let tag = item.tag();
    let known = tag == 1
        || CURRENT_NOTE_ACTIONS.iter().any(|entry| entry.0 == tag)
        || GROUPS
            .iter()
            .any(|group| group.items.iter().any(|entry| entry.0 == tag));
    // Native getters may reenter close; final pure owner checks prevent a live old item from authorizing a new session.
    (owned
        && known
        && native_markdown_controller_is_current(controller)
        && !controller.ivars().close_finalizer_scheduled.get())
    .then_some(tag)
}

/// Detach callbacks from the owned two-level menu after close acceptance, without rebuilding or touching unknown system trees.
pub(super) fn detach_native_markdown_workspace_menu(controller: &NativeMarkdownController) {
    let Some(controls) = controller.ivars().workspace_controls.get() else {
        // Incomplete construction has no published menu; cleanup grants no authority to create a replacement.
        return;
    };
    for control in controls {
        let Some(popup) = control.downcast_ref::<NSPopUpButton>() else {
            continue; // Existing control teardown owns button/search targets; do not inspect foreign menus.
        };
        let Some(menu) = popup.menu() else { continue };
        let items = menu.itemArray();
        for index in 0..items.count() {
            // Both configured document-menu levels are owned here; never traverse a private system subtree.
            let item = items.objectAtIndex(index);
            if let Some(submenu) = item.submenu() {
                let children = submenu.itemArray();
                for child in 0..children.count() {
                    let child = children.objectAtIndex(child);
                    // SAFETY: These are main-thread leaves configured here; nil removes only project target/action edges.
                    unsafe {
                        child.setTarget(None);
                        child.setAction(None);
                    }
                }
            }
            // SAFETY: Top-level items are also owned here; nil cleanup is idempotent for groups and inert titles.
            unsafe {
                item.setTarget(None);
                item.setAction(None);
            }
        }
    }
}
