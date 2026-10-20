//! 实时查询只借原栏和当前已排窗口；匹配坐标随 Scene 交付，不保查询文字或第二份正文。

use super::*;
use crate::native_surface::{
    NativeMarkdownController,
    find_bar::{
        NativeMarkdownFindBar, NativeMarkdownFindViewportFeedback, NativeMarkdownFindVisibleSource,
        NativeMarkdownPagedFindHighlightError,
    },
};
use objc2::rc::{Retained, Weak as NativeWeak};
use std::ops::Range;

#[derive(Clone)]
pub(super) struct Binding {
    bar: Weak<NativeMarkdownFindBar>,
    controller: Rc<NativeWeak<NativeMarkdownController>>,
}

/// 匹配交付与原栏的临时claim共同活到Scene安装；归还Native claim若重入，
/// 此时原输入回调已能撤销新Scene，不会在撤图之后再安装一幅旧查询的画面。
pub(super) struct Prepared {
    pub ranges: Box<[Range<usize>]>,
    _binding: Option<(
        Rc<NativeMarkdownFindBar>,
        Retained<NativeMarkdownController>,
    )>,
}

impl NativeMarkdownPagedBody {
    /// composition root 绑定同一笔记的实际栏；两边均不新增 Doc、Source 或 Native 强边。
    pub(in crate::native_surface) fn bind_find_highlights(
        &self,
        bar: Weak<NativeMarkdownFindBar>,
        controller: &NativeMarkdownController,
    ) {
        let binding = Binding {
            bar,
            controller: Rc::new(NativeWeak::from(controller)),
        };
        drop(self.find_highlights.replace(Some(binding)));
        self.refresh_bound_find_highlights();
    }

    /// 原栏移交模式时只能解绑它自己；迟到的旧栏不得撤销后来绑定的新栏。
    pub(in crate::native_surface) fn unbind_find_highlights(
        &self,
        bar: &Weak<NativeMarkdownFindBar>,
    ) {
        let owns = self
            .find_highlights
            .borrow()
            .as_ref()
            .is_some_and(|actual| Weak::ptr_eq(&actual.bar, bar));
        if owns {
            self.clear_find_highlights_binding();
        }
    }

    /// accepted close 不保弱控制块或旧匹配；若正文仍在运行，原队列负责擦掉其旧高亮画面。
    pub(in crate::native_surface) fn clear_find_highlights_binding(&self) {
        let binding = self.find_highlights.borrow_mut().take();
        drop(binding); // NativeWeak的最后控制块可进入平台；此处已经没有Body RefMut。
        self.refresh_bound_find_highlights();
    }

    fn refresh_bound_find_highlights(&self) {
        if self.current_body() {
            // 立即撤CPU候选；标题查询的显式取消关闭失败也由同一Body首错边界处理。
            if let Err(cause) = self
                .clear_scene()
                .map_err(Into::into)
                .and_then(|()| self.schedule())
            {
                self.fail(cause);
            }
        }
    }

    /// 输入回调持原 Feedback；这里只撤图并合并原队列，不读查询、不改选择、不申请新 Source。
    pub(in crate::native_surface) fn refresh_find_highlights(
        &self,
        bar: &Rc<NativeMarkdownFindBar>,
        controller: &NativeMarkdownController,
        feedback: &NativeMarkdownFindViewportFeedback,
    ) {
        let binding = self.find_highlights.borrow().clone();
        if self.current_body()
            && binding
                .as_ref()
                .is_some_and(|actual| Weak::ptr_eq(&actual.bar, &Rc::downgrade(bar)))
            && feedback.current(controller)
        {
            self.refresh_bound_find_highlights();
        }
    }

