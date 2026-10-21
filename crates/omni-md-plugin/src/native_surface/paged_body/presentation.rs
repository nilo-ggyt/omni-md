//! 展示模式只替换当前正文的排版续体；文稿、输入客户、历史及 Native 根始终属于同一会话。

use super::NativeMarkdownPagedBodyError as Error;
use super::*;
use crate::native_surface::editor::viewport::ModeSourceAnchor;
use omni_markdown_core::MarkdownPresentationMode;

/// 与普通 prepare 共用同一个同步准入；Native 重入只登记 deferred，不并行领取第二个 Producer。
pub(super) struct PresentationUse<'a>(pub(super) &'a Cell<bool>);

impl Drop for PresentationUse<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

impl NativeMarkdownPagedBody {
    /// 下一次模式动作必须先证明上一份派生排版已真实归还；共享的当前文稿和基础字体不在退休集合中。
    pub(in crate::native_surface) fn verify_mode_retirement(
        &self,
    ) -> std::result::Result<(), String> {
        let slot = self.mode_retirement.borrow();
        let Some(receipt) = slot.as_ref() else {
            return Ok(()); // 首次打开尚未退休任何 Producer，不制造一个虚假的已关闭对象。
        };
        if !receipt.source.payload_released() {
            return Err("上一模式的排版 Source 仍未归还".into());
        }
        for reader in receipt
            .reader
            .iter()
            .chain(std::iter::once(&receipt.writer))
        {
            let actual = reader.snapshot();
            if actual.file_open
                || actual.mapped_bytes != 0
                || actual.buffer_bytes != 0
                || actual.cleanup_failed
                || actual.reader_failed
                || !actual.source_payload_released
                || actual.first_source_errno.is_some()
            {
                return Err(format!("上一模式的排版目录仍未归还：{actual:?}"));
            }
        }
        let worker = receipt
            .worker
            .snapshot()
            .ok_or("上一模式的 worker 观察已损坏")?;
        if !worker.is_released() {
            return Err(format!("上一模式的整形 worker 仍未归还：{worker:?}"));
        }
        for cell in &receipt.cells {
            cell.verify_released()?; // 格显示 Source、映射和实际目录仍沿各自原收据核验。
        }
        // 字号或模式可以更换 Core payload，却仍合法共享同一冻结映射。
        // 只认当前实际字体持有的原映射身份；关闭已先清空 fonts，届时全部映射仍须真实归还。
        let current_mappings = self
            .fonts
            .borrow()
            .iter()
            .map(|font| font.native().bytes_observer())
            .collect::<Vec<_>>();
        for (bytes, payload) in &receipt.fonts {
            let mapping_retained = current_mappings
                .iter()
                .any(|current| current.same_mapping_resource(bytes));
            if !payload.payload_released()
                || bytes.failed()
                || (!mapping_retained && (!bytes.unmap_succeeded() || bytes.mapped_bytes() != 0))
            {
                return Err("上一模式的样式字体仍持有映射或文件".into());
            }
        }
        Ok(())
    }

