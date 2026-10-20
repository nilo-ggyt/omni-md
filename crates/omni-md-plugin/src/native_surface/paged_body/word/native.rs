//! 原keyDown先等真实导航收据，再由唯一NSTextInputContext同步解释；不保存虚构marked状态。

use super::*;
use crate::native_surface::paged_input::NativeMarkdownPagedInputState;
use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2_app_kit::NSEvent;

pub(in crate::native_surface::paged_body) enum NativeInput {
    Key {
        event: Retained<NSEvent>,
        bytes: usize,
    },
    Command(Sel),
    Paste(String),
    Attachments {
        paths: Vec<std::path::PathBuf>,
        /// 只保原可见命中的canonical代次/位置；正文变化后绝不把旧drop改插到新文稿。
        drop: Option<(u64, usize)>,
    },
    Format(omni_markdown_core::MarkdownFormatAction),
    InsertTable,
    Fold(super::super::super::paged_text_view::NativeMarkdownPagedSourceFoldAction),
}
impl NativeInput {
    /// 事件数量另受原32段限制；字符payload与普通新增文本共用原输入字节额度。
    pub(in crate::native_surface::paged_body) fn bytes(&self) -> usize {
        match self {
            Self::Key { bytes, .. } => *bytes,
            Self::Command(_) | Self::Format(_) | Self::InsertTable | Self::Fold(_) => 0,
            Self::Paste(text) => text.len(),
            Self::Attachments { paths, .. } => {
                paths.iter().map(|path| path.as_os_str().len()).sum()
            }
        }
    }
}

