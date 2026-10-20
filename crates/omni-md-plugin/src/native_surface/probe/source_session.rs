//! 正式窗口Source接入的单个业务场景；沿原workspace、controller动作、流式文稿和严格关闭运行。

use super::*;
use crate::native_surface::paged_body::{
    NativeMarkdownPagedBody, NativeMarkdownPagedBodyPaintObserver,
};
use crate::native_surface::paged_text_view::NativeMarkdownPagedSceneKey;
use std::io::{self, Read, Seek, SeekFrom, Write};

const INSERTED: &str = "中文😀接入e\u{301}";
const QUERY: &str = "第115行";
const FIELD_RANGE: NSRange = NSRange::new(1, 2);

#[path = "source_attachments.rs"]
mod attachments;

#[path = "source_relative_link.rs"]
mod relative_link;
pub(super) use relative_link::RelativeLink;

use super::mode_viewport as viewport;

#[path = "source_folding.rs"]
mod folding;

/// 只保存实际帧key及一次后继事件；没有Scene、Reader、Body或Native强边。
struct SourceFrame {
    weak: std::rc::Weak<Self>,
    body: RefCell<std::rc::Weak<NativeMarkdownPagedBody>>,
    generation: Cell<u64>,
    mode: Cell<MarkdownPresentationMode>,
    fold_revision: Cell<Option<u64>>,
    failure: RefCell<Option<String>>,
    key: RefCell<Option<NativeMarkdownPagedSceneKey>>,
    signal: RefCell<Option<Box<dyn FnOnce()>>>,
}

/// Pending按钮流程的预声明后态；只保原完整Input和实际动作代次，不携带旧Scene几何。
#[derive(Clone, Copy)]
struct PendingModeInput {
    input: NativeMarkdownPagedInputState,
    epoch: MemoryActionEpoch,
    mode: MarkdownPresentationMode,
}
impl NativeMarkdownPagedBodyPaintObserver for SourceFrame {
    fn frame_published(&self, key: &NativeMarkdownPagedSceneKey, _paints: usize) {
        if self.signal.borrow().is_none() {
            return;
        } // 已消费的观察没有重新等待后继帧的权限。
        if key.presentation_mode != self.mode.get()
            || key.request.input.generation != self.generation.get()
            || self
                .fold_revision
                .get()
                .is_some_and(|revision| key.request.input.fold_revision != revision)
        {
            // Word→附件→字符会有真实中间帧；仅原终点generation能唤醒本动作，不把中间Ready当成功。
            let rearmed = (|| {
                if key.request.input.generation > self.generation.get()
                    || self
                        .fold_revision
                        .get()
                        .is_some_and(|revision| key.request.input.fold_revision > revision)
                {
                    return Err("正式Source期望帧已被后继编辑越过".into());
                }
                let body = self
                    .body
                    .borrow()
                    .upgrade()
                    .ok_or("正式Source原帧owner已释放")?;
                let frame = self.weak.upgrade().ok_or("正式Source原帧观察已释放")?;
                let erased: Rc<dyn NativeMarkdownPagedBodyPaintObserver> = frame;
                body.observe_next_cpu_frame(Rc::downgrade(&erased))
                    .map_err(|cause| cause.to_string())
            })();
            if let Err(cause) = rearmed {
                *self.failure.borrow_mut() = Some(cause);
            } else {
                return;
            } // 原一次通知已结束；登记只接下一真实发布，不递归回调、轮询或强持Body。
        }
        *self.key.borrow_mut() = Some(key.clone());
        let signal = self.signal.borrow_mut().take();
        if let Some(signal) = signal {
            signal();
        }
    }
}

/// 原FD保持测试合成seed的独立字节来源；不保存整篇期待文本，返回前先归还此测试资源。
pub(super) struct SourceSession {
    original: MemoryWorkloadFacts,
    original_file: std::fs::File,
    bar: std::rc::Weak<NativeMarkdownFindBar>,
    replaced_bytes: std::ops::Range<usize>,
    selection: Option<MarkdownTextSelection>,
    frame: Rc<SourceFrame>,
    attachments: Option<attachments::SourceAttachments>,
    viewport: RefCell<Option<Rc<viewport::ModeViewportExpectation>>>,
    folding: Option<folding::SourceFolding>,
    pending_mode_input: Option<PendingModeInput>,
}

