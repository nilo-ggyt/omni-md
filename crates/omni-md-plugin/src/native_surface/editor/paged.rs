//! 两种展示模式共用唯一分页图；模型、Find 客户端和 Native 资源沿原会话结束。

use super::*;
use crate::native_surface::paged_body::{
    NativeMarkdownPagedBody, NativeMarkdownPagedBodyColors, NativeMarkdownPagedBodyConfiguration,
    NativeMarkdownPagedBodyInputObserver,
};
use crate::native_surface::paged_input::NativeMarkdownPagedInputCommit;
use crate::native_surface::paged_text_view::{
    NativeMarkdownPagedCloseReceipt, NativeMarkdownPagedTextView,
};

/// 窗口持业务模型，Body持原布局管线；两者借同一allocation，不建立Foundation全文镜像。
pub(crate) struct NativeMarkdownEditorSurface {
    pub(in crate::native_surface) find_bar: RefCell<Option<Rc<NativeMarkdownFindBar>>>,
    pub(in crate::native_surface) find_resources: RefCell<NativeMarkdownFindResources>,
    pub(in crate::native_surface) find_ledger: RefCell<Option<Rc<NativeMarkdownOwnedLedger>>>,
    pub(in crate::native_surface) session_field_editor:
        RefCell<Option<Weak<NativeMarkdownSessionFieldEditor>>>,
    pub(in crate::native_surface) scroll_view: Retained<NSScrollView>,
    pub(in crate::native_surface) text_view: Rc<Retained<NativeMarkdownPagedTextView>>,
    pub(in crate::native_surface) body: Rc<NativeMarkdownPagedBody>,
    pub(in crate::native_surface) owned_ledger: Rc<NativeMarkdownOwnedLedger>,
    _observer: Rc<SourceEdits>,
    close: OnceCell<Rc<NativeMarkdownPagedCloseReceipt>>,
    pub(in crate::native_surface) model: NativeMarkdownTextModel,
    pub(in crate::native_surface) document_resources: NativeMarkdownOwnedSnapshot,
}

/// 原提交只排同一canonical的业务反馈；选择/marked变化不伪造磁盘文字修改。
struct SourceEdits {
    document: WeakMarkdownDocument,
}
impl NativeMarkdownPagedBodyInputObserver for SourceEdits {
    fn committed(&self, commit: &NativeMarkdownPagedInputCommit) {
        if commit.invalidation.characters_changed || commit.invalidation.marked_changed {
            // 候选也会改变canonical，但纯unmark可能没有字符变化；两者都刷新原会话事实。
            // 自动保存期限会核实际marked状态，不借850ms期限提前提交系统仍拥有的候选。
            queue_native_markdown_autosave_for_document(self.document.clone());
        }
        // 纯选择和unmark也改变历史按钮是否可用；它们不能依赖下一次正文字符变化才更新。
        super::super::controller::queue_native_markdown_history_for_document(&self.document);
    }

    fn local_input_changed(&self) {
        // 局部键入不安排磁盘写入；真正结束草稿后，先前dirty正文仍沿原期限自动保存。
        super::super::controller::queue_native_markdown_local_input_for_document(&self.document);
    }
}