impl NativeMarkdownPagedBody {
    /// 格式/插表只保原领域动作；实际选择必须等队首执行时从前步真实Commit取得。
    pub(in crate::native_surface::paged_body) fn accept_native_edit(
        &self,
        action: NativeInput,
        permit: &NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        let files = matches!(&action, NativeInput::Attachments { .. });
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?;
        }
        if let NativeInput::Attachments { paths, .. } = &action {
            crate::native_surface::workspace_actions::admit_attachment_paths(0, paths)
                .map_err(ViewError::AttachmentImport)?; // 无效外部批次在建立任何输入屏障前整体拒绝。
        }
        let Some(pending) = self.native_input_chain(files)? else {
            return Ok(false);
        };
        // 正在解释前步时到达的外部菜单仍排在原队尾；本队首自有动作由View明确跳过defer。
        let slot = Rc::new(RefCell::new(None));
        {
            let mut steps = pending.steps.borrow_mut();
            if steps.len() >= 32 {
                return Err(ViewError::Busy.into());
            }
            if let NativeInput::Attachments { paths, .. } = &action {
                // 与原文件入口相同：最多四个待处理批次，不截断用户已经接纳的路径前缀。
                let queued = steps.iter().filter(|step| matches!(step,
                    WordInput::Native(slot) if matches!(&*slot.borrow(), Some(NativeInput::Attachments { .. }))
                )).count();
                crate::native_surface::workspace_actions::admit_attachment_paths(queued, paths)
                    .map_err(ViewError::AttachmentImport)?;
            }
            let bytes = action.bytes();
            if bytes
                > crate::native_surface::text_storage::NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES
                    .saturating_sub(pending.queued_bytes.get())
            {
                return Err(ViewError::Busy.into());
            }
            pending.queued_bytes.set(pending.queued_bytes.get() + bytes);
            steps.push_back(WordInput::Native(slot.clone()));
        }
        let serial = self.serial.get();
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        let checked = self
            .input
            .check_permit(permit, &current)
            .map_err(ViewError::from);
        if let Err(cause) = checked {
            pending
                .queued_bytes
                .set(pending.queued_bytes.get() - action.bytes());
            pending.steps.borrow_mut().retain(
                |step| !matches!(step, WordInput::Native(actual) if Rc::ptr_eq(actual, &slot)),
            );
            if files
                && pending.steps.borrow().is_empty()
                && pending.waiting_vertical.borrow().is_none()
            {
                self.cancel_original_word_delete(&pending)?; // 只归还本请求留下的原空链；不让失败批次永久占有输入屏障。
            } else if pending.current_at(self, serial) {
                self.schedule()?; // getter重入可能已消费原唤醒；只恢复原链，不重放被拒绝动作。
            }
            return Err(cause.into());
        }
        *slot.borrow_mut() = Some(action); // 原检查的Native getter已经返回，未完成的占槽不再阻挡前步。
        self.schedule()?;
        Ok(true)
    }

    /// 普通键盘只接现存Word/垂直屏障；文件I/O需让出Native回调，仍由同一原链延后执行。
    fn native_input_chain(&self, for_files: bool) -> Result<Option<Rc<PendingWordDelete>>> {
        if let Some(pending) = self.pending_word_delete.borrow().clone() {
            return Ok(Some(pending));
        }
        let vertical = self.pending_vertical.borrow().clone();
        if vertical.is_none() && !for_files {
            return Ok(None);
        }
        if vertical
            .as_ref()
            .is_some_and(|vertical| !vertical.current_native(self))
        {
            return Err(ViewError::Revoked.into());
        }
        let request = self.view()?.capture_scene_request()?;
        if vertical.as_ref().is_some_and(|vertical| {
            !vertical.current_native(self) || !vertical.matches(self, &request)
        }) {
            return Err(ViewError::Revoked.into());
        }
        let current = |epoch| {
            self.current(epoch)
                && vertical
                    .as_ref()
                    .is_none_or(|vertical| vertical.current_native(self))
        };
        let permit = self
            .input
            .capture(self.configuration.epoch, &current)
            .map_err(ViewError::from)?;
        let pending = Rc::new(PendingWordDelete {
            request: RefCell::new(request),
            steps: RefCell::new(VecDeque::new()),
            queued_bytes: Cell::new(0),
            receipt: RefCell::new(None),
            retired_serial: Cell::new(None),
            permit: RefCell::new(Some(permit)),
            bound: RefCell::new(None),
            ready: RefCell::new(None),
            fold_expansion: RefCell::new(None),
            fold_failure: RefCell::new(None),
            move_prepared: Cell::new(false),
            visual_word: Cell::new(false),
            visual_seed: Cell::new(None),
            visual_ready: Cell::new(None),
            interpreting_native: Cell::new(false),
            native_insert_at: Cell::new(0),
            attachment_failure: RefCell::new(None),
            waiting_vertical: RefCell::new(vertical),
        });
        *self.pending_word_delete.borrow_mut() = Some(Rc::clone(&pending));
        Ok(Some(pending))
    }

    /// getter之前先占原到达位置；Native重入只能排其后，转换失败只撤自己的空槽。
    pub(in crate::native_surface::paged_body) fn accept_native_key(
        &self,
        event: &NSEvent,
        permit: &NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        let Some(pending) = self.native_input_chain(false)? else {
            return Ok(false);
        };
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?; // 额度或后继事件不能盖掉原Source已经发生的真实首错。
        }
        let slot = Rc::new(RefCell::new(None));
        {
            let mut steps = pending.steps.borrow_mut();
            if steps.len() >= 32 {
                return Err(ViewError::Busy.into());
            }
            steps.push_back(WordInput::Native(Rc::clone(&slot)));
        }
        let serial = self.serial.get();
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        let charged = Cell::new(0);
        let copied: Result<NativeInput> = (|| {
            self.input
                .check_permit(permit, &current)
                .map_err(ViewError::from)?;
            let characters = event.characters();
            self.input
                .check_permit(permit, &current)
                .map_err(ViewError::from)?;
            let plain = event.charactersIgnoringModifiers();
            self.input
                .check_permit(permit, &current)
                .map_err(ViewError::from)?;
            let character_units = characters.as_ref().map_or(0, |text| text.length());
            self.input
                .check_permit(permit, &current)
                .map_err(ViewError::from)?;
            let plain_units = plain.as_ref().map_or(0, |text| text.length());
            self.input
                .check_permit(permit, &current)
                .map_err(ViewError::from)?;
            let units = character_units
                .checked_add(plain_units)
                .ok_or(ViewError::CounterExhausted)?;
            let bytes = units.checked_mul(2).ok_or(ViewError::CounterExhausted)?;
            let maximum =
                crate::native_surface::text_storage::NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES;
            if bytes > maximum.saturating_sub(pending.queued_bytes.get()) {
                return Err(ViewError::Busy.into());
            }
            pending.queued_bytes.set(pending.queued_bytes.get() + bytes);
            charged.set(bytes); // retain和临时NSString释放均可重入，后继必须先看到本event已占用的额度。
            // SAFETY: 原主线程同步参数仍活，retain仅保同一实际event，绝不重造时间戳/键盘载荷。
            let retained = unsafe { Retained::retain(std::ptr::from_ref(event).cast_mut()) }
                .ok_or(ViewError::Revoked)?;
            drop(characters);
            drop(plain); // 临时NSString归还可重入；最终原票检查后才安装event和额度。
            self.input
                .check_permit(permit, &current)
                .map_err(ViewError::from)?;
            Ok(NativeInput::Key {
                event: retained,
                bytes,
            })
        })();
        match copied {
            Ok(input) => {
                *slot.borrow_mut() = Some(input);
                self.schedule()?;
                Ok(true)
            }
            Err(cause) => {
                pending
                    .queued_bytes
                    .set(pending.queued_bytes.get() - charged.get());
                pending.steps.borrow_mut().retain(
                    |step| !matches!(step, WordInput::Native(actual) if Rc::ptr_eq(actual, &slot)),
                );
                if pending.current_at(self, serial) {
                    self.schedule()?; // 载荷转换已结束，原None槽造成的暂停必须由同一链恢复。
                }
                Err(cause)
            }
        }
    }

    /// 菜单先占原到达位置；Paste在本回调用累计额度转换，不未来重读clipboard或越额复制。
    pub(in crate::native_surface::paged_body) fn accept_native_command(
        &self,
        selector: Sel,
        permit: &NativeMarkdownPagedInputPermit,
    ) -> Result<bool> {
        let Some(pending) = self.native_input_chain(false)? else {
            return Ok(false);
        };
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?; // 额度或后继事件不能盖掉原Source已经发生的真实首错。
        }
        let slot = Rc::new(RefCell::new(None));
        {
            let mut steps = pending.steps.borrow_mut();
            if steps.len() >= 32 {
                return Err(ViewError::Busy.into());
            }
            steps.push_back(WordInput::Native(Rc::clone(&slot)));
        }
        let serial = self.serial.get();
        let current = |epoch| self.current(epoch) && pending.current_native_at(self, serial);
        let charged = Cell::new(0);
        let captured: Result<NativeInput> = (|| {
            self.input
                .check_permit(permit, &current)
                .map_err(ViewError::from)?;
            if selector != objc2::sel!(paste:) && selector != objc2::sel!(pasteAsPlainText:) {
                return Ok(NativeInput::Command(selector));
            }
            let board = objc2_app_kit::NSPasteboard::generalPasteboard();
            self.input
                .check_permit(permit, &current)
                .map_err(ViewError::from)?;
            if selector == objc2::sel!(paste:) {
                // 文件先于文本表示；路径在原菜单到达时捕获，队首不再读取后来变化的clipboard。
                let paths = crate::native_surface::workspace_actions::native_markdown_pasteboard_file_paths(&board)
                    .map_err(ViewError::AttachmentImport)?;
                self.input
                    .check_permit(permit, &current)
                    .map_err(ViewError::from)?;
                if !paths.is_empty() {
                    let queued = pending.steps.borrow().iter().filter(|step| matches!(step,
                        WordInput::Native(slot) if matches!(&*slot.borrow(), Some(NativeInput::Attachments { .. }))
                    )).count();
                    crate::native_surface::workspace_actions::admit_attachment_paths(
                        queued, &paths,
                    )
                    .map_err(ViewError::AttachmentImport)?;
                    let input = NativeInput::Attachments { paths, drop: None };
                    let bytes = input.bytes();
                    if bytes > crate::native_surface::text_storage::NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES.saturating_sub(pending.queued_bytes.get()) {
                        return Err(ViewError::Busy.into());
                    }
                    pending.queued_bytes.set(pending.queued_bytes.get() + bytes);
                    charged.set(bytes);
                    return Ok(input);
                }
            }
            // SAFETY: 原公开字符串类型常量只用于这次真实菜单到达的clipboard读取。
            let text = board.stringForType(unsafe { objc2_app_kit::NSPasteboardTypeString });
            self.input
                .check_permit(permit, &current)
                .map_err(ViewError::from)?;
            let text = text.ok_or(ViewError::ClipboardUnavailable)?;
            let copied = self.input.copy_permit_text_with_limit(permit, &text, &current, &|bytes| {
                let remaining = crate::native_surface::text_storage::NATIVE_MARKDOWN_INPUT_TRANSACTION_MAX_BYTES.saturating_sub(pending.queued_bytes.get());
                if bytes > remaining { return false; }
                pending.queued_bytes.set(pending.queued_bytes.get() + bytes);
                charged.set(bytes);
                true
            });
            drop(text);
            drop(board);
            let copied = copied.map_err(ViewError::from)?;
            self.input
                .check_permit(permit, &current)
                .map_err(ViewError::from)?;
            Ok(NativeInput::Paste(copied))
        })();
        match captured {
            Ok(input) => {
                *slot.borrow_mut() = Some(input);
                self.schedule()?;
                Ok(true)
            }
            Err(cause) => {
                pending
                    .queued_bytes
                    .set(pending.queued_bytes.get() - charged.get());
                pending.steps.borrow_mut().retain(
                    |step| !matches!(step, WordInput::Native(actual) if Rc::ptr_eq(actual, &slot)),
                );
                if pending.current_at(self, serial) {
                    self.schedule()?; // 载荷转换已结束，原None槽造成的暂停必须由同一链恢复。
                }
                Err(cause)
            }
        }
    }

    /// 只有实际解释栈内的同步输入回调能交接这张Commit；外部点击/Find没有此入口。
    pub(in crate::native_surface::paged_body) fn begin_native_commit(
        &self,
        before: NativeMarkdownPagedInputState,
        commit: Rc<NativeMarkdownPagedInputCommit>,
    ) -> Result<bool> {
        let Some(pending) = self.pending_word_delete.borrow().clone() else {
            return Ok(false);
        };
        if !pending.interpreting_native.get() {
            return Ok(false);
        }
        let serial = self.serial.get();
        let mut expected = pending.request();
        if expected.input != before || !pending.current_at_before_commit(self, serial, before) {
            return Err(ViewError::Revoked.into());
        }
        commit
            .check_current(&|epoch| self.current(epoch))
            .map_err(ViewError::from)?;
        expected.input = commit.state;
        let actual = self.view()?.capture_scene_request()?;
        commit
            .check_current(&|epoch| self.current(epoch))
            .map_err(ViewError::from)?;
        if !actual.same(&expected) {
            return Err(ViewError::Revoked.into());
        }
        *pending.receipt.borrow_mut() = Some(commit);
        Ok(true)
    }

    /// 同一Commit授权自有gutter变化与下一permit；不从后继Input补签旧event或外部resize。
    pub(in crate::native_surface::paged_body) fn finish_native_commit(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
        reveal: bool,
    ) -> Result<()> {
        let pending = self
            .pending_word_delete
            .borrow()
            .clone()
            .ok_or(ViewError::Revoked)?;
        if !pending.interpreting_native.get() || !self.owns_word_commit(commit) {
            return Err(ViewError::Revoked.into());
        }
        let before = pending.request();
        let request = self.synchronize_committed_gutter(commit, &before)?;
        if !pending.current_at(self, self.serial.get()) {
            return Err(ViewError::Revoked.into());
        }
        *pending.request.borrow_mut() = request;
        if reveal {
            self.accept_selection_reveal(commit)?;
        }
        self.arm_word_successor(&pending, Some(commit))
    }

    /// 每回合只解释一次原event；marked的所有状态在同步回调内由原Input发布。
    pub(in crate::native_surface::paged_body) fn advance_native_input(
        &self,
        pending: &Rc<PendingWordDelete>,
    ) -> Result<bool> {
        let slot = {
            let mut steps = pending.steps.borrow_mut();
            if !matches!(steps.front(), Some(WordInput::Native(_))) {
                return Ok(false);
            }
            match steps.pop_front() {
                Some(WordInput::Native(slot)) => slot,
                _ => unreachable!("原同次借用已核Native队首"),
            }
        };
        let input = slot.borrow_mut().take().ok_or(ViewError::Busy)?;
        let bytes = input.bytes();
        let serial = self.serial.get();
        let result: Result<bool> = (|| {
            if !pending.current_native_at(self, serial) {
                return Err(ViewError::Revoked.into());
            }
            if let Some(producer) = self.producer.borrow_mut().as_mut() {
                producer.target_source_health()?;
                producer.cancel_word_delete();
                producer.cancel_word_group();
                producer.cancel_visual();
            } // 上一查询已经完成，Native回调不得继续领取其旧bound/Ready。
            pending.bound.borrow_mut().take();
            pending.ready.borrow_mut().take();
            pending.visual_seed.set(None);
            pending.visual_ready.set(None);
            pending.native_insert_at.set(0);
            let view = self.view()?;
            pending.interpreting_native.set(true);
            let result = match &input {
                NativeInput::Key { event, .. } => view.interpret_queued_key(event),
                NativeInput::Command(selector) => {
                    view.perform_queued_command(*selector).map(|()| true)
                }
                NativeInput::Paste(text) => view.perform_queued_paste(text).map(|()| true),
                NativeInput::Attachments { paths, drop } => {
                    match view.perform_queued_attachments(paths, *drop) {
                        Err(cause @ ViewError::AttachmentImport(_)) => {
                            // 外部文件拒绝只结束此动作；原canonical和Body仍可编辑，后继已接纳输入照常接续。
                            let mut failure = pending.attachment_failure.borrow_mut();
                            if failure.is_none() {
                                *failure = Some(cause);
                            } // 原链只保一次业务首错，实际Source/资源错误仍沿Err归Body。
                            Ok(true)
                        }
                        other => other.map(|()| true), // Core首错或已提交显示后果必须原样失败，不能伪装成普通导入拒绝。
                    }
                }
                NativeInput::Format(action) => view.perform_queued_format(*action).map(|()| true),
                NativeInput::InsertTable => view.perform_queued_table_insertion().map(|()| true),
                NativeInput::Fold(action) => {
                    match view.perform_queued_source_fold(action.clone()) {
                        Err(cause @ ViewError::FoldAction(_)) => {
                            let mut failure = pending.fold_failure.borrow_mut();
                            if failure.is_none() {
                                *failure = Some(cause);
                            }
                            Ok(true) // 普通业务拒绝只消费本菜单，后续已接纳文字仍沿同一链结算。
                        }
                        other => other.map(|()| true),
                    }
                }
            };
            result.map_err(Into::into)
        })();
        pending.interpreting_native.set(false);
        pending.native_insert_at.set(0);
        drop(input); // 真实event/新增payload退出后才归还额度；取消的局部Rc不能拖住Native对象。
        pending.queued_bytes.set(pending.queued_bytes.get() - bytes);
        if matches!(result, Ok(false)) {
            self.cancel_original_word_delete(pending)?; // 焦点已实际移交，正文只归还旧事件，不假报已解释。
            self.schedule()?;
            return Ok(true);
        }
        let result = result.and_then(|_| self.arm_word_successor(pending, None));
        self.word_input_result(pending, serial, serial, result)?;
        Ok(true)
    }

    /// 未解释的原event等真实caret Scene；其自有viewport操作只交接这次API的真实geometry。
    pub(in crate::native_surface::paged_body) fn native_preparation_geometry<T>(
        &self,
        before: &NativeMarkdownPagedSceneRequest,
        viewport: &Rc<crate::native_surface::paged_viewport::NativeMarkdownPagedViewport>,
        operation: impl FnOnce() -> std::result::Result<
            T,
            crate::native_surface::paged_viewport::NativeMarkdownPagedViewportError,
        >,
    ) -> std::result::Result<
        T,
        crate::native_surface::paged_viewport::NativeMarkdownPagedViewportError,
    > {
        use crate::native_surface::paged_viewport::NativeMarkdownPagedViewportError as GeometryError;
        let pending = self.pending_word_delete.borrow().clone();
        let Some(pending) = pending.filter(|pending| {
            pending.native_front()
                && !pending.interpreting_native.get()
                && pending.waiting_vertical.borrow().is_none()
        }) else {
            return operation(); // 普通绘制与原垂直续体没有额外输入图或Native getter。
        };
        let serial = self.serial.get();
        if !pending.current_native_at(self, serial) || !pending.request().same(before) {
            return Err(GeometryError::Revoked);
        }
        let view = self.view().map_err(|_| GeometryError::Closed)?;
        let scroll = view.enclosingScrollView();
        if !pending.current_native_at(self, serial) {
            return Err(GeometryError::Revoked);
        }
        let scroll = scroll.ok_or(GeometryError::Closed)?;
        let frame = scroll.frame();
        if !pending.current_native_at(self, serial) {
            return Err(GeometryError::Revoked);
        }
        let bounds = scroll.bounds();
        if !pending.current_native_at(self, serial) {
            return Err(GeometryError::Revoked);
        }
        let value = operation()?; // 原API已核实际目标Y、Native挂载、有限frame及同Input；失败不能补签。
        let geometry = viewport.geometry()?;
        if !pending.current_at(self, serial) || !geometry.is_current() {
            return Err(GeometryError::Revoked);
        }
        let after_frame = scroll.frame();
        if !pending.current_at(self, serial) || !geometry.is_current() {
            return Err(GeometryError::Revoked);
        }
        let after_bounds = scroll.bounds();
        if !pending.current_at(self, serial)
            || !geometry.is_current()
            || after_frame != frame
            || after_bounds != bounds
        {
            return Err(GeometryError::Revoked); // 外层resize或移动不是这次自有内容高度/reveal的授权。
        }
        let actual = view.capture_scene_request()?;
        if !pending.current_at(self, serial)
            || !geometry.is_current()
            || actual.input != before.input
            || !actual.target.same(&before.target)
            || !Weak::ptr_eq(&actual.document, &before.document)
            || actual.frame_in_parent != geometry.body_frame
            || actual.virtual_viewport.document_origin != geometry.body_frame.origin
            || actual.virtual_viewport.scroll_epoch != geometry.scroll_epoch
            || actual.virtual_viewport.layout_width != before.virtual_viewport.layout_width
            || actual.virtual_viewport.layout_origin_x != before.virtual_viewport.layout_origin_x
        {
            return Err(GeometryError::Revoked);
        }
        *pending.request.borrow_mut() = actual; // 只有这个原API成功且原nonce仍当前，才交回同票剩余event。
        Ok(value)
    }

    /// 当前原event产生的垂直命令接在它后面；其真实最终收据之前不能解释下一个key。
    pub(in crate::native_surface::paged_body) fn attach_native_vertical(
        &self,
        vertical: &Rc<super::super::navigation::PendingVertical>,
    ) -> Result<()> {
        let pending = self.pending_word_delete.borrow().clone();
        let Some(pending) = pending.filter(|pending| pending.interpreting_native.get()) else {
            return Ok(());
        };
        if !pending.current_native_at(self, self.serial.get()) || !vertical.current_native(self) {
            return Err(ViewError::Revoked.into());
        }
        *pending.waiting_vertical.borrow_mut() = Some(Rc::clone(vertical));
        Ok(())
    }

    /// 原垂直续体的最终提交才释放事件屏障；同before/同pending不能由外部选择冒充。
    pub(in crate::native_surface::paged_body) fn bind_vertical_native_receipt(
        &self,
        vertical: &Rc<super::super::navigation::PendingVertical>,
        before: &NativeMarkdownPagedSceneRequest,
        commit: Option<Rc<NativeMarkdownPagedInputCommit>>,
    ) -> Result<bool> {
        let Some(pending) = self.pending_word_delete.borrow().clone() else {
            return Ok(false);
        };
        let waiting = pending.waiting_vertical.borrow().clone();
        if !waiting
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, vertical))
        {
            return Ok(false);
        }
        let serial = self.serial.get();
        if !pending.current_at_before_commit(self, serial, before.input) {
            return Err(ViewError::Revoked.into());
        }
        let mut expected = before.clone();
        if let Some(commit) = &commit {
            commit
                .check_current(&|epoch| self.current(epoch))
                .map_err(ViewError::from)?;
            expected.input = commit.state;
        }
        let actual = self.view()?.capture_scene_request()?;
        if !actual.same(&expected) {
            return Err(ViewError::Revoked.into());
        }
        if let Some(commit) = &commit {
            commit
                .check_current(&|epoch| self.current(epoch))
                .map_err(ViewError::from)?;
        }
        pending.waiting_vertical.borrow_mut().take();
        *pending.request.borrow_mut() = actual;
        *pending.receipt.borrow_mut() = commit;
        Ok(true)
    }

    /// 接到同一垂直收据的原键盘链继续认证下一permit；没有等待者不触碰普通垂直交互。
    pub(in crate::native_surface::paged_body) fn finish_vertical_native_receipt(
        &self,
        commit: Option<&NativeMarkdownPagedInputCommit>,
    ) -> Result<()> {
        let pending = self
            .pending_word_delete
            .borrow()
            .clone()
            .ok_or(ViewError::Revoked)?;
        self.arm_word_successor(&pending, commit)
    }

    /// 仅判断是否存在输入屏障，不捕新Source或创建默认队列。
    pub(in crate::native_surface::paged_body) fn native_input_pending(&self) -> bool {
        self.pending_word_delete.borrow().is_some() || self.pending_vertical.borrow().is_some()
    }
    pub(in crate::native_surface::paged_body) fn native_front_needs_scene(&self) -> bool {
        self.pending_word_delete
            .borrow()
            .as_ref()
            .is_some_and(|pending| pending.native_front())
    }
    pub(in crate::native_surface::paged_body) fn native_waits_for_vertical(&self) -> bool {
        self.pending_word_delete
            .borrow()
            .as_ref()
            .is_some_and(|pending| pending.waiting_vertical.borrow().is_some())
    }
}

impl PendingWordDelete {
    /// 只核队首，不能让后面的event使当前未完成Word失去原Composer续体。
    pub(super) fn native_front(&self) -> bool {
        matches!(self.steps.borrow().front(), Some(WordInput::Native(_)))
    }
    /// Core已提交但Native通知尚未发出，仅比较原before与同一个现存owner/epoch。
    fn current_at_before_commit(
        &self,
        owner: &NativeMarkdownPagedBody,
        serial: u64,
        before: NativeMarkdownPagedInputState,
    ) -> bool {
        owner.current_body()
            && owner.serial.get() == serial
            && owner
                .pending_word_delete
                .borrow()
                .as_ref()
                .is_some_and(|actual| std::ptr::eq(&**actual, self))
            && self.request().input == before
    }
}