impl SourceSession {
    /// 先在正式预览窗口建立真实Find字段，后续Source必须接管同一栏而非新建替身。
    fn begin(workload: &NativeMarkdownMemoryWorkload) -> Result<Self, String> {
        // Host激活后已经固定原Doc和磁盘；专项只消费这份原票，不能重采后来同名文稿。
        workload.verify_unchanged()?;
        let original = workload.facts()?;
        // Show只改变原正文的实际分配高度；本次有限insertText才推进一次Find意图。
        // 在首个动作之前声明后态，不能等布局尚未Ready时重新采样一个较新的输入作为基线。
        let before = original.opening_epoch;
        let expected = MemoryActionEpoch {
            find: before
                .find
                .checked_add(1)
                .ok_or("正式Source查询输入代次耗尽")?,
            ..before
        };
        show_native_markdown_find_bar(true)?;
        let (bar, frame) = with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("正式Source前没有文稿")?;
            let paged = editor;
            let same_input = || {
                workload.current(surface, &original)
                    && paged
                        .text_view
                        .input_state()
                        .is_ok_and(|actual| actual == original.opening_input)
            };
            let current = || same_input() && before.permits(surface);
            if !current() {
                return Err("正式Source显示Find时原文稿或完整输入已变化".into());
            }
            let bar = editor
                .find_bar()
                .borrow()
                .clone()
                .ok_or("正式Source前未建立Find")?;
            let field = bar.fields()[0].clone();
            let input = native_markdown_table_intent_step(&current, || field.currentEditor())
                .map_err(|_| "正式Source取得Find编辑器时原输入已撤销")?
                .and_then(|editor| editor.downcast::<NSTextView>().ok())
                .ok_or("正式Find未取得真实字段editor")?;
            let units = native_markdown_table_intent_step(&current, || input.string().length())
                .map_err(|_| "正式Source读取Find长度时原输入已撤销")?;
            native_markdown_table_intent_step(&current, || {
                input.setSelectedRange(NSRange::new(0, units));
            })
            .map_err(|_| "正式Source设置Find选区时原输入已撤销")?;
            let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*input);
            let query = native_markdown_table_intent_step(&current, || NSString::from_str(QUERY))
                .map_err(|_| "正式Source建立有限查询时原输入已撤销")?;
            // SAFETY: 实际字段editor在主线程存活，载荷为有限NSString；NSNotFound按协议使用其真实选区。
            unsafe {
                client.insertText_replacementRange(&query, NSRange::new(NSNotFound as usize, 0));
            }
            let after = || same_input() && expected.permits(surface);
            native_markdown_table_intent_step(&after, || input.setSelectedRange(FIELD_RANGE))
                .map_err(|_| "正式Source查询没有产生准确一次原输入变化")?;
            // 原输入事务已结束，下面只绑定其完整CPU终点；尚未Ready不等于失败，也不能
            // 交给441直接抓Scene。字段值和焦点仍由原模式后的verify_mode逐项核对。
            let frame = MemoryFrameObservation::capture(
                surface,
                &original,
                expected,
                original.opening_input,
            )?;
            Ok((Rc::downgrade(&bar), frame))
        })?;
        let previous = workload.cpu_frame.replace(Some(frame));
        drop(previous); // 观察只持原弱身份；其析构不跨工作负载RefMut或Native输入栈。
        Ok(Self {
            original_file: std::fs::File::open(&workload.path)
                .map_err(|error| error.to_string())?,
            original,
            bar,
            replaced_bytes: 0..0,
            selection: None,
            frame: Rc::new_cyclic(|weak| SourceFrame {
                weak: weak.clone(),
                body: RefCell::new(std::rc::Weak::new()),
                generation: Cell::new(0),
                mode: Cell::new(MarkdownPresentationMode::Source),
                fold_revision: Cell::new(None),
                failure: RefCell::new(None),
                key: RefCell::new(None),
                signal: RefCell::new(None),
            }),
            attachments: None,
            viewport: RefCell::new(None),
            folding: None,
            pending_mode_input: None,
        })
    }

    /// 同一Find栏真实改变高度后立即点击原按钮；整个同步动作不借旧Pending几何建立位置期待。
    /// cancel先请求Source再选回原Preview；接受路径只请求Source，后态Find+1在动作前明确声明。
    fn begin_pending_mode(
        &mut self,
        workload: &NativeMarkdownMemoryWorkload,
        cancel: bool,
    ) -> Result<(), String> {
        let (before, old_height) = with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let epoch = MemoryActionEpoch::capture(surface);
            let editor = surface.editor.as_ref().ok_or("Pending模式前没有原文稿")?;
            let paged = editor;
            let input = paged
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?;
            if input != self.original.opening_input {
                return Err("Pending模式不能以改变后的Input重新开始".into());
            }
            let before = PendingModeInput {
                input,
                epoch,
                mode: MarkdownPresentationMode::InlinePreview,
            };
            if !self.pending_mode_current(surface, before) {
                return Err("Pending模式前原文稿、模式或输入已变化".into());
            }
            let height = editor.scroll_view().frame().size.height;
            if !height.is_finite() || height <= 0.0 || !self.pending_mode_current(surface, before) {
                return Err("Pending模式前原高度或输入不稳定".into());
            }
            Ok((before, height))
        })?;
        let expected = PendingModeInput {
            mode: if cancel {
                MarkdownPresentationMode::InlinePreview
            } else {
                MarkdownPresentationMode::Source
            },
            epoch: MemoryActionEpoch {
                find: before
                    .epoch
                    .find
                    .checked_add(u64::from(!cancel))
                    .ok_or("Pending模式Find代次耗尽")?,
                ..before.epoch
            },
            ..before
        };
        // 440显示替换栏；第一轮隐藏替换行，第二轮再显示，始终复用同一栏和真实scroll布局。
        show_native_markdown_find_bar(!cancel)?;
        let (controller, button) = with_active_native_markdown_surface(|surface| {
            if !self.pending_mode_current(surface, before) {
                return Err("Find高度变化撤销原模式输入".into());
            }
            let editor = surface.editor.as_ref().expect("原输入已核同一文稿");
            let paged = editor;
            let height = editor.scroll_view().frame().size.height;
            if height == old_height || !height.is_finite() || height <= 0.0 {
                return Err("Pending模式没有发生真实Find高度变化".into());
            }
            match paged.text_view.capture_ready_scene_key() {
                Err(crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending) => {}
                Ok(_) => return Err("高度变化后已经Ready，未覆盖真实Pending按钮流程".into()),
                Err(cause) => return Err(cause.to_string()),
            }
            if !self.pending_mode_current(surface, before) {
                return Err("取得Pending状态时原输入已变化".into());
            }
            let button = surface
                .controller
                .ivars()
                .presentation_mode_button
                .get()
                .expect("原窗口已装配模式按钮")
                .clone();
            if button.state() != objc2_app_kit::NSControlStateValueOff
                || !self.pending_mode_current(surface, before)
            {
                return Err("Pending流程前实际Preview按钮状态不符".into());
            }
            Ok((surface.controller.clone(), button))
        })?;
        button.setState(objc2_app_kit::NSControlStateValueOn);
        // SAFETY: 真实原controller和其自有NSButton存活；归还Active租约后执行生产selector一次。
        unsafe {
            let _: () = msg_send![&*controller, changeMarkdownPresentationMode: &*button];
        }
        with_active_native_markdown_surface(|surface| {
            if !self.pending_mode_current(surface, before)
                || button.state() != objc2_app_kit::NSControlStateValueOn
                || !self.pending_mode_current(surface, before)
            {
                return Err("Pending Source请求提前改变模式、Input或按钮".into());
            }
            Ok(())
        })?;
        if cancel {
            button.setState(objc2_app_kit::NSControlStateValueOff);
            // SAFETY: 同一个真实按钮立即选择原实际模式，必须撤掉尚待Ready的前一次Source动作。
            unsafe {
                let _: () = msg_send![&*controller, changeMarkdownPresentationMode: &*button];
            }
        }
        let frame = with_active_native_markdown_surface(|surface| {
            if !self.pending_mode_current(surface, before) {
                return Err("Pending按钮同步返回改变了原输入或实际模式".into());
            }
            let state = button.state();
            let expected_state = if cancel {
                objc2_app_kit::NSControlStateValueOff
            } else {
                objc2_app_kit::NSControlStateValueOn
            };
            if state != expected_state || !self.pending_mode_current(surface, before) {
                return Err("Pending按钮同步后态不符合原请求".into());
            }
            if cancel {
                // 这里只绑定本次Preview取消动作；Source接受不能用当下仍为Preview的模式签CPU终点。
                MemoryFrameObservation::capture(
                    surface,
                    &self.original,
                    expected.epoch,
                    expected.input,
                )
                .map(Some)
            } else {
                Ok(None)
            }
        })?;
        if let Some(frame) = frame {
            let old = workload.cpu_frame.replace(Some(frame));
            drop(old);
        }
        self.pending_mode_input = Some(expected);
        Ok(())
    }

    /// 只比较原Doc/Body、完整Input与预声明代次；原生getter后的重入不能成为新的测试基线。
    fn pending_mode_current(
        &self,
        surface: &NativeMarkdownWindowSurface,
        expected: PendingModeInput,
    ) -> bool {
        memory_workload_facts_current(surface, &self.original)
            && expected.epoch.permits(surface)
            && surface.editor.as_ref().is_some_and(|editor| {
                editor.presentation_mode() == expected.mode
                    && editor
                        .text_view
                        .input_state()
                        .is_ok_and(|actual| actual == expected.input)
            })
            && expected.epoch.permits(surface)
    }

    /// 真实完整帧之后再核模式和原字段；取消阶段尤其要证明晚到Source动作没有改掉Preview。
    fn verify_pending_mode(&self) -> Result<(), String> {
        let expected = self
            .pending_mode_input
            .ok_or("Pending模式检查缺少原动作后态")?;
        self.verify_mode(expected.mode == MarkdownPresentationMode::Source)?;
        with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            if !self.pending_mode_current(surface, expected) {
                return Err("Pending模式完成后原完整输入或代次已变".into());
            }
            let button = surface
                .controller
                .ivars()
                .presentation_mode_button
                .get()
                .expect("原窗口已装配模式按钮");
            let state = button.state();
            let desired = if expected.mode == MarkdownPresentationMode::Source {
                objc2_app_kit::NSControlStateValueOn
            } else {
                objc2_app_kit::NSControlStateValueOff
            };
            if state != desired || !self.pending_mode_current(surface, expected) {
                return Err("Pending终点按钮未保持实际模式".into());
            }
            Ok(())
        })
    }

    /// 模式流只读；独立原FD逐块比对完整canonical，不把当前正文复制为期待结果，也不产生新文件。
    fn verify_pending_mode_data(&mut self) -> Result<(), String> {
        let expected = self.pending_mode_input.ok_or("Pending字节检查缺少原输入")?;
        with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            if !self.pending_mode_current(surface, expected) {
                return Err("Pending字节检查前原输入已撤销".into());
            }
            let editor = surface.editor.as_ref().expect("原输入已核原文稿");
            let document = editor.document().borrow();
            if document.can_undo()
                || document.can_redo()
                || document.selection() != self.original.opening_selection
            {
                return Err("只读Pending模式改变了原选择或历史".into());
            }
            let mut comparison = SourceComparison {
                original: &mut self.original_file,
                replaced: 0..0,
                inserted: &[],
                written: 0,
                scratch: [0; 64 * 1024],
            };
            document
                .write_to(&mut comparison)
                .map_err(|cause| cause.to_string())?;
            if comparison.written != self.original.disk.2 as usize {
                return Err("Pending模式原canonical字节长度变化".into());
            }
            Ok(())
        })
    }

    /// 真实NSButton进入原controller selector；测试不直接替换enum或调用内部模式工厂。
    fn change_mode(&self, source: bool) -> Result<(), String> {
        let (controller, button) = with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            *self.viewport.borrow_mut() = Some(Rc::new(
                viewport::ModeViewportExpectation::capture(surface, source)?,
            ));
            let button = surface
                .controller
                .ivars()
                .presentation_mode_button
                .get()
                .expect("正式窗口已装配模式按钮")
                .clone();
            Ok((surface.controller.clone(), button))
        })?;
        // 原selector需要从真实Active槽捕获文稿；先归还租约，不让driver自己的借出态拒绝合法点击。
        button.setState(if source {
            objc2_app_kit::NSControlStateValueOn
        } else {
            objc2_app_kit::NSControlStateValueOff
        });
        // SAFETY: 实际controller和同窗口原按钮均存活，selector ABI与生产点击完全相同。
        unsafe {
            let _: () = msg_send![&*controller, changeMarkdownPresentationMode: &*button];
        }
        Ok(())
    }

    /// 只沿原弱文稿和controller身份消费；模式变化不授权后继同名笔记承接本场景。
    fn owns(&self, surface: &NativeMarkdownWindowSurface) -> Result<(), String> {
        if !native_markdown_controller_has_foreground_authority(&surface.controller)
            || std::ptr::from_ref(&*surface.controller).addr() != self.original.owner
            || !surface.editor.as_ref().is_some_and(|editor| {
                std::ptr::eq(
                    self.original.document.as_ptr(),
                    Rc::as_ptr(editor.document()),
                )
            })
        {
            return Err("正式Source场景的原窗口或文稿已变化".into());
        }
        Ok(())
    }

    /// 同一Find值、原字段、真实响应者与NSRange都要保留，控件Rc相等不能替代输入关系。
    fn verify_mode(&self, source: bool) -> Result<(), String> {
        with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let editor = surface.editor.as_ref().ok_or("正式模式缺少文稿")?;
            let expected_mode = if source {
                MarkdownPresentationMode::Source
            } else {
                MarkdownPresentationMode::InlinePreview
            };
            if editor.presentation_mode() != expected_mode {
                return Err("controller没有安装所选展示模式".into());
            }
            // 正式具体类型只持一个 Body；下面继续验证实际挂载、原输入和真实模式帧。
            // SAFETY: 活scroll由当前editor持有；这里只核真实挂载关系，不遍历未知Native子树。
            if unsafe { editor.scroll_view().superview() }.is_none() {
                return Err("正式模式的滚动图未挂载".into());
            }
            let bar = editor
                .find_bar()
                .borrow()
                .clone()
                .ok_or("模式交接丢失Find栏")?;
            if !std::rc::Weak::ptr_eq(&Rc::downgrade(&bar), &self.bar) || !bar.is_visible() {
                return Err("模式交接重建或隐藏了原Find栏".into());
            }
            let field = bar.fields()[0].clone();
            if field.stringValue().to_string() != QUERY {
                return Err("模式交接改变了原查询值".into());
            }
            let input = field
                .currentEditor()
                .and_then(|editor| editor.downcast::<NSTextView>().ok())
                .ok_or("模式交接丢失原Find字段editor")?;
            let responder = surface
                .panel
                .firstResponder()
                .ok_or("模式交接没有实际响应者")?;
            if std::ptr::from_ref(&*responder).cast::<()>()
                != std::ptr::from_ref(&*input).cast::<()>()
                || input.selectedRange() != FIELD_RANGE
            {
                // 只在原严格失败处分辨焦点与选区；不读取查询/正文，不改变后态或领取后继输入。
                #[cfg(feature = "native-markdown-runtime-probe")]
                eprintln!(
                    "native_mode_find_focus phase=verify source={source} responder_same={} expected_range={FIELD_RANGE:?} actual_range={:?} key_window={} app_active={}",
                    std::ptr::from_ref(&*responder).addr() == std::ptr::from_ref(&*input).addr(),
                    input.selectedRange(),
                    surface.panel.isKeyWindow(),
                    NSApplication::sharedApplication(surface.panel.mtm()).isActive(),
                );
                return Err("模式交接没有保持原Find焦点及NSRange".into());
            }
            if !source {
                self.verify_mode_viewport(surface)?;
            }
            Ok(())
        })
    }

    /// 真实Source帧或Preview原终点才消费一次位置期望，后续文字编辑不反复比较过期坐标。
    fn verify_mode_viewport(&self, surface: &NativeMarkdownWindowSurface) -> Result<(), String> {
        let expected = self.viewport.borrow_mut().take();
        if let Some(expected) = expected {
            expected.verify(surface)?;
        }
        Ok(())
    }

    /// 模式不再销毁 Native 图；只在原 Producer 的真实 Source/目录/worker 归还后交付终点。
    fn observe_retired(&self, observer: impl FnOnce() + 'static) -> Result<(), String> {
        with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let editor = surface.editor.as_ref().ok_or("释放观察缺少正式editor")?;
            editor.body.verify_mode_retirement()
        })?;
        observer(); // 原 owner 租约已归还；只交付已真实完成的同步退休结果，不造延时或空队列证明。
        Ok(())
    }

    fn verify_retired(&self) -> Result<(), String> {
        with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let editor = surface.editor.as_ref().ok_or("释放检查缺少正式editor")?;
            editor.body.verify_mode_retirement()
        })
    }

    /// 在实际帧已经发布时核同key；否则只登记原Body下一完整帧，没有轮询/主动重绘。
    fn observe_frame(&self, edits: u64, observer: impl FnOnce() + 'static) -> Result<(), String> {
        with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let source = surface
                .editor
                .as_ref()
                .ok_or("正式Source帧观察没有实际分页后端")?;
            if let Some((published, paints)) = source.text_view.probe_cpu_published_frame()
                && published.request.input.generation == self.original.generation + edits
                && published.presentation_mode == MarkdownPresentationMode::Source
                && self
                    .frame
                    .fold_revision
                    .get()
                    .is_none_or(|revision| published.request.input.fold_revision == revision)
                && paints != 0
                && source.text_view.cpu_paint_pending() == Some(false)
                && source
                    .text_view
                    .capture_ready_scene_key()
                    .is_ok_and(|ready| ready.same(&published))
            {
                *self.frame.key.borrow_mut() = Some(published);
                observer();
                return Ok(());
            }
            *self.frame.key.borrow_mut() = None;
            self.frame.generation.set(self.original.generation + edits);
            // 换模式保留同一generation；这里所有consumer明确等待Source，不能拿登记时尚在
            // Preview的当前模式作为期待。真实Preview中间帧只沿原一次发布通知续等，不冒充终点。
            self.frame.mode.set(MarkdownPresentationMode::Source);
            *self.frame.body.borrow_mut() = Rc::downgrade(&source.body);
            *self.frame.failure.borrow_mut() = None;
            *self.frame.signal.borrow_mut() = Some(Box::new(observer));
            let erased: Rc<dyn NativeMarkdownPagedBodyPaintObserver> = self.frame.clone();
            source
                .body
                .observe_next_cpu_frame(Rc::downgrade(&erased))
                .map_err(|cause| cause.to_string())
        })
    }

    fn verify_frame(&self, edits: u64) -> Result<(), String> {
        if let Some(cause) = self.frame.failure.borrow_mut().take() {
            return Err(cause);
        }
        let key = self
            .frame
            .key
            .borrow()
            .clone()
            .ok_or("正式Source没有实际完整帧交付")?;
        with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let source = surface
                .editor
                .as_ref()
                .ok_or("正式Source完整帧缺少实际后端")?;
            if key.request.input.generation != self.original.generation + edits
                || key.presentation_mode != MarkdownPresentationMode::Source
                || key.presentation_mode != source.body.presentation_mode()
                || self
                    .frame
                    .fold_revision
                    .get()
                    .is_some_and(|revision| key.request.input.fold_revision != revision)
                || source.model.document().borrow().generation() != self.original.generation + edits
                || !source
                    .text_view
                    .capture_ready_scene_key()
                    .map_err(|cause| cause.to_string())?
                    .same(&key)
                || source.text_view.cpu_paint_pending() != Some(false)
            {
                return Err("正式Source的帧已被后继输入或几何撤销".into());
            }
            if let Some(cause) = source.body.take_failure() {
                return Err(cause.to_string());
            }
            self.verify_mode_viewport(surface)?;
            Ok(())
        })
    }

    /// 原查找必须实际选中独立夹具的标记；不从新选择反推期望再构造输入范围。
    fn insert(&mut self) -> Result<(), String> {
        self.verify_frame(0)?;
        with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let editor = surface.editor.as_ref().ok_or("正式输入缺少文稿")?;
            let source = editor;
            let selection = editor.document().borrow().selection();
            let range = selection.anchor_utf16.min(selection.head_utf16)
                ..selection.anchor_utf16.max(selection.head_utf16);
            if range != self.original.table_match {
                return Err("正式Source查找未选中原标记".into());
            }
            let key = source
                .text_view
                .capture_ready_scene_key()
                .map_err(|cause| cause.to_string())?;
            source
                .text_view
                .verify_find_target_visible(&key, NSRange::new(range.start, range.len()))
                .map_err(|cause| cause.to_string())?;
            let document = editor.document().borrow();
            self.replaced_bytes = document
                .byte_offset_for_utf16(range.start)
                .map_err(|cause| cause.to_string())?
                ..document
                    .byte_offset_for_utf16(range.end)
                    .map_err(|cause| cause.to_string())?;
            self.selection = Some(selection);
            drop(document);
            if !surface
                .panel
                .makeFirstResponder(Some(source.text_view.as_super()))
            {
                return Err("正式Source输入未取得真实焦点".into());
            }
            let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**source.text_view);
            // SAFETY: 正式PagedView本身实现该协议；有限NSString和NSNotFound保留真实Input当前选区语义。
            unsafe {
                client.insertText_replacementRange(
                    &NSString::from_str(INSERTED),
                    NSRange::new(NSNotFound as usize, 0),
                );
            }
            Ok(())
        })
    }

    /// 原工具栏Undo/Redo仍走controller，逐笔history结果在后继帧事件独立核对。
    fn history(&self, redo: bool) -> Result<(), String> {
        let (events, mtm) = with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let editor = surface.editor.as_ref().ok_or("正式Source历史没有原正文")?;
            editor
                .document()
                .borrow()
                .byte_offset_for_utf16(0)
                .map_err(|cause| cause.to_string())?;
            let before = editor
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?;
            let current = || {
                self.owns(surface).is_ok()
                    && editor
                        .text_view
                        .input_state()
                        .is_ok_and(|actual| actual == before)
            };
            let controls = surface
                .controller
                .ivars()
                .workspace_controls
                .get()
                .expect("正式窗口已装配工具栏");
            let button = controls[super::super::workspace_actions::NATIVE_MARKDOWN_TOOLBAR_START
                + usize::from(redo)]
            .clone()
            .downcast::<NSButton>()
            .expect("原Undo/Redo工具栏装配为NSButton");
            if button.tag() != if redo { 23 } else { 22 }
                || !button.isEnabled()
                || !button.refusesFirstResponder()
            {
                return Err("正式Source原Undo/Redo按钮不可用或会夺取焦点".into());
            }
            let result = super::button_mouse::prepare(&button, &surface.panel, &current);
            // Native命中/坐标转换后再取真实Source首错和完整Input；不能把来源失效
            // 隐成鼠标按钮不可用。所有Doc/Surface借用在NSApp接纳事件前归还。
            editor
                .document()
                .borrow()
                .byte_offset_for_utf16(0)
                .map_err(|cause| cause.to_string())?;
            self.owns(surface)?;
            if editor
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?
                != before
            {
                return Err("正式Source历史鼠标准备期间完整Input已变化".into());
            }
            Ok((result?, surface.panel.mtm()))
        })?;
        // Down/Up都构造并核准后才入同一原窗口队列；NSButton真实tracking消费Up，
        // 不是直接performClick或调用action。后继仍用原generation/历史/CPU终点确认一次操作。
        super::button_mouse::post(events, mtm);
        Ok(())
    }

    /// 全canonical通过原write_to流式比原FD与本次唯一替换，不用实现自身的revision作唯一oracle。
    fn verify_edit(
        &mut self,
        workload: &NativeMarkdownMemoryWorkload,
        edits: u64,
    ) -> Result<(), String> {
        self.verify_frame(edits)?;
        let selection = self.selection.ok_or("正式Source没有原查找选择")?;
        let range = selection.anchor_utf16.min(selection.head_utf16)
            ..selection.anchor_utf16.max(selection.head_utf16);
        let inserted = !edits.is_multiple_of(2);
        with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let editor = surface.editor.as_ref().ok_or("正式历史缺少文稿")?;
            let document = editor.document().borrow();
            let expected_selection = if inserted {
                MarkdownTextSelection::caret(range.start + INSERTED.encode_utf16().count())
            } else {
                selection
            };
            let expected_length = self.original.length - if inserted { range.len() } else { 0 }
                + if inserted {
                    INSERTED.encode_utf16().count()
                } else {
                    0
                };
            if document.generation() != self.original.generation + edits
                || document.selection() != expected_selection
                || document.utf16_len() != expected_length
                || document.can_undo() != inserted
                || document.can_redo() == inserted
            {
                let cause = format!(
                    "正式Source逐笔历史不符：edits={edits} generation={}/{} selection={:?}/{expected_selection:?} units={}/{} undo={}/{} redo={}/{} dirty={}",
                    document.generation(),
                    self.original.generation + edits,
                    document.selection(),
                    document.utf16_len(),
                    expected_length,
                    document.can_undo(),
                    inserted,
                    document.can_redo(),
                    !inserted,
                    document.is_dirty(),
                );
                // 先输出本次真实标量首因再走原清理；后续Native异常不能使诊断只剩关闭次因。
                eprintln!("native_source_session_failure {cause}");
                return Err(cause);
            }
            let replaced = if inserted {
                self.replaced_bytes.clone()
            } else {
                0..0
            };
            let text = if inserted { INSERTED.as_bytes() } else { &[] };
            let expected_bytes = self.original.disk.2 as usize - replaced.len() + text.len();
            let mut comparison = SourceComparison {
                original: &mut self.original_file,
                replaced,
                inserted: text,
                written: 0,
                scratch: [0; 64 * 1024],
            };
            document
                .write_to(&mut comparison)
                .map_err(|cause| cause.to_string())?;
            if comparison.written != expected_bytes {
                return Err("正式Source流式完整性核验的总字节不符".into());
            }
            drop(document);
            if edits == 4 {
                surface.save_current_document()?;
                let editor = surface.editor.as_ref().ok_or("正式保存后原文稿缺失")?;
                let document = editor.document().borrow();
                if document.is_dirty()
                    || document
                        .current_revision()
                        .map_err(|cause| cause.to_string())?
                        != self.original.revision
                {
                    return Err("正式Source恢复保存未保原完整revision".into());
                }
                drop(document);
                let mut saved =
                    std::fs::File::open(&workload.path).map_err(|cause| cause.to_string())?;
                let metadata = saved.metadata().map_err(|cause| cause.to_string())?;
                if metadata.len() != self.original.disk.2 {
                    return Err("正式保存文件长度不同于原合成文稿".into());
                }
                let mut saved_bytes = [0; 64 * 1024];
                let mut original_bytes = [0; 64 * 1024];
                self.original_file
                    .seek(SeekFrom::Start(0))
                    .map_err(|cause| cause.to_string())?;
                loop {
                    let read = saved
                        .read(&mut saved_bytes)
                        .map_err(|cause| cause.to_string())?;
                    if read == 0 {
                        break;
                    }
                    self.original_file
                        .read_exact(&mut original_bytes[..read])
                        .map_err(|cause| cause.to_string())?;
                    if saved_bytes[..read] != original_bytes[..read] {
                        return Err("正式保存磁盘字节不同于原合成文稿".into());
                    }
                }
                use std::os::unix::fs::MetadataExt;
                let mut facts = workload.facts.borrow_mut();
                let facts = facts.as_mut().ok_or("正式保存缺少原关闭事实")?;
                facts.generation = self.original.generation + 4;
                facts.disk = (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.len(),
                    metadata.mtime(),
                    metadata.mtime_nsec(),
                );
            }
            Ok(())
        })
    }

    fn focus_find(&self) -> Result<(), String> {
        with_active_native_markdown_surface(|surface| {
            self.owns(surface)?;
            let bar = self.bar.upgrade().ok_or("正式Find原栏已释放")?;
            let field = bar.fields()[0].clone();
            // SAFETY: 同一实际栏仍由原editor拥有，测试只重新激活该字段，不重建客户图。
            unsafe {
                field.selectText(None);
            }
            let input = field
                .currentEditor()
                .and_then(|input| input.downcast::<NSTextView>().ok())
                .ok_or("正式Find字段未重新获得editor")?;
            input.setSelectedRange(FIELD_RANGE);
            Ok(())
        })
    }
}

