//! 文档命令按固定职责分组；菜单只持静态标签/有限tag，不保存正文、路径或第二控制器。

use super::*;

/// 原“更多”菜单的五个当前笔记动作；它们不使用侧栏行号，也不占用格式分组的位置。
pub(super) const CURRENT_NOTE_ACTIONS: &[(isize, &str)] = &[
    (30, "重命名…"),
    (31, "移动…"),
    (32, "创建副本"),
    (33, "在 Finder 中显示"),
    (34, "删除…"),
];

/// 固定两层菜单的数据声明同时驱动真实构造和功能保留测试。
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

/// 创建一个实际叶项，target借用同一controller；父menu拥有它直到accepted清理。
pub(super) fn workspace_menu_item(
    title: &str,
    tag: isize,
    controller: &NativeMarkdownController,
    mtm: MainThreadMarker,
) -> Retained<NSMenuItem> {
    // SAFETY: 固定selector采用NSMenuItem action ABI；controller由当前surface强持有到detach之后。
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(sel!(performMarkdownWorkspaceAction:)),
            ns_string!(""),
        )
    };
    item.setTag(tag);
    // SAFETY: target为借用关系，所有此函数创建的item在accepted期间逐项置nil后再释放controller。
    unsafe {
        item.setTarget(Some(controller));
    }
    item
}

/// 文档菜单保留完整文件与格式命令；可见工具栏仍复用这些原selector和tag。
pub(super) fn install_native_markdown_workspace_menu(
    actions: &NSPopUpButton,
    controller: &NativeMarkdownController,
    ledger: &NativeMarkdownOwnedLedger,
    has_document: bool,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    let root = actions.menu().ok_or("文档菜单不可用")?;
    // 各叶项明确配置可用性，不能让AppKit按未声明的外部responder猜测启用状态。
    root.setAutoenablesItems(false);
    let save = workspace_menu_item("保存", 1, controller, mtm);
    save.setKeyEquivalent(ns_string!("s"));
    root.addItem(&save);
    // SAFETY: 复制沿既有selector直接执行同一显式剪贴板事务，不新增命令或正文镜像。
    let copy = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            ns_string!("复制全文"),
            Some(sel!(copyEntireMarkdownDocument:)),
            ns_string!(""),
        )
    };
    // SAFETY: 同一surface强持controller，accepted时由本模块断开此借用target/action。
    unsafe {
        copy.setTarget(Some(controller));
    }
    copy.setEnabled(has_document);
    root.addItem(&copy);
    assert!(controller.ivars().copy_document_menu_item.set(copy).is_ok());
    root.addItem(&NSMenuItem::separatorItem(mtm));
    for &(tag, title) in CURRENT_NOTE_ACTIONS {
        // 当前文档即使不在侧栏的有限页内，也保留原五项直接入口；空态不提供假目标。
        let item = workspace_menu_item(title, tag, controller, mtm);
        item.setEnabled(has_document);
        root.addItem(&item);
    }
    root.addItem(&NSMenuItem::separatorItem(mtm));
    for group in GROUPS {
        // 分组数固定5，每个子菜单最多8项；没有动态全库/正文扫描或无限层级。
        let submenu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(group.title));
        submenu.setAutoenablesItems(false);
        for &(tag, title) in group.items {
            // 保留的日常格式与文件动作仍使用原 tag；已删除的功能没有可执行叶项。
            let item = workspace_menu_item(title, tag, controller, mtm);
            if tag == 118 {
                // 表格仅在真实文档存在时启用，后续install/empty沿原controller方法更新此item。
                item.setEnabled(has_document);
                assert!(controller.ivars().table_menu_item.set(item.clone()).is_ok());
            }
            submenu.addItem(&item);
        }
        // 每个显式menu和其实际item独立登记，不能只跟踪最上层而漏掉子菜单资源。
        ledger.include_menu("workspace.actions.submenu", &submenu)?;
        // SAFETY: 分组没有action/target，不可作为命令叶执行。
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