    /// 只有原可见字形和完整输入票仍有效时才接纳切换；准备失败不先销毁可用的旧排版。
    pub(in crate::native_surface) fn change_presentation_mode(
        &self,
        mode: MarkdownPresentationMode,
        scroll: &NSScrollView,
        anchor: ModeSourceAnchor,
        key: &NativeMarkdownPagedSceneKey,
    ) -> Result<()> {
        self.preflight_release()?;
        self.verify_mode_retirement()
            .map_err(|_| Error::ResourceReleaseFailed)?;
        if self.input.is_cell()
            || self.active_cell.borrow().is_some()
            || self.pending_cell.borrow().is_some()
            || self.has_pending_input()
        {
            return Err(Error::Busy); // 已接纳的文字和单格事务必须由原 owner 完成，不能用换模式取消。
        }
        let previous = self.presentation_mode();
        if previous == mode {
            return Ok(());
        }
        let view = self.view()?;
        let original = || {
            self.current_body()
                && self.presentation_mode() == previous
                && anchor.current_document()
                && view
                    .capture_ready_scene_key()
                    .is_ok_and(|actual| actual.same(key))
        };
        if !original() {
            return Err(ViewError::Revoked.into());
        }
        // 后面的 AppKit getter 可以交付原通知；先锁住同一同步准备准入，再保留实际旧输入。
        self.preparing.set(true);
        let preparation = PresentationUse(&self.preparing);
        let result = (|| {
            let current_scroll = view.enclosingScrollView();
            if !original()
                || !current_scroll
                    .as_deref()
                    .is_some_and(|actual| std::ptr::eq(actual, scroll))
            {
                return Err(ViewError::Revoked.into());
            }
            drop(current_scroll);
            // 只在接纳前的字体准备栈保原 Scene，用于保留 Native 回调中的 Source 首错。
            // 进入退休前明确归还，不能因认证字体而把旧 Reader 跨模式保活。
            let scene = self.scene.borrow().clone().ok_or(ViewError::Pending)?;
            if !scene.key().same(key) {
                return Err(ViewError::Revoked.into());
            }
            // 产品两种用途明确选择目标 Native base；通用 Body 仍沿原传入字体。
            // 捕获只借本 Body 的有限活字体，仍逐实例核真实来源/轴/字号/平台推进。
            let captured = self.capture_base_font(mode, &original);
            scene.validate_ready(&original)?;
            let base = captured?;
            let typography = (mode == MarkdownPresentationMode::InlinePreview)
                .then(|| semantics::Typography::new(base.native_font()))
                .transpose();
            scene.validate_ready(&original)?;
            let typography = typography?;
            if !original() {
                return Err(ViewError::Revoked.into());
            }
            // 目标 wrapper 记录对应模式的真实行度量；共享 payload 和映射只来自上方实际捕获收据。
            let registered = NativeMarkdownLineFont::register(base, &self.catalog, &original);
            scene.validate_ready(&original)?;
            let registered = registered?;
            let registered = if let Some(typography) = &typography {
                let captured =
                    registered.capture_preview_metrics(&typography.metrics_owner, &original);
                scene.validate_ready(&original)?;
                captured?
            } else {
                registered
            };
            drop(scene);
            if !original() {
                return Err(ViewError::Revoked.into());
            }
            let next_serial = self
                .serial
                .get()
                .checked_add(1)
                .ok_or(Error::CounterExhausted)?;
            let next_layout = self
                .layout_epoch
                .get()
                .checked_add(1)
                .ok_or(Error::CounterExhausted)?;
            let mut retired = self.resource_receipts()?;
            // 零号也可能因用途变化而退休，不能再跳过首项。只有与目标实际同 Core
            // payload 的旧字体继续存活；其它 payload 必须释放，共享映射另按当前 owner 核验。
            let target_authority = registered.native().core_font().authority();
            retired.fonts = self
                .fonts
                .borrow()
                .iter()
                .filter(|font| !font.native().core_font().permits(&target_authority))
                .map(|font| {
                    (
                        font.native().bytes_observer(),
                        font.native().core_font().resource_observer(),
                    )
                })
                .collect();
            // 从这里接纳本次切换：先撤旧导航和绘图 Reader，保原 Input 的多范围、亲和及折叠事实。
            // 模式动作已经结束真实 marked/单格事务；它不是一次文本替换，也不写文稿或 Undo。
            self.cancel_pending_find()?;
            self.cancel_word_delete()?;
            self.cancel_mode_viewport();
            self.revoke_source_target();
            drop(self.selection_reveal.borrow_mut().take());
            drop(self.pending_vertical.borrow_mut().take());
            drop(self.pending_page_scroll.borrow_mut().take());
            view.prepare_presentation_change()?;
            self.clear_scene()?;
            view.release_scene_readers()?;
            let producer = self.producer.borrow_mut().take();
            let worker = self.worker.borrow_mut().take();
            let cleanup = producer::merge_preparation_cleanup(
                producer.map_or(Ok(()), Producer::retire),
                worker.map_or(Ok(()), FontWorker::close),
            );
            *self.mode_retirement.borrow_mut() = Some(retired);
            if let Err(cause) = cleanup {
                // 实际资源归还失败后不建立新排版掩盖首错；正文仍可沿统一关闭边归还剩余资源。
                self.failed.set(true);
                return Err(cause);
            }
            self.scheduled.set(None);
            self.deferred.set(false);
            self.font_notices.borrow_mut().clear();
            if self.font_notice_failed.get() {
                self.failed.set(true);
                return Err(Error::WorkerFailed);
            }
            self.serial.set(next_serial);
            self.layout_epoch.set(next_layout);
            self.font_choices.borrow_mut().clear();
            let old_fonts = self.fonts.replace(vec![Rc::new(registered)]);
            let old_typography = self.typography.replace(typography);
            self.presentation.set(mode); // 原 Producer 已实际退场，不能让它误接相同 Input/宽度的新模式。
            drop((old_fonts, old_typography));
            // 模式的结构义务与旧输入的位置恢复分开：真实新输入可以取消旧锚点，
            // 却不能让已经接纳的 Preview 留源码 ruler，或让 Source 缺失同一行号附件。
            let mode_current = || self.current_body() && self.presentation_mode() == mode;
            let input_current = || {
                mode_current()
                    && self
                        .input
                        .state()
                        .is_ok_and(|state| state == key.request.input)
                    && anchor.current_document()
            };
            if !mode_current() {
                return Err(ViewError::Revoked.into());
            }
            let gutter = self.gutter.borrow().clone();
            if let Some(gutter) = gutter {
                // 标准 ruler 是同一正文会话的有限附件；预览清旧像素并隐藏，最终统一关闭才封闭共享页账。
                gutter
                    .set_mode_visibility(mode == MarkdownPresentationMode::Source, &mode_current)?;
            } else if mode == MarkdownPresentationMode::Source {
                let lines = self
                    .document
                    .borrow()
                    .as_ref()
                    .expect("模式动作只属于 Body 原文档")
                    .borrow()
                    .line_count();
                let gutter = Rc::new(super::super::gutter::paged::NativeMarkdownPagedGutter::new(
                    self.weak.clone(),
                    scroll,
                    &view,
                    lines,
                    self.mtm,
                )?);
                if !mode_current() {
                    return Err(ViewError::Revoked.into());
                }
                *self.gutter.borrow_mut() = Some(gutter.clone());
                gutter.mount()?; // owner 先归原槽，同步 clip 通知才能看见唯一的行号归属。
            }
            if !mode_current() {
                return Err(ViewError::Revoked.into());
            }
            let viewport = self
                .viewport
                .borrow()
                .clone()
                .expect("活动正文已装配原视口");
            viewport.synchronize()?;
            if !mode_current() {
                return Err(ViewError::Revoked.into());
            }
            if !input_current() {
                // 新输入已经拥有自己的选择/滚动位置；只继续同模式真实排版，不能恢复旧 anchor。
                return self.schedule();
            }
            // 同一个 Source 锚点在原 scroll 的新真实 lane 上恢复；绑定 controller 前不发布文首过渡帧。
            // 原同步守卫仍借同一个锚点；这里只移交它的有限弱文稿和坐标值，不复制 Scene 或正文。
            self.restore_mode_viewport(anchor.clone())
        })();
        drop(preparation);
        let deferred = self.deferred.replace(false);
        if (deferred || result.is_err()) && self.current_body() {
            // 接纳前失败可能已经撤掉旧 Scene；接纳后 Native 新输入也可能撤旧位置恢复。
            // 两者都只恢复当前同一 owner 的实际排版，不重放模式动作、文字输入或文稿编辑。
            self.schedule()?;
        }
        result
    }
}
