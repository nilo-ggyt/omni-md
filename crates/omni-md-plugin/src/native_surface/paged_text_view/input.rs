//! 唯一输入事务与有界协议读取；所有Core借用在NSInputContext或几何操作前结束。

use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, ClassType, DefinedClass};
use objc2_foundation::{NSAttributedString, NSPoint, NSRange, NSString};

use super::{
    NativeMarkdownPagedCharacterRect, NativeMarkdownPagedInputCommit,
    NativeMarkdownPagedInputOutcome, NativeMarkdownPagedTextView, NativeMarkdownPagedTextViewError,
    Origin, Result, SceneUse, bounded_viewport, finite_rect,
};

const SUBSTRING_UNITS: usize = 4096;

impl NativeMarkdownPagedTextView {
    /// 附件链接先由原Input发布，再确认同一provisional文件，最后才发可重入Native通知。
    /// Core拒绝时不调用确认闭包；调用方仍拥有原文件回滚责任，已提交显示失败不能删除附件。
    pub(in crate::native_surface) fn insert_attachment_link(
        &self,
        before: super::NativeMarkdownPagedInputState,
        text: &str,
        interpreted: bool,
        confirm_attachment: impl FnOnce(),
    ) -> Result<()> {
        let _use = crate::payload_use::PayloadUseGuard::enter(&self.ivars().depth);
        let mut origin = self.origin()?;
        origin.interpreted = interpreted; // 只有队首附件调用方传true；外部工具栏不能从共享depth冒领原Word收据。
        if origin.state != before {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        let commit =
            origin
                .input
                .insert_text_isolated(&permit, text, super::not_found(), &current)?;
        confirm_attachment(); // 本步只确认已经在canonical中的文件，不在显示回调之后才建立其持久归属。
        self.finish_edit_commit(&origin, commit)
    }

    /// 保存/模式交接仍由真实NSTextInputClient结束组合；不复制或丢弃尚未提交的marked文字。
    pub(in crate::native_surface) fn commit_composition_before_release(&self) -> Result<()> {
        if self.is_busy() {
            return Err(NativeMarkdownPagedTextViewError::Busy);
        }
        // discard可以同步回调客户；原view payload必须活到本次Native栈退出，关闭仍走既有Busy/延后归还。
        let _use = crate::payload_use::PayloadUseGuard::enter(&self.ivars().depth);
        let input = self
            .ivars()
            .input
            .borrow()
            .clone()
            .ok_or(NativeMarkdownPagedTextViewError::Closed)?;
        if input.state()?.marked.is_none() {
            return Ok(()); // 没有组合就没有提交事务；排版失败不应阻止原canonical保存与正常关闭。
        }
        let origin = self.origin()?;
        let context = self
            .ivars()
            .context
            .borrow()
            .clone()
            .ok_or(NativeMarkdownPagedTextViewError::ContextUnavailable)?;
        self.finish_composition_in_context(&input, &origin.delegate, &context, origin.epoch)
    }

    /// 保存与真正辞任后剩余的组合共用原客户收尾；这不是另一次输入请求或重新捕获目标。
    /// 只接受该 Input 原已显示文字，系统上下文结束期间仍准许原客户的真实协议回调。
    pub(super) fn finish_composition_in_context(
        &self,
        input: &std::rc::Rc<super::NativeMarkdownPagedInput>,
        delegate: &std::rc::Rc<dyn super::NativeMarkdownPagedTextViewDelegate>,
        context: &std::rc::Rc<super::InputContextOwner>,
        epoch: u64,
    ) -> Result<()> {
        if !self.same_input_context(input, delegate, context)
            || self.ivars().epoch != epoch
            || !delegate.current(epoch)
            || !self.same_input_context(input, delegate, context)
        {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // 保存接受客户已经显示的组合文字，不擅自选择候选词。先由原 Input 真正结束
        // Core 组合；discard 只结束系统转换会话，不能冒充一次 insertText 或提交收据。
        self.cancel_link_intent();
        let mut origin = self.origin()?;
        if !std::rc::Rc::ptr_eq(&origin.input, input)
            || !std::rc::Rc::ptr_eq(&origin.delegate, delegate)
        {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        origin.interpreted =
            self.ivars().interpreting_key.get() && self.ivars().input_callback_depth.get() == 1;
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = input.capture(epoch, &current)?;
        let unmarked = input.unmark(&permit, &current)?;
        if !self.same_input_context(input, delegate, context)
            || self.ivars().epoch != epoch
            || !delegate.current(epoch)
            || !self.same_input_context(input, delegate, context)
        {
            return Err(NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(
                NativeMarkdownPagedTextViewError::Revoked,
            )));
        }
        // 先清系统转换，再交原unmark收据的显示通知。否则invalidateCharacterCoordinates
        // 本身也可能回调仍持旧缓冲的IME，使它在本地marked已清后把旧文字追加到caret。
        // 全部 RefCell/Doc 借用已结束；这里只结束原会话，不用discard伪造Core提交结果。
        let discarded = self.input_context_step(|| context.0.discardMarkedText());
        let current = self.same_input_context(input, delegate, context)
            && self.ivars().epoch == epoch
            && delegate.current(epoch)
            && self.same_input_context(input, delegate, context);
        if !current {
            return discarded.and(Err(NativeMarkdownPagedTextViewError::CommittedEffect(
                Box::new(NativeMarkdownPagedTextViewError::Revoked),
            )));
        }
        let displayed = self.finish_edit_outcome(&origin, unmarked);
        discarded?;
        displayed?;
        if input.state()?.marked.is_some() {
            return Err(super::NativeMarkdownPagedInputError::CompositionActive.into());
        }
        Ok(())
    }

    /// 同步上下文回调只借原客户；合法提交可改变完整 Input，但不能改用后继挂载的身份。
    /// InputTarget 在构造后不可变，同一 Input Rc 固定原 Doc/草稿；格内容版本会随真实输入变化，
    /// 因此这里不能比较旧 TargetDomain 中的 content_serial，更不能拿旧 Scene 补签新文字。
    pub(super) fn same_input_context(
        &self,
        input: &std::rc::Rc<super::NativeMarkdownPagedInput>,
        delegate: &std::rc::Rc<dyn super::NativeMarkdownPagedTextViewDelegate>,
        context: &std::rc::Rc<super::InputContextOwner>,
    ) -> bool {
        !self.ivars().closed.get()
            && self
                .ivars()
                .input
                .borrow()
                .as_ref()
                .is_some_and(|actual| std::rc::Rc::ptr_eq(actual, input))
            && self
                .ivars()
                .delegate
                .borrow()
                .as_ref()
                .is_some_and(|actual| {
                    std::rc::Weak::ptr_eq(actual, &std::rc::Rc::downgrade(delegate))
                })
            && self
                .ivars()
                .context
                .borrow()
                .as_ref()
                .is_some_and(|actual| std::rc::Rc::ptr_eq(actual, context))
    }

    /// NSTextInputClient 的写回是 void；本次真实上下文调用必须取回其回调首错。
    /// 暂存并恢复此前视图错误，不给这里伪造 keyDown/菜单解释权限，也不让旧 Pending
    /// 冒充本次转换失败。Native 调用和回调期间没有持有错误槽或正文的 RefCell 借用。
    pub(super) fn input_context_step(&self, operation: impl FnOnce()) -> Result<()> {
        let previous = self.ivars().last_failure.borrow_mut().take();
        operation();
        let callback = self.ivars().last_failure.borrow_mut().take();
        if let Some(previous) = previous {
            self.failure(previous);
        }
        callback.map_or(Ok(()), Err)
    }

    /// 延后导航只能通知已提交的原收据；此处新origin仅供显示副作用，不重签导航before或位置。
    pub(in crate::native_surface) fn finish_navigation_commit(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
    ) -> Result<()> {
        let origin = self
            .origin()
            .map_err(|cause| NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause)))?;
        commit
            .check_current(&|epoch| origin.session_current(self, epoch))
            .map_err(|cause| {
                NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause.into()))
            })?;
        self.finish_commit_ref(&origin, commit)?;
        origin
            .delegate
            .reveal_committed_selection(commit)
            .map_err(|cause| NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause)))
    }

    pub(super) fn apply_native_input(
        &self,
        text: &AnyObject,
        replacement: NSRange,
        selected: Option<NSRange>,
    ) -> Result<()> {
        self.cancel_cell_pointer_intent(); // 直接 Native Client 插字/候选也撤旧终点，不借 keyDown 是否经过来判断取消。
        self.cancel_link_intent(); // 真实IME/插字到达已晚于原click，不能让异步链接越过这个输入。
        let mut origin = self.origin()?;
        origin.interpreted =
            self.ivars().interpreting_key.get() && self.ivars().input_callback_depth.get() == 1;
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        if selected.is_none()
            && replacement.location == objc2_foundation::NSNotFound as usize
            && origin.state.marked.is_none()
            && origin.delegate.defer_insert(text, &permit)?
        {
            return Ok(()); // 只表示按真实顺序接纳；不得立即在删除前的旧caret插字或报告已经提交。
        }
        // 显式替换始终按原范围执行；只有真实key解释栈的同步回调能交接后继事件。
        // marked本身从不排队，也不向系统谎报尚未发布的组合；外部直接调用仍会撤原任务。
        let commit = match selected {
            Some(selected) => {
                origin
                    .input
                    .set_marked_native(&permit, text, selected, replacement, &current)?
            }
            None => origin
                .input
                .insert_native(&permit, text, replacement, &current)?,
        };
        self.finish_edit_commit(&origin, commit)
    }

    pub(super) fn apply_unmark(&self) -> Result<()> {
        self.cancel_link_intent(); // 组合提交或客户交接同样结束原指针的外部激活权限。
        let mut origin = self.origin()?;
        origin.interpreted =
            self.ivars().interpreting_key.get() && self.ivars().input_callback_depth.get() == 1;
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        let outcome = origin.input.unmark(&permit, &current)?;
        self.finish_edit_outcome(&origin, outcome)
    }

    pub(super) fn finish_outcome(
        &self,
        origin: &Origin,
        outcome: NativeMarkdownPagedInputOutcome,
    ) -> Result<()> {
        match outcome {
            NativeMarkdownPagedInputOutcome::Committed(commit) => {
                self.finish_owned_commit(origin, commit, false)
            }
            NativeMarkdownPagedInputOutcome::Unchanged(state) => {
                if state != origin.state || !origin.current(self) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
                Ok(()) // 真无历史/无marked/相同选择不通知显示，也不制造一笔编辑。
            }
        }
    }

    /// 字符输入/组合/删除/历史动作请求实际活动端可见；普通程序选区仍只发布原收据。
    pub(super) fn finish_edit_outcome(
        &self,
        origin: &Origin,
        outcome: NativeMarkdownPagedInputOutcome,
    ) -> Result<()> {
        match outcome {
            NativeMarkdownPagedInputOutcome::Committed(commit) => {
                self.finish_edit_commit(origin, commit)
            }
            unchanged => self.finish_outcome(origin, unchanged), // 无变化不新建定位意图或显示帧。
        }
    }

    /// 同一真实编辑收据先完成Native通知，再让原Body/Cell唯一Producer定位postcommit caret。
    pub(super) fn finish_edit_commit(
        &self,
        origin: &Origin,
        commit: NativeMarkdownPagedInputCommit,
    ) -> Result<()> {
        self.finish_owned_commit(origin, commit, true)
    }

    /// 原同步回调先交实际Rc收据，再发通知；外部mouse/find origin没有解释权限。
    pub(super) fn finish_owned_commit(
        &self,
        origin: &Origin,
        commit: impl Into<std::rc::Rc<NativeMarkdownPagedInputCommit>>,
        reveal: bool,
    ) -> Result<()> {
        // 普通输入转交唯一 owned 收据；插表还需把同一 Rc 交给后继首格激活，不能重取选择补票。
        let commit = commit.into();
        // Core已消费并验证的空IME提交可能保留完整before；它没有文字、marked、折叠或选择后果。
        // 只在这个确实持有提交前Origin的入口识别相同完整状态，不能把事后捕获Origin的
        // Find/导航通知也当作空操作。真实解释中的同张收据仍须交回原队列，不能提前丢弃。
        let changed = commit.state != origin.state;
        let reveal = reveal && changed;
        let interpreted = origin.interpreted
            && origin
                .delegate
                .begin_interpreted_commit(origin.state, std::rc::Rc::clone(&commit))
                .map_err(|cause| {
                    NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause))
                })?;
        if changed {
            self.finish_commit_ref(origin, &commit)?;
        } else {
            // 没有显示后果便保留原Scene、CPU候选和用户滚动位置；尤其不能把离屏caret
            // 揭示成新的滚动意图。仍核原owner/epoch/完整后态，真实失权不能返回假成功。
            commit
                .check_current(&|epoch| origin.session_current(self, epoch))
                .map_err(|cause| {
                    NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause.into()))
                })?;
        }
        if interpreted {
            return origin
                .delegate
                .finish_interpreted_commit(&commit, reveal)
                .map_err(|cause| {
                    NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause))
                });
        }
        if reveal {
            origin
                .delegate
                .reveal_committed_selection(&commit)
                .map_err(|cause| {
                    NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause))
                })?;
        } // 普通直接输入继续沿原收据显示；没有屏障就没有新增默认事件owner。
        Ok(())
    }

    /// 队首原event只解释一次；false只表示解释前焦点已移交，原Body应撤该链而非认作回调失败。
    pub(in crate::native_surface) fn interpret_queued_key(
        &self,
        event: &objc2_app_kit::NSEvent,
    ) -> Result<bool> {
        let origin = self.origin()?;
        let window = origin
            .step(self, || self.window())?
            .ok_or(NativeMarkdownPagedTextViewError::Revoked)?;
        let responder = origin.step(self, || window.firstResponder())?;
        if !responder.is_some_and(|responder| {
            std::ptr::eq::<objc2_app_kit::NSResponder>(&*responder, self.as_super().as_super())
        }) {
            // 延后期间焦点可能已交给Find或其他窗口；原正文没有权限把后续key写到旧文稿。
            // 此处只撤原未解释事件，不重放到新客户，也不改写系统给出的replacementRange。
            return Ok(false);
        }
        let (_, context) = self.foreground_context()?;
        self.with_interpreted_input(|| {
            if !origin.session_current(self, origin.epoch) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            // 延后key仍只进入原客户context一次；真实IME回调沿原解释栈和Commit交接，
            // 不能改用legacy interpretKeyEvents直出字符，也不能在消费后按旧选择补签权限。
            let consumed = context.0.handleEvent(event);
            if !origin.session_current(self, origin.epoch) {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            if !consumed {
                // NO只授予原super传播，不授予另一轮文本解释；with_interpreted_input仍保原callback首错。
                // SAFETY: 原主线程活NSView和保留的同一NSEvent覆盖同步super keyDown:调用。
                unsafe {
                    let _: () = objc2::msg_send![super(self), keyDown: event];
                }
                if !origin.session_current(self, origin.epoch) {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
            }
            Ok(())
        })?;
        Ok(true)
    }

    /// 公开菜单延后后沿同一typed实现执行；不重新走公开排队入口产生循环。
    pub(in crate::native_surface) fn perform_queued_command(
        &self,
        selector: objc2::runtime::Sel,
    ) -> Result<()> {
        self.with_interpreted_input(|| {
            let _callback =
                crate::payload_use::PayloadUseGuard::enter(&self.ivars().input_callback_depth);
            self.command_from_input_context(selector)
        })
    }

    /// 队首格式借同一Core实现，并显式授权本次原Commit交回后继输入；公开UI不从depth推断归属。
    pub(in crate::native_surface) fn perform_queued_format(
        &self,
        action: omni_markdown_core::MarkdownFormatAction,
    ) -> Result<()> {
        self.with_interpreted_input(|| {
            let _callback =
                crate::payload_use::PayloadUseGuard::enter(&self.ivars().input_callback_depth);
            let _use = crate::payload_use::PayloadUseGuard::enter(&self.ivars().depth);
            self.cancel_link_intent();
            let mut origin = self.origin()?;
            origin.interpreted = true; // 仅此受控队首入口认证本event；不把共享depth授给外部toolbar重入。
            self.apply_format_at(&origin, action)
        })
    }

    /// 原插表语义在前步收据之后生成模板和首格caret，不排预计算字符串或旧anchor。
    pub(in crate::native_surface) fn perform_queued_table_insertion(&self) -> Result<()> {
        self.with_interpreted_input(|| {
            let _callback =
                crate::payload_use::PayloadUseGuard::enter(&self.ivars().input_callback_depth);
            let _use = crate::payload_use::PayloadUseGuard::enter(&self.ivars().depth);
            self.cancel_link_intent();
            let mut origin = self.origin()?;
            origin.interpreted = true; // 原Native槽只执行一次；公开插表按钮必须另排其到达位置。
            self.insert_default_table_at(&origin)
        })
    }

    /// 到达时已经复制的Paste只借原String，本回调不再读取clipboard或构造NSString。
    pub(in crate::native_surface) fn perform_queued_paste(&self, text: &str) -> Result<()> {
        self.with_interpreted_input(|| {
            let _callback =
                crate::payload_use::PayloadUseGuard::enter(&self.ivars().input_callback_depth);
            let mut origin = self.origin()?;
            origin.interpreted = self.ivars().input_callback_depth.get() == 1;
            let current = |epoch| epoch == origin.epoch && origin.current(self);
            let permit = origin.input.capture(origin.epoch, &current)?;
            let commit =
                origin
                    .input
                    .insert_text_isolated(&permit, text, super::not_found(), &current)?;
            self.finish_edit_commit(&origin, commit)
        })
    }

    /// 仅一次原key/菜单解释栈持此标记；同步回调失败保原typed首因，不能假报IME完成。
    pub(super) fn with_interpreted_input(
        &self,
        operation: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        if self.ivars().interpreting_key.replace(true) {
            return Err(NativeMarkdownPagedTextViewError::Busy);
        }
        let result = operation();
        self.ivars().interpreting_key.set(false);
        let callback = self.ivars().interpretation_failure.borrow_mut().take();
        if let Some(cause) = callback {
            return Err(cause);
        }
        result
    }

    /// Find延后定位仍保同一收据；显示通知只借原Commit，不移动后再重签。
    pub(super) fn finish_commit_ref(
        &self,
        origin: &Origin,
        commit: &NativeMarkdownPagedInputCommit,
    ) -> Result<()> {
        let result = (|| {
            let current = |epoch| origin.session_current(self, epoch);
            commit.check_current(&current)?;
            let cpu = self.ivars().cpu.borrow().clone();
            if let Some(cpu) = cpu {
                cpu.cancel_candidate();
            } // 已提交状态撤销旧候选；借用归还后才drop真实Native context。
            commit.check_current(&current)?; // 释放回调可重入；失权仍是CommittedEffect，不能重放Core事务。
            origin.delegate.committed(commit)?;
            commit.check_current(&current)?;
            // 唯一已提交receipt授权通知；不使用旧before，也不重捕后继g/selection补签。
            let geometry = self.ivars().geometry_epoch.get();
            let visible = self.visibleRect();
            commit.check_current(&current)?;
            if geometry != self.ivars().geometry_epoch.get() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            let bounds = self.bounds();
            commit.check_current(&current)?;
            if geometry != self.ivars().geometry_epoch.get() {
                return Err(NativeMarkdownPagedTextViewError::Revoked);
            }
            if let Some(visible) = bounded_viewport(visible, bounds) {
                self.setNeedsDisplayInRect(visible);
                commit.check_current(&current)?;
                if geometry != self.ivars().geometry_epoch.get() {
                    return Err(NativeMarkdownPagedTextViewError::Revoked);
                }
            } // 尚未挂窗没有实际正面积视口；保留已接受canonical输入，不伪造绘制Ready。
            // 正文已提交不等于新字形坐标已发布；此时通知 IME 重查可能把有效缓存刷新成 Pending。
            // Body 的 committed 已撤旧 Scene 并安排唯一排版；只有 scene_ready 对同一实际
            // Input/几何和 context 发坐标失效通知，被后继输入撤销的中间状态不会伪报完成。
            // 保存、结束组合和关闭仍各自处理原 context，不依赖这项显示完成通知归还资源。
            Ok(())
        })();
        // 到这里Core已经发布；错误只能停止显示后果，不能让调用方误认正文仍是旧值。
        result.map_err(|cause| NativeMarkdownPagedTextViewError::CommittedEffect(Box::new(cause)))
    }

    pub(super) fn substring(
        &self,
        range: NSRange,
    ) -> Result<Option<(Retained<NSAttributedString>, NSRange)>> {
        let origin = self.origin()?;
        if range.location == objc2_foundation::NSNotFound as usize
            || range.location > origin.state.utf16_len
        {
            return Ok(None);
        }
        let requested_end = range
            .location
            .checked_add(range.length)
            .ok_or(NativeMarkdownPagedTextViewError::InvalidRange)?;
        let mut start = range.location;
        let mut end = requested_end
            .min(origin.state.utf16_len)
            .min(start.saturating_add(SUBSTRING_UNITS));
        let mut units = [0u16; SUBSTRING_UNITS];
        let current = |epoch| epoch == origin.epoch && origin.current(self);
        let permit = origin.input.capture(origin.epoch, &current)?;
        let mut unit = [0u16; 1];
        // 同原许可有限读实际UTF16；Cell不新增Snapshot，Body仍复用原Doc窗口。
        if start < origin.state.utf16_len {
            origin
                .input
                .copy_utf16_code_units(&permit, start, &mut unit, &current)?;
            if (0xdc00..=0xdfff).contains(&unit[0]) {
                start = start
                    .checked_sub(1)
                    .ok_or(NativeMarkdownPagedTextViewError::InvalidRange)?;
            }
        }
        end = end.min(start.saturating_add(SUBSTRING_UNITS));
        if end > start && end < origin.state.utf16_len {
            origin
                .input
                .copy_utf16_code_units(&permit, end, &mut unit, &current)?;
            if (0xdc00..=0xdfff).contains(&unit[0]) {
                end -= 1;
            }
        }
        origin
            .input
            .copy_utf16_code_units(&permit, start, &mut units[..end - start], &current)?;
        // 所有Doc/胶囊借用已归还；Foundation初始化只能持固定数组与同一原目标。
        if !origin.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        // SAFETY: 固定数组覆盖完整有限length；init同步复制UTF16，指针在整个调用内有效。
        let string = origin.step(self, || unsafe {
            NSString::initWithCharacters_length(
                NSString::alloc(),
                NonNull::from(&mut units[0]),
                end - start,
            )
        })?;
        let value = origin.step(self, || {
            NSAttributedString::initWithString(NSAttributedString::alloc(), &string)
        })?;
        Ok(Some((value, NSRange::new(start, end - start))))
    }

    pub(super) fn range_in_source(&self, scene: &SceneUse, range: NSRange) -> Result<()> {
        let end = range
            .location
            .checked_add(range.length)
            .ok_or(NativeMarkdownPagedTextViewError::InvalidRange)?;
        if end > scene.origin.state.utf16_len {
            return Err(NativeMarkdownPagedTextViewError::InvalidRange);
        }
        let current = |epoch| epoch == scene.origin.epoch && scene.current(self);
        let permit = scene.origin.input.capture(scene.origin.epoch, &current)?;
        scene
            .origin
            .input
            .validate_utf16_range(&permit, range.location..end, &current)?;
        if !scene.current(self) {
            return Err(NativeMarkdownPagedTextViewError::Revoked);
        }
        Ok(())
    }

    /// 只有实际Ready scene提供的首行/插入点才转换screen坐标；Pending不制造候选窗锚点。
    pub fn screen_rect(&self, range: NSRange) -> Result<NativeMarkdownPagedCharacterRect> {
        let _use = crate::payload_use::PayloadUseGuard::enter(&self.ivars().depth);
        let scene = self.scene()?;
        self.range_in_source(&scene, range)?;
        let mut value = scene.scene.first_rect(range)?;
        if !scene.current(self) || !finite_rect(value.rect, false) || value.rect.size.height <= 0.0
        {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        self.range_in_source(&scene, value.actual_range)?;
        let window = scene
            .step(self, || self.window())?
            .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
        #[cfg(feature = "native-markdown-runtime-probe")]
        let raw = value.rect;
        let base = scene.step(self, || self.convertRect_toView(value.rect, None))?;
        value.rect = scene.step(self, || window.convertRectToScreen(base))?;
        // 零宽换行仍有真实行高；只在显式验收构建记录两个原生转换的几何，
        // 区分空矩形转换与原Scene范围/排版故障，不读取或打印任何正文。
        #[cfg(feature = "native-markdown-runtime-probe")]
        if raw.size.width == 0.0 {
            eprintln!(
                "native_zero_width_screen_rect raw={raw:?} base={base:?} screen={:?} actual_range={:?}",
                value.rect, value.actual_range,
            );
        }
        if !finite_rect(value.rect, false) {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        Ok(value)
    }

    /// NSTextInputClient点为screen坐标，不能把window或bitmap坐标直接冒充正文命中。
    pub fn screen_hit(&self, point: NSPoint) -> Result<usize> {
        let _use = crate::payload_use::PayloadUseGuard::enter(&self.ivars().depth);
        if !point.x.is_finite() || !point.y.is_finite() {
            return Err(NativeMarkdownPagedTextViewError::InvalidGeometry);
        }
        let scene = self.scene()?;
        let window = scene
            .step(self, || self.window())?
            .ok_or(NativeMarkdownPagedTextViewError::Pending)?;
        let base = scene.step(self, || window.convertPointFromScreen(point))?;
        let local = scene.step(self, || self.convertPoint_fromView(base, None))?;
        let index = scene.scene.hit(local)?;
        self.range_in_source(&scene, NSRange::new(index, 0))?;
        Ok(index)
    }
}

#[cfg(feature = "native-input-trace")]
impl NativeMarkdownPagedTextView {
    /// 显式arm只借当前View所属的原Input；不触发焦点、系统按键或输入源变更。
    pub(in crate::native_surface) fn arm_input_trace(
        &self,
        ticks: u64,
        numer: u32,
        denom: u32,
    ) -> std::result::Result<(), crate::NativeInputTraceAccessError> {
        if self.ivars().closed.get() {
            return Err(crate::NativeInputTraceAccessError::NoEditor);
        }
        if self.is_busy() {
            return Err(crate::NativeInputTraceAccessError::Busy);
        }
        let input = self
            .ivars()
            .input
            .borrow()
            .clone()
            .ok_or(crate::NativeInputTraceAccessError::NoEditor)?;
        input.arm_input_trace(ticks, numer, denom)
    }

    /// View没有独立记录器；复制原Input句柄后结束槽借用，scope只持原有限标量payload。
    pub(in crate::native_surface) fn input_trace(
        &self,
    ) -> Option<crate::native_input_trace::SharedNativeInputTrace> {
        let input = self.ivars().input.borrow().clone()?;
        input.input_trace()
    }

    /// 原显式drain不派生新的Input或Native图；活动selector由原TraceScope返回ActiveScope。
    pub(in crate::native_surface) fn drain_input_trace(
        &self,
    ) -> std::result::Result<crate::NativeInputTraceSnapshot, crate::NativeInputTraceAccessError>
    {
        let input = self
            .ivars()
            .input
            .borrow()
            .clone()
            .ok_or(crate::NativeInputTraceAccessError::NoEditor)?;
        input.drain_input_trace()
    }

    /// 同步范围只观察本客户公开marked语义的原事实，不调用NSTextInputContext或读取正文。
    fn input_trace_observation(&self) -> (Option<crate::NativeInputTraceDocument>, bool) {
        let input = self.ivars().input.borrow().clone();
        input.map_or((None, false), |input| input.input_trace_observation())
    }

    /// 只在真正keyDown栈内建立事件来源；后继排队解释不会把旧event猜成仍在当前栈中。
    pub(super) fn begin_input_trace_event(
        &self,
        event: &objc2_app_kit::NSEvent,
    ) -> Option<crate::native_input_trace::TraceScope> {
        let trace = self.input_trace()?;
        let ticks = crate::native_surface::input_trace::native_input_trace_now();
        let timestamp_bits = event.timestamp().to_bits(); // Native getter时没有Doc或RefCell借用存活。
        let (document, marked) = self.input_trace_observation();
        crate::native_input_trace::TraceScope::event(
            &trace,
            ticks,
            timestamp_bits,
            document,
            marked,
        )
    }

    /// 系统IME异步调用同一协议入口时照实记录selector，缺少当前event则保持None。
    pub(super) fn begin_input_trace_selector(
        &self,
        selector: crate::NativeInputTraceSelector,
    ) -> Option<crate::native_input_trace::TraceScope> {
        let trace = self.input_trace()?;
        let (document, marked) = self.input_trace_observation();
        crate::native_input_trace::TraceScope::selector(
            &trace,
            selector,
            crate::native_surface::input_trace::native_input_trace_now(),
            document,
            marked,
        )
    }

    /// 原selector与其后果实际返回后关闭同一scope；不输出、不注册observer或安排后继回调。
    pub(super) fn finish_input_trace_scope(
        &self,
        scope: Option<crate::native_input_trace::TraceScope>,
    ) {
        if let Some(scope) = scope {
            if self.input_trace().is_none() {
                return; // 原Input.close已撤payload；Drop标记aborted，不能拿marked消失伪报组合已确认。
            }
            let (document, marked) = self.input_trace_observation();
            scope.finish(
                crate::native_surface::input_trace::native_input_trace_now(),
                document,
                marked,
            );
        }
    }

    /// admission失败只转成既有有限类别；不把typed错误或敏感业务字符串写入记录。
    pub(super) fn reject_input_trace(&self, cause: &NativeMarkdownPagedTextViewError) {
        let Some(input) = self.ivars().input.borrow().clone() else {
            return;
        };
        let rejection = match cause {
            NativeMarkdownPagedTextViewError::Closed
            | NativeMarkdownPagedTextViewError::Input(
                super::NativeMarkdownPagedInputError::Closed,
            ) => crate::NativeInputTraceRejection::Closing,
            NativeMarkdownPagedTextViewError::Input(
                super::NativeMarkdownPagedInputError::DocumentReleased,
            ) => crate::NativeInputTraceRejection::DocumentUnavailable,
            _ => crate::NativeInputTraceRejection::CanonicalError,
        };
        input.reject_input_trace(rejection);
    }
}
