//! 当前真实输入客户的中文原生编辑菜单；复用既有 selector 和历史，不另建命令路由。

use super::*;
use objc2::sel;
use objc2_app_kit::{NSMenuItem, NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardTypeString};
use objc2_foundation::{NSString, ns_string};

impl NativeMarkdownPagedTextView {
    /// 每次打开按原客户的当前选区与历史决定可用项，不读取或提交组合候选。
    pub(super) fn edit_context_menu(&self) -> Result<Retained<NSMenu>> {
        let _use = PayloadUseGuard::enter(&self.ivars().depth);
        self.stop_drag_scroll(); // 原生菜单进入跟踪时不继续旧拖选的视口期限。
        self.close_context_menu(); // 旧菜单不累积；提取 owner 后才调用 AppKit，允许合法的关闭重入。
        let origin = self.origin()?;
        let client = origin.delegate.context_menu_client()?;
        if !origin.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        if let Some(client) = client {
            return client.edit_context_menu(); // 在格外父背景右键仍编辑原活动格，不能错误撤销父文稿历史。
        }
        let undo = origin
            .input
            .can_perform_history(NativeMarkdownPagedInputHistory::Undo)?;
        let redo = origin
            .input
            .can_perform_history(NativeMarkdownPagedInputHistory::Redo)?;
        let editing = origin.state.marked.is_none();
        let selected = origin.state.selection_range_count != 0
            && (origin.state.selection_range_count > 1 || origin.state.primary_range.length != 0);
        let pasteboard = origin.step(self, NSPasteboard::generalPasteboard)?;
        // SAFETY: 两个系统公开类型常量只描述可接收格式；本菜单不读取剪贴板中的文字或文件路径。
        let native_types = unsafe { [NSPasteboardTypeString, NSPasteboardTypeFileURL] };
        let types = NSArray::from_slice(if origin.input.is_cell() {
            &native_types[..1] // 原格只接收纯文本；文件导入归正文工作区，菜单不擅自扩大能力。
        } else {
            &native_types
        });
        let paste = origin
            .step(self, || pasteboard.availableTypeFromArray(&types))?
            .is_some();
        let menu = origin.step(self, || {
            NSMenu::initWithTitle(NSMenu::alloc(self.mtm()), ns_string!("编辑"))
        })?;
        origin.step(self, || menu.setAutoenablesItems(false))?;
        // 空 keyEquivalent 保持现有键盘绑定；这里仅增加右键入口，不注册全局或应用快捷键。
        for (index, (title, action, enabled)) in [
            ("撤销", sel!(undo:), undo),
            ("重做", sel!(redo:), redo),
            ("剪切", sel!(cut:), editing && selected),
            ("复制", sel!(copy:), editing && selected),
            ("粘贴", sel!(paste:), editing && paste),
            (
                "全选",
                sel!(selectAll:),
                editing && origin.state.utf16_len != 0,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            if index == 2 || index == 5 {
                let separator = origin.step(self, || NSMenuItem::separatorItem(self.mtm()))?;
                origin.step(self, || menu.addItem(&separator))?;
            }
            let label = origin.step(self, || NSString::from_str(title))?;
            // SAFETY: 固定 action 均由本 NSResponder 实现，参数为正常单 sender 的菜单 action ABI。
            let item = origin.step(self, || unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(self.mtm()),
                    &label,
                    Some(action),
                    ns_string!(""),
                )
            })?;
            // SAFETY: 菜单 target 弱借当前 view；该 owner 在 begin_close 归还 view 前逐项撤 action/target。
            origin.step(self, || unsafe { item.setTarget(Some(self)) })?;
            origin.step(self, || item.setEnabled(enabled))?;
            origin.step(self, || menu.addItem(&item))?;
        }
        if !origin.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        *self.ivars().context_menu.borrow_mut() = Some(menu.clone());
        Ok(menu) // AppKit 负责弹出和跟踪；view 只保这一份有限 owner，无正文或新历史快照。
    }

    /// 被关闭或替换的原菜单立即停止跟踪，所有动作在客户释放之前失权。
    pub(super) fn close_context_menu(&self) {
        let menu = self.ivars().context_menu.borrow_mut().take();
        if let Some(menu) = menu {
            menu.cancelTrackingWithoutAnimation();
            // 只遍历本方法实际建立的六个动作与两个分隔符；不触碰系统或其他窗口的菜单。
            for item in menu.itemArray() {
                // SAFETY: 只清理由本 view 安装的公开菜单关系，未借用任何正文或活动输入事务。
                unsafe {
                    item.setAction(None);
                    item.setTarget(None);
                }
            }
        }
    }

    /// 关闭核验观察本客户实际创建的菜单与项目；弱票不延长跟踪、输入或文稿寿命。
    pub(in crate::native_surface) fn context_menu_weaks(&self) -> Vec<objc2::rc::Weak<AnyObject>> {
        let menu = self.ivars().context_menu.borrow().clone();
        let mut roles = Vec::new();
        if let Some(menu) = menu {
            roles.push(objc2::rc::Weak::from(<NSMenu as AsRef<AnyObject>>::as_ref(
                &menu,
            )));
            // Native getter 在原槽借用之外执行；仅返回当前一份菜单的真实项目弱引用。
            for item in menu.itemArray() {
                roles.push(objc2::rc::Weak::from(
                    <NSMenuItem as AsRef<AnyObject>>::as_ref(&item),
                ));
            }
        }
        roles
    }
}
