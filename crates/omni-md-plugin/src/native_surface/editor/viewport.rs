//! 模式交换只携带一个真实字形的源位置和屏内偏移，不让旧排版图跨事件存活。

use super::*;

/// 源位置来自已经显示的一个完整scalar/EOF；屏内Y统一从正文可见区顶部向下计量。
#[derive(Clone)]
pub(in crate::native_surface) struct ModeSourceAnchor {
    pub document: WeakMarkdownDocument,
    pub generation: u64,
    pub length: usize,
    pub selection: MarkdownTextSelection,
    pub probe: std::ops::Range<usize>,
    pub screen_y: f64,
}

impl ModeSourceAnchor {
    /// 有限旧票只核同一canonical；不从后继generation重算、平移或猜测源位置。
    pub(in crate::native_surface) fn current_document(&self) -> bool {
        self.document.upgrade().is_some_and(|document| {
            let document = document.borrow();
            document.generation() == self.generation
                && document.utf16_len() == self.length
                && document.selection() == self.selection
        })
    }
}

impl NativeMarkdownEditorSurface {
    /// 挂载完成后让原 Body 绑定当前控制器；Find 布局与模式锚仍由同一个 owner 续排。
    pub(in crate::native_surface) fn finish_mode_viewport_mount(
        &self,
        controller: &Retained<NativeMarkdownController>,
    ) -> Result<(), String> {
        self.body
            .bind_mode_viewport(controller)
            .map_err(|cause| cause.to_string())
    }
}