/// 同步原文档菜单的五个叶项；每个Native getter/setter后都拒绝关闭或较新的启停状态。
pub(super) fn set_current_note_actions_available(
    controller: &NativeMarkdownController,
    available: bool,
) {
    let current = || {
        controller.ivars().document_actions_available.get() == available
            && native_markdown_controller_has_foreground_authority(controller)
    };
    if !current() {
        return; // 构造时叶项已按has_document设置；未发布或已关闭的窗口不补建菜单。
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
        return; // 已拆除的菜单没有后继启停权限。
    };
    if !current() {
        return;
    }
    let items = menu.itemArray();
    if !current() {
        return;
    }
    for index in 0..items.count() {
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
        if CURRENT_NOTE_ACTIONS.iter().any(|entry| entry.0 == tag) {
            item.setEnabled(available);
        } // 只更新当前笔记动作，保存/格式/查找仍归原有状态与命令规则。
    }
}

/// 仅当前自有工具栏按钮或menu实际leaf可贡献tag，NSPopUpButton本身不发第二次action。
pub(super) fn native_markdown_workspace_menu_action(
    controller: &NativeMarkdownController,
    sender: &AnyObject,
) -> Option<isize> {
    if !native_markdown_controller_is_current(controller)
        || controller.ivars().close_finalizer_scheduled.get()
    {
        // 迟到/外来控制器不因menu仍被框架短暂持有就恢复操作权限。
        return None;
    }
    let controls = controller.ivars().workspace_controls.get()?;
    if let Some(button) = sender.downcast_ref::<NSButton>() {
        let owned = controls
            .iter()
            .skip(NATIVE_MARKDOWN_TOOLBAR_START)
            .any(|control| std::ptr::eq(&**control, button.as_super()));
        let tag = button.tag();
        // 只有实际安装的这些动作按钮可直达；外方改tag不能把header按钮变成任意文件命令。
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
            continue; // 按钮和搜索字段没有菜单，不从任意系统层级寻找可执行tag。
        };
        let Some(menu) = popup.menu() else { continue };
        let items = menu.itemArray();
        for index in 0..items.count() {
            // 文档菜单固定两层；只查实际配置的有限菜单，不沿系统responder寻找命令。
            let candidate = items.objectAtIndex(index);
            if std::ptr::eq(&*candidate, item) {
                owned = true;
                break;
            }
            if let Some(submenu) = candidate.submenu() {
                let children = submenu.itemArray();
                for child in 0..children.count() {
                    // 仍逐个比较活对象身份，不以标题或tag猜归属。
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
    // 上方Native getter可重入关闭；最后再纯核owner，不能将活item当作新会话权限。
    (owned
        && known
        && native_markdown_controller_is_current(controller)
        && !controller.ivars().close_finalizer_scheduled.get())
    .then_some(tag)
}

/// accepted后对固定两层项目菜单拆回调；不重建菜单、不触碰未知系统菜单树。
pub(super) fn detach_native_markdown_workspace_menu(controller: &NativeMarkdownController) {
    let Some(controls) = controller.ivars().workspace_controls.get() else {
        // 构造尚未完成时没有已发布菜单，不补建替身。
        return;
    };
    for control in controls {
        let Some(popup) = control.downcast_ref::<NSPopUpButton>() else {
            continue; // 按钮/搜索字段的借用边已由原controls拆图处理，不查外方菜单。
        };
        let Some(menu) = popup.menu() else { continue };
        let items = menu.itemArray();
        for index in 0..items.count() {
            // 文档固定两层均为实际自有菜单，不遍历系统私有子树。
            let item = items.objectAtIndex(index);
            if let Some(submenu) = item.submenu() {
                let children = submenu.itemArray();
                for child in 0..children.count() {
                    let child = children.objectAtIndex(child);
                    // SAFETY: 这些全为本模块配置的主线程叶项，置nil只解除项目target/action。
                    unsafe {
                        child.setTarget(None);
                        child.setAction(None);
                    }
                }
            }
            // SAFETY: 顶层同属本项目配置，分组和隐藏标题无action时nil清理亦为幂等。
            unsafe {
                item.setTarget(None);
                item.setAction(None);
            }
        }
    }
}