    /// 发布真实窗口时才读取同一栏的已确认查询。visitor 的取消门只读 Rust 标量，
    /// 不在每个字符上调用 Scene.current、viewport.geometry 或任何 Native getter。
    pub(super) fn prepare_find_highlights(
        &self,
        key: &NativeMarkdownPagedSceneKey,
        window: Option<&super::super::paged_line_viewport::NativeMarkdownComposedViewport>,
        grids: &[super::scene::GridRowPublication],
        before_publish: impl FnOnce() -> Result<()>,
    ) -> Result<Option<Prepared>> {
        let empty = || Prepared {
            ranges: Box::new([]),
            _binding: None,
        };
        let binding = self.find_highlights.borrow().clone();
        let Some(binding) = binding else {
            before_publish()?;
            return Ok(self.find_highlights.borrow().is_none().then(empty));
        };
        let bound = || {
            self.find_highlights
                .borrow()
                .as_ref()
                .is_some_and(|actual| {
                    Weak::ptr_eq(&actual.bar, &binding.bar)
                        && Rc::ptr_eq(&actual.controller, &binding.controller)
                })
        };
        let Some(bar) = binding.bar.upgrade() else {
            before_publish()?;
            return Ok(bound().then(empty));
        };
        // Weak.load 及其临时 Native claim 不跨 Body 的 RefCell；热循环只借本栈实际controller。
        let Some(controller) = binding.controller.load() else {
            before_publish()?;
            return Ok(bound().then(empty));
        };
        let Some(feedback) = NativeMarkdownFindViewportFeedback::capture(
            &bar,
            &controller,
            controller.find_input_intent(),
            false,
        ) else {
            before_publish()?;
            return Ok(bound().then(|| Prepared {
                ranges: Box::new([]),
                _binding: Some((bar, controller)),
            })); // 原窗口已失前台资格，没有其他controller可替代它。
        };
        let serial = self.serial.get();
        let layout_epoch = self.layout_epoch.get();
        let current = || {
            self.current_body()
                && self.serial.get() == serial
                && self.layout_epoch.get() == layout_epoch
                && self
                    .input
                    .state()
                    .is_ok_and(|state| state == key.request.input)
                && bound()
                && feedback.current(&controller)
        };
        if !current() {
            return Ok(None);
        }
        if key.request.input.marked.is_some()
            || self.input.is_cell()
            || self.active_cell_body().is_some()
        {
            // 未确认正文或局部草稿不借canonical显示旧匹配；最终Native权限检查也可能换查询。
            before_publish()?;
            return Ok(current().then(|| Prepared {
                ranges: Box::new([]),
                _binding: Some((bar, controller)),
            }));
        }
        let mut visible = Vec::new();
        let mut sources = Vec::new();
        if let Some(window) = window {
            visible.extend(
                window
                    .visible_find_ranges(&current)
                    .map_err(producer::scene_error)?,
            );
        }
        for row in grids {
            for cell in &row.cells {
                if let super::scene::GridCellContent::Composed(window) = &cell.content
                    && let Some(range) = window
                        .visible_canonical_range(&current)
                        .map_err(producer::scene_error)?
                {
                    sources.push(NativeMarkdownFindVisibleSource::Cell {
                        canonical: range,
                        window,
                    });
                }
            }
        }
        // 同一物理行可被正文/格边缘重复覆盖；排序后原地合并，既不重复读源也不重复加深半透明像素。
        visible.sort_unstable_by_key(|range| range.start);
        let mut merged: Vec<Range<usize>> = Vec::with_capacity(visible.len());
        for range in visible {
            if let Some(last) = merged.last_mut()
                && range.start <= last.end
            {
                last.end = last.end.max(range.end);
            } else {
                merged.push(range);
            }
        }
        sources.extend(
            merged
                .into_iter()
                .map(NativeMarkdownFindVisibleSource::Body),
        );
        sources.sort_unstable_by_key(|source| match source {
            NativeMarkdownFindVisibleSource::Body(range) => range.start,
            NativeMarkdownFindVisibleSource::Cell { canonical, .. } => canonical.start,
        }); // 只排序有限已排窗口域；Grid的真正raw片段在匹配时才沿原目录惰性推进。
        let document = self
            .document
            .borrow()
            .clone()
            .ok_or(NativeMarkdownPagedBodyError::Closed)?;
        let result = match feedback.prepare_paged_highlights(&bar, &document, &sources, &current) {
            Ok(result) => result,
            Err(NativeMarkdownPagedFindHighlightError::Source(
                omni_markdown_core::MarkdownDocumentBufferError::FindInterrupted,
            )) if !current() => {
                return Ok(None);
            }
            Err(NativeMarkdownPagedFindHighlightError::Source(cause)) => {
                return Err(NativeMarkdownPagedBodyError::View(ViewError::Document(
                    cause,
                )));
            }
            Err(NativeMarkdownPagedFindHighlightError::Window(cause)) => {
                return Err(NativeMarkdownPagedBodyError::View(producer::scene_error(
                    cause,
                )));
            }
        };
        // 原ticket的最后Source/Native几何核验在此发生；其getter若重入新查询，
        // 下面只读Rust票便拒绝旧匹配，Producer安装Scene前不再经过另一个Native检查。
        before_publish()?;
        if current() {
            Ok(Some(Prepared {
                ranges: result,
                _binding: Some((bar, controller)),
            }))
        } else {
            Ok(None)
        }
    }
}