impl NativeMarkdownEditorSurface {
    /// 从唯一 Core 文稿建立正文与滚动根；两种模式共用同一有界 Input/Body。
    pub(crate) fn new_with_mode(
        document: MarkdownDocumentBuffer,
        frame: NSRect,
        mode: MarkdownPresentationMode,
        resource_root: &NativeMarkdownOwnedLedger,
        mtm: MainThreadMarker,
    ) -> Result<Self, String> {
        let model = NativeMarkdownTextModel::new(document);
        let backing = model.document().borrow().original_backing_observer();
        let document_resources = resource_root.include_document_backing(backing)?;
        let owned_ledger = Rc::new(NativeMarkdownOwnedLedger::for_editor(resource_root));
        let scroll_view = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
        scroll_view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        scroll_view.setHasVerticalScroller(true);
        scroll_view.setHasHorizontalScroller(false);
        scroll_view.setDrawsBackground(true);
        // 正文和源码沿同一 14pt 文稿等宽 base；UI 字体的中文半宽策略不属于文稿排版合同。
        let font = native_markdown_document_fixed_font(14.0).map_err(|cause| cause.to_string())?;
        let paragraph = objc2_app_kit::NSParagraphStyle::defaultParagraphStyle();
        let body = NativeMarkdownPagedBody::attach(
            Rc::clone(model.document()),
            &scroll_view,
            font.as_ref(),
            &paragraph,
            NativeMarkdownPagedBodyConfiguration {
                presentation_mode: mode,
                epoch: 1,
                // 系统Emoji字体集合可超过64MiB；512MiB仅限制来源文件extent，
                // 沿原匿名clone+mmap按表/字形触页，不预读全部字体；实际驻留仍独立验收。
                maximum_font_bytes: 512 * 1024 * 1024,
                padding: 8.0,
                colors: NativeMarkdownPagedBodyColors {
                    background: [0.075, 0.084, 0.11, 1.0],
                    active_line: [27.0 / 255.0, 32.0 / 255.0, 42.0 / 255.0, 1.0],
                    text: [0.9, 0.91, 0.94, 1.0],
                    selection: [0.2, 0.38, 0.63, 1.0],
                    caret: [0.9, 0.91, 0.94, 1.0],
                },
            },
            mtm,
        )
        .map_err(|cause| cause.to_string())?;
        let text_view = body.view().map_err(|cause| cause.to_string())?;
        owned_ledger.include("editor.scroll", &scroll_view)?;
        owned_ledger.include("editor.paged-input", &text_view)?;
        let observer = Rc::new(SourceEdits {
            document: Rc::downgrade(model.document()),
        });
        let erased: Rc<dyn NativeMarkdownPagedBodyInputObserver> = observer.clone();
        body.observe_input(Rc::downgrade(&erased))
            .map_err(|cause| cause.to_string())?;
        // 初次挂载只揭示当前真实选择；后续模式锚由同一 Body 的模式事务保存。
        body.reveal_current_selection()
            .map_err(|cause| cause.to_string())?;
        Ok(Self {
            find_bar: RefCell::new(None),
            find_resources: RefCell::new(NativeMarkdownFindResources::default()),
            find_ledger: RefCell::new(Some(Rc::new(NativeMarkdownOwnedLedger::for_editor(
                resource_root,
            )))),
            session_field_editor: RefCell::new(None),
            scroll_view,
            text_view,
            body,
            owned_ledger,
            _observer: observer,
            close: OnceCell::new(),
            model,
            document_resources,
        })
    }

    /// 只缓存真实接纳的关闭收据；资源清理首错仍留原收据，未接纳的Busy不封闭当前图。
    pub(in crate::native_surface) fn begin_close(
        &self,
    ) -> Result<Rc<NativeMarkdownPagedCloseReceipt>, String> {
        if let Some(receipt) = self.close.get() {
            return Ok(receipt.clone());
        }
        let receipt = self.body.begin_close().map_err(|cause| cause.to_string())?;
        // Busy属于尚未接纳：不写OnceCell、下次仍可正常预检；成功后只保原Body同一次资源收据。
        // 若Native清理重入已观察同receipt，沿原单元取回，不重建关闭图或丢失其sticky首错。
        Ok(self.close.get_or_init(|| receipt).clone())
    }

    /// 父窗口已完成保存/辞任预检；这里仅接纳拆图，最终成功仍由原close收据核验。
    pub(in crate::native_surface) fn stop_accepting_input(&self) {
        let _ = self.begin_close(); // 最终barrier仍取得原Body结果；Drop不把Busy或清理失败伪装为释放成功。
    }
}

impl Drop for NativeMarkdownEditorSurface {
    fn drop(&mut self) {
        self.stop_accepting_input();
        if let Some(bar) = self.find_bar.take() {
            bar.finish_accepted();
        }
        // 观察客户随当前图归还；Body只保Weak，迟到任务不会延长此会话或canonical根。
    }
}