/// 比较器仅维护原FD坐标与64KiB scratch；整篇canonical和独立原文件都沿流读，不创建全文副本。
struct SourceComparison<'a> {
    original: &'a mut std::fs::File,
    replaced: std::ops::Range<usize>,
    inserted: &'a [u8],
    written: usize,
    scratch: [u8; 64 * 1024],
}
impl Write for SourceComparison<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut at = 0;
        while at < bytes.len() {
            let position = self.written + at;
            let count = if position >= self.replaced.start
                && position < self.replaced.start + self.inserted.len()
            {
                let from = position - self.replaced.start;
                let count = (bytes.len() - at).min(self.inserted.len() - from);
                if bytes[at..at + count] != self.inserted[from..from + count] {
                    return Err(io::Error::other("Source新增片段与原输入不同"));
                }
                count
            } else {
                let original_at = if position < self.replaced.start {
                    position
                } else {
                    position - self.inserted.len() + self.replaced.len()
                };
                let before_insert = if position < self.replaced.start {
                    self.replaced.start - position
                } else {
                    usize::MAX
                };
                let count = (bytes.len() - at)
                    .min(before_insert)
                    .min(self.scratch.len());
                self.original.seek(SeekFrom::Start(original_at as u64))?;
                self.original.read_exact(&mut self.scratch[..count])?;
                if bytes[at..at + count] != self.scratch[..count] {
                    return Err(io::Error::other("Source未编辑部分与原文件不同"));
                }
                count
            };
            at += count;
        }
        self.written += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl NativeMarkdownMemoryWorkload {
    pub(crate) fn exercise_source_session(&self, stage: i16) -> Result<(), String> {
        if stage == 440 {
            *self.source_session.borrow_mut() = Some(SourceSession::begin(self)?);
            return Ok(());
        }
        let mut case = self
            .source_session
            .borrow_mut()
            .take()
            .ok_or("正式Source场景owner缺失")?;
        let result = match stage {
            // Find打开/输入已让旧Scene失效；只能消费440所绑的同一完整CPU帧后再捕模式锚点。
            441 => self
                .verify_memory_frame()
                .and_then(|()| case.change_mode(true)),
            442 => case.verify_mode(true),
            443 | 446 => case.verify_frame(0),
            448 => case.verify_frame(1),
            450 => case.verify_frame(2),
            452 => case.verify_frame(3),
            454 | 460 => case.verify_frame(4),
            445 => case.verify_retired().and_then(|()| {
                perform_native_markdown_find_action(MarkdownFindAction::Next).map(|_| ())
            }),
            447 => case.insert(),
            449 => case.verify_edit(self, 1).and_then(|()| case.history(false)),
            451 => case.verify_edit(self, 2).and_then(|()| case.history(true)),
            453 => case.verify_edit(self, 3).and_then(|()| case.history(false)),
            455 => case.verify_edit(self, 4),
            456 => case.focus_find().and_then(|()| case.change_mode(false)),
            457 => case.verify_mode(false),
            458 => case.verify_retired().and_then(|()| case.change_mode(true)),
            459 => case.verify_mode(true),
            560 => case.begin_pending_mode(self, true),
            561 => self
                .verify_memory_frame()
                .and_then(|()| case.verify_pending_mode()),
            562 => case.begin_pending_mode(self, false),
            563 => case
                .verify_frame(0)
                .and_then(|()| case.verify_pending_mode()),
            564 => case
                .verify_retired()
                .and_then(|()| case.verify_pending_mode_data()),
            565 => case
                .verify_pending_mode()
                .and_then(|()| self.verify_unchanged()),
            461 => case.begin_attachments(self),
            462 => case
                .verify_attachment_prefix(self, 7, &format!("alpha{}后续 beta ", attachments::LINK))
                .and_then(|()| case.history(false)),
            463 => case
                .verify_attachment_prefix(self, 8, &format!("alpha{} beta ", attachments::LINK))
                .and_then(|()| case.history(false)),
            464 => case
                .verify_attachment_prefix(self, 9, attachments::PREFIX)
                .and_then(|()| case.history(true)),
            465 => case
                .verify_attachment_prefix(self, 10, &format!("alpha{} beta ", attachments::LINK))
                .and_then(|()| case.history(false)),
            466 => case
                .verify_attachment_prefix(self, 11, attachments::PREFIX)
                .and_then(|()| case.history(false)),
            467 => case
                .verify_attachment_prefix(self, 12, "")
                .and_then(|()| case.save_attachment_restore(self, 12))
                .and_then(|()| case.enqueue_missing_attachment()),
            468 => case
                .verify_attachment_prefix(self, 13, attachments::AFTER_FAILURE)
                .and_then(|()| case.verify_attachment_rejection())
                .and_then(|()| case.history(false)),
            469 => case
                .verify_attachment_prefix(self, 14, "")
                .and_then(|()| case.save_attachment_restore(self, 14))
                .and_then(|()| case.queue_canceled_attachment()),
            _ => Err("未知正式Source场景事件".into()),
        };
        *self.source_session.borrow_mut() = Some(case);
        result?;
        // 同一同步栈先接受原Return，旧附件任务不能抢在关闭前执行；request_return也先归还本场景FD/目录。
        if matches!(stage, 469 | 565) {
            self.request_return()
        } else {
            Ok(())
        }
    }

    /// 回调只投原事件；真正完整帧和旧图释放在各自原owner上触发，不用另一套等待器。
    pub(crate) fn observe_source_session_terminal(
        &self,
        stage: i16,
        observer: impl FnOnce() + 'static,
    ) -> Result<(), String> {
        let case = self
            .source_session
            .borrow_mut()
            .take()
            .ok_or("正式Source终点缺少原场景")?;
        let result = (|| {
            if matches!(stage, 441 | 561) {
                let frame = self
                    .cpu_frame
                    .borrow()
                    .clone()
                    .ok_or("正式Source切换前缺少原Find完整帧观察")?;
                return frame.observe(observer);
            }
            if matches!(stage, 445 | 458 | 564) {
                return case.observe_retired(observer);
            }
            if stage == 457 {
                let expected = case
                    .viewport
                    .borrow()
                    .clone()
                    .ok_or("正式预览终点缺少原位置期望")?;
                return with_active_native_markdown_surface(|surface| {
                    case.owns(surface)?;
                    expected.observe_preview_terminal(&surface.controller, observer)
                });
            }
            let edits = match stage {
                // 模式验证本身也必须等原Source帧；后继443/460继续严格核同key、
                // 原完整Input与viewport，不从登记时尚存的Preview模式取得替代终点。
                442 | 443 | 446 | 563 => 0,
                448 => 1,
                450 => 2,
                452 => 3,
                454 | 459 | 460 => 4,
                462..=469 => {
                    u64::try_from(stage - 455).expect("正式附件事件连续对应七至十四次字符提交")
                }
                _ => return Err("未声明的正式Source帧终点".into()),
            };
            case.observe_frame(edits, observer)
        })(); // 任一注册拒绝也先归还原场景与FD，再让driver执行真实清理。

        *self.source_session.borrow_mut() = Some(case);
        result
    }
}
