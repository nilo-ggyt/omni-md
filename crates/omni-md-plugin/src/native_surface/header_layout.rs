//! 页眉只保存已有控件的弱身份；同步布局不拥有文档、计时器或后继窗口权限。

use super::*;

mod geometry;
pub(super) use geometry::editor_area_frames;
use geometry::header_frames;

/// 只弱观察既有两行页眉、临时反馈和三列footer；实际关闭仍由原构造账本和controller负责。
pub(super) struct NativeMarkdownHeaderControls {
    controls: [Weak<NSView>; 14],
}

/// 构造完成前尚无全局Active owner；运行期必须额外验证现有前台所有权。
#[derive(Clone, Copy)]
pub(super) enum NativeMarkdownHeaderAuthority {
    Construction,
    Foreground,
}

impl NativeMarkdownHeaderControls {
    /// 两个原按钮保持位置与动作；只写入原路由计算的历史可用性，不拥有历史或字段。
    pub(super) fn set_history_available(&self, available: [bool; 2], current: &dyn Fn() -> bool) {
        for (control, available) in self.controls[4..6].iter().zip(available) {
            if !current() {
                return; // 旧文稿或输入已撤权时，不升级仍存活的旧控件。
            }
            let Some(control) = control
                .load()
                .and_then(|control| control.downcast::<NSControl>().ok())
            else {
                return; // 实际按钮已经离开，不分配替身或重建工具栏。
            };
            if !current() {
                return;
            }
            control.setEnabled(available);
        }
    }

    /// 页眉只观察格式栏实际scroll根，内部各格式按钮由原controls和视图树持有。
    pub(super) fn new(controls: [&NSView; 8], toolbar: [Weak<NSView>; 6]) -> Self {
        let [
            title,
            mode,
            menu,
            status,
            path,
            footer_mode,
            footer_save,
            footer_counts,
        ] = controls.map(Weak::new);
        let [undo, redo, find, info, copy, format] = toolbar;
        Self {
            controls: [
                title,
                mode,
                menu,
                status,
                undo,
                redo,
                find,
                info,
                copy,
                format,
                path,
                footer_mode,
                footer_save,
                footer_counts,
            ],
        }
    }

    /// 同一同步调用只发布同一内容尺寸；重入resize或关闭立即撤销余下setter权限。
    pub(super) fn layout(
        &self,
        controller: &NativeMarkdownController,
        window: &objc2_app_kit::NSWindow,
        authority: NativeMarkdownHeaderAuthority,
    ) -> bool {
        if controller.ivars().close_finalizer_scheduled.get()
            || VERIFYING_NATIVE_MARKDOWN_CLOSE.with(Cell::get).is_some()
            || (matches!(authority, NativeMarkdownHeaderAuthority::Foreground)
                && !native_markdown_controller_has_foreground_authority(controller))
        {
            // 第一项Native getter之前就拒绝accepted/Closing；Construction仅免除尚未发布Active这一条件。
            return false;
        }
        // 代次只保护当前栈内同步发布；耗尽则保持现状，不回绕成旧布局权限。
        let Some(revision) = controller
            .ivars()
            .header_layout_revision
            .get()
            .checked_add(1)
        else {
            return false;
        };
        controller.ivars().header_layout_revision.set(revision);
        let Some(content) = window.contentView() else {
            // 关闭后没有内容根，不恢复已拆卸控件。
            return false;
        };
        let size = content.frame().size;
        let Some(frames) = header_frames(size.width, size.height) else {
            // 非法尺寸不产生部分几何；窗口最小尺寸仍由既有900x600合同限定。
            return false;
        };
        let is_current = || {
            // 先核对纯标量；任何重入后的旧栈都不能继续修改同一批控件。
            if controller.ivars().close_finalizer_scheduled.get()
                || VERIFYING_NATIVE_MARKDOWN_CLOSE.with(Cell::get).is_some()
                || controller.ivars().header_layout_revision.get() != revision
                || (matches!(authority, NativeMarkdownHeaderAuthority::Foreground)
                    && !native_markdown_controller_has_foreground_authority(controller))
            {
                return false;
            }
            let Some(panel) = controller.ivars().editing_window.load() else {
                // weak窗口已结束，局部控件存活也不构成布局权限。
                return false;
            };
            let owned_window: &objc2_app_kit::NSWindow = &panel;
            if !std::ptr::eq(owned_window, window) {
                // 外来窗口通知不得修改本controller的页眉。
                return false;
            }
            if matches!(authority, NativeMarkdownHeaderAuthority::Foreground)
                && !super::controller::native_markdown_controller_owns_editing_window(
                    controller, window,
                )
            {
                // 只有构造中的同步调用可尚未发布Active；实际resize必须属于现有owner。
                return false;
            }
            let Some(current_content) = window.contentView() else {
                // getter重入关闭后内容根消失，旧矩形不能重新挂回。
                return false;
            };
            if !std::ptr::eq(&*content, &*current_content) || current_content.frame().size != size {
                // 真实内容根或尺寸变化使整组旧结果失效；由新的resize正常同步接管。
                return false;
            }
            // 上方getter也可能重入；最后再检查代次与关闭事实，不保留任何TLS借用。
            !controller.ivars().close_finalizer_scheduled.get()
                && VERIFYING_NATIVE_MARKDOWN_CLOSE.with(Cell::get).is_none()
                && controller.ivars().header_layout_revision.get() == revision
                && (!matches!(authority, NativeMarkdownHeaderAuthority::Foreground)
                    || native_markdown_controller_has_foreground_authority(controller))
        };

        for (control, frame) in self.controls.iter().zip(frames.0) {
            if !is_current() {
                // accepted teardown里即使Weak尚活，也不先升级旧控件而干扰严格释放。
                return false;
            }
            // weak临时升级只覆盖一个setter；不让整组临时强引用跨关闭或布局回调保活。
            let Some(control) = control.load() else {
                // 构造合同中缺少任一控件时停止，不能创建替身掩盖关闭状态。
                return false;
            };
            if !is_current() {
                // 不排队重试；较新的resize或关闭已经取得唯一后续权限。
                return false;
            }
            control.setFrame(NSRect::new(
                NSPoint::new(frame.x, frame.y),
                NSSize::new(frame.width, frame.height),
            ));
            if !is_current() {
                // setter内重入后，剩余旧控件不得覆盖较新一次完整布局。
                return false;
            }
        }
        true
    }
}
