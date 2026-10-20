//! 原Cell动作先有限准入，再归还显示读取，最后只消费同张canonical提交票。

use super::*;
use crate::native_surface::paged_input::{
    NativeMarkdownPagedCellCommit, NativeMarkdownPagedInputOutcome, NativeMarkdownPagedInputPermit,
    NativeMarkdownPagedInputState,
};
use crate::native_surface::paged_text_view::{
    NativeMarkdownPagedCellAction, NativeMarkdownPagedCellPointer,
    NativeMarkdownPagedPointerLocation,
};
use objc2_app_kit::NSScrollView;
use objc2_foundation::{NSPoint, NSRange, NSRect, NSSize};
use omni_markdown_core::markdown_document_buffer::VerifiedMarkdownTableCellSource;
use omni_markdown_core::{
    MarkdownCellDraftWordAnchor, MarkdownPresentationMode, MarkdownTextSelection,
};

/// 父 Body 只持当前一个真实编辑图。子图弱认父亲，原草稿仍只属于 CellSession，
/// 这里的行与 raw 票不复制正文，也不能给后继 canonical 或 SourcePublication 续权。
pub(super) struct ActiveCell {
    pub(super) body: Rc<NativeMarkdownPagedBody>,
    pub(super) scroll: Retained<NSScrollView>,
    row: omni_markdown_core::VerifiedMarkdownLayoutTableRow,
    source: VerifiedMarkdownTableCellSource,
    parent_request: NativeMarkdownPagedSceneRequest,
    content_frame: NSRect,
}

/// 跨提交只保存原点击意图。旧 hit 的 row/raw/frame 会随 canonical 失权，
/// 重新挂载必须从新 Scene 取得，不能为等待而保留旧出版物或大来源票。
pub(super) struct ReopenPointer {
    head: usize,
    affinity: crate::native_surface::paged_text_view::NativeMarkdownPagedHeadAffinity,
    backward_bias: bool,
    clicks: usize,
    extending: bool,
    anchor: Option<usize>,
    word_anchor: Option<crate::native_surface::paged_text_view::NativeMarkdownPagedWordAnchor>,
    dragging: bool,
    intent: Weak<()>,
    /// reveal只保首Down的标量；快Drag/Up另外保一个最新终点，不借旧Hit或Source延长权限。
    endpoint: RefCell<Option<NativeMarkdownPagedPointerLocation>>,
    cancelled: Cell<bool>,
    /// 旧RequiredRow仍占来源时，下一真实Down只保原请求和有限点；不是另一条来源或队列。
    replacement: RefCell<Option<Box<ReopenDown>>>,
}

struct ReopenDown {
    request: NativeMarkdownPagedSceneRequest,
    location: NativeMarkdownPagedPointerLocation,
}

impl From<NativeMarkdownPagedCellPointer> for ReopenPointer {
    fn from(pointer: NativeMarkdownPagedCellPointer) -> Self {
        Self {
            head: pointer.hit.head_utf16,
            affinity: pointer.hit.head_affinity,
            backward_bias: pointer.hit.backward_bias,
            clicks: pointer.clicks,
            extending: pointer.extending,
            anchor: pointer.anchor,
            word_anchor: pointer.word_anchor,
            dragging: pointer.dragging,
            intent: pointer.intent,
            endpoint: RefCell::new(None),
            cancelled: Cell::new(false),
            replacement: RefCell::new(None),
        }
    }
}

impl ReopenPointer {
    /// 几何与来源只取当前真实 hit，原位置/亲和仍是已接纳的同一次鼠标意图。
    fn pointer(
        &self,
        mut hit: crate::native_surface::paged_text_view::NativeMarkdownPagedCellHit,
    ) -> NativeMarkdownPagedCellPointer {
        hit.head_utf16 = self.head;
        hit.head_affinity = self.affinity;
        hit.backward_bias = self.backward_bias;
        NativeMarkdownPagedCellPointer {
            hit,
            clicks: self.clicks,
            extending: self.extending,
            anchor: self.anchor,
            word_anchor: self.word_anchor,
            dragging: self.dragging,
            intent: self.intent.clone(),
        }
    }
}

/// 一次真实动作至多占一个槽；Pointer 等自己的 Cell Source，导航等父亲唯一来源请求。
/// 这些是已经接纳的业务续体，不是每格任务、历史缓存或另一份可写正文。
pub(super) enum PendingCell {
    /// 原手势的最后一个真实窗口终点；只保数值和弱票，不保事件或旧 Scene/Source。
    ParentPointer {
        request: RefCell<NativeMarkdownPagedSceneRequest>,
        pointer: RefCell<NativeMarkdownPagedPointerLocation>,
        /// 初Down尚未命中时单独保其窗口点；最新Drag不得替换这个真实首查询输入。
        down: RefCell<Option<NativeMarkdownPagedPointerLocation>>,
        waiting_group: Cell<bool>,
        /// 仅真实child自身Down越界时存在；其request/anchor仍为child局部域。
        child: Option<Weak<ActiveCell>>,
    },
    Pointer {
        owner: Weak<ActiveCell>,
        pointer: NativeMarkdownPagedCellPointer,
        applied: Cell<bool>,
        endpoint: RefCell<Option<NativeMarkdownPagedPointerLocation>>,
    },
    Navigation {
        owner: Weak<ActiveCell>,
        permit: RefCell<Option<NativeMarkdownPagedInputPermit>>,
        action: NativeMarkdownPagedCellAction,
        result: RefCell<Option<semantics::CellNavigationStep>>,
    },
    Reopen {
        state: NativeMarkdownPagedInputState,
        position: usize,
        column: usize,
        next_row: bool,
        destination: RefCell<Option<semantics::CellDestination>>,
        target: RefCell<Option<Rc<target::SourceTargetRequest>>>,
        pointer: Option<ReopenPointer>,
    },
    Closing {
        owner: Weak<ActiveCell>,
        /// 原父请求的在途导航真正交回后才移除子图，不拿局部取消当来源任务已完成。
        drained: Cell<bool>,
    },
}

impl NativeMarkdownPagedBody {
    /// 预览插表沿唯一原提交收据进入首格，来源准备/远端定位/挂载继续使用已有 Reopen。
    /// Source 模式仍只显示字面模板与其同笔 caret，不自动切换输入域或新增控件。
    pub(super) fn accept_inserted_table(
        &self,
        commit: &NativeMarkdownPagedInputCommit,
    ) -> Result<()> {
        commit
            .check_current(&|epoch| self.current(epoch))
            .map_err(ViewError::Input)?;
        if self.presentation_mode() != MarkdownPresentationMode::InlinePreview {
            return Ok(());
        }
        if self.pending_cell.borrow().is_some() || self.active_cell.borrow().is_some() {
            // 提交通知允许真实 Native 重入；已接纳的新 Down/格动作优先，自动聚焦不能覆盖它。
            // 模板仍是已经完成的一笔事务，不重试插入，也不撤销用户此后接纳的实际意图。
            return Ok(());
        }
        drop(self.selection_reveal.borrow_mut().take());
        self.revoke_source_target(); // 先撤普通 caret reveal；同次首格只保这一条真实定位意图。
        *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::Reopen {
            state: commit.state,
            position: commit.state.selection.head_utf16,
            column: 0,
            next_row: false,
            destination: RefCell::new(None),
            target: RefCell::new(None),
            pointer: None,
        }));
        // 下一原量子先走 RequiredRow/CellInline，再消费 SourceTarget 的实测内容框；
        // 此处没有 Scene/Source 强边，也不在按钮回调内同步解析表格或创建空占位 child。
        self.schedule()
    }

    /// 菜单只借当前实际子 owner，Weak 身份与 InputState 一起使用，不能用视图地址或空历史回落正文。
    pub(in crate::native_surface) fn active_cell_body(&self) -> Option<Rc<Self>> {
        self.active_cell
            .borrow()
            .as_ref()
            .map(|active| active.body.clone())
    }

    pub(super) fn notify_local_cell_input(&self) {
        let observer = self
            .input_observer
            .borrow()
            .as_ref()
            .and_then(Weak::upgrade);
        if let Some(observer) = observer {
            observer.local_input_changed();
        }
    }

    /// 新格只借父亲已注册字体、catalog 与已冻结段落；原 mapping 的最后释放仍由全部真实引用决定。
    /// 滚动容器只覆盖当前真实内容框与视口交集，巨格高度不会变成 Native backing 大小。
    fn mount_cell(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        hit: &crate::native_surface::paged_text_view::NativeMarkdownPagedCellHit,
    ) -> Result<Rc<ActiveCell>> {
        if self.input.is_cell() || self.active_cell.borrow().is_some() {
            return Err(NativeMarkdownPagedBodyError::Busy);
        }
        if self.cell_close.borrow().is_some() {
            return Err(ViewError::Pending.into());
        }
        let left = hit.content_frame.origin.x.max(request.viewport.origin.x);
        let bottom = hit.content_frame.origin.y.max(request.viewport.origin.y);
        let right = (hit.content_frame.origin.x + hit.content_frame.size.width)
            .min(request.viewport.origin.x + request.viewport.size.width);
        let top = (hit.content_frame.origin.y + hit.content_frame.size.height)
            .min(request.viewport.origin.y + request.viewport.size.height);
        if right <= left || top <= bottom {
            return Err(ViewError::Pending.into()); // 行顶 padding 无文字交集，不能创造一张空编辑框。
        }
        let session = NativeMarkdownPagedCellSession::begin(
            self.input.document_identity(),
            hit.source.clone(),
        )
        .map_err(ViewError::Input)?;
        let frame = NSRect::new(
            NSPoint::new(left, bottom),
            NSSize::new(right - left, top - bottom),
        );
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(self.mtm), frame);
        scroll.setHasVerticalScroller(true);
        scroll.setHasHorizontalScroller(false);
        scroll.setDrawsBackground(true);
        let body = Self::mount_target(
            Attachment::Cell(session),
            &scroll,
            self.fonts.borrow().clone(),
            self.catalog.clone(),
            self.paragraph.clone(),
            None,
            NativeMarkdownPagedBodyConfiguration {
                presentation_mode: MarkdownPresentationMode::Source,
                epoch: self.configuration.epoch,
                maximum_font_bytes: self.configuration.maximum_font_bytes,
                padding: 0.0,
                colors: self.configuration.colors,
            },
        )?;
        *body.cell_parent.borrow_mut() = self.weak.clone();
        let active = Rc::new(ActiveCell {
            body,
            scroll,
            row: hit.row.clone(),
            source: hit.source.clone(),
            parent_request: request.clone(),
            content_frame: hit.content_frame,
        });
        // 先发布唯一拥有者再调用可重入 Native；失败只拆这一原 owner，不覆盖后继格。
        *self.active_cell.borrow_mut() = Some(active.clone());
        let mounted = (|| {
            let parent = self.view()?;
            parent.addSubview(&active.scroll);
            let child = active.body.view()?;
            let window = parent.window().ok_or(ViewError::Pending)?;
            if !self.owns_cell(&active) || !window.makeFirstResponder(Some(&**child)) {
                return Err(ViewError::Revoked.into());
            }
            Ok(())
        })();
        if let Err(cause) = mounted {
            let cleanup = self.detach_cell(&active).map(|_| ());
            return producer::merge_preparation_cleanup(Err(cause), cleanup).map(|_| active);
        }
        self.notify_local_cell_input();
        Ok(active)
    }

    fn owns_cell(&self, active: &Rc<ActiveCell>) -> bool {
        self.active_cell
            .borrow()
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, active))
    }

    /// 点击先把真实 raw 位置交同一 Cell Input，再由原选择 reveal 驱动远端字形；
    /// 不能等零号页 Ready 后拿它冒充巨格中段的可见性。
    fn seed_cell_pointer(
        &self,
        active: &Rc<ActiveCell>,
        pointer: &NativeMarkdownPagedCellPointer,
    ) -> Result<()> {
        let document = self
            .document
            .borrow()
            .as_ref()
            .expect("格只挂原正文")
            .clone();
        let range = active
            .source
            .canonical_range_for(&document.borrow())?
            .clone();
        let head = pointer
            .hit
            .head_utf16
            .checked_sub(range.start)
            .filter(|head| *head <= range.end - range.start)
            .ok_or(ViewError::InvalidRange)?;
        let child = &active.body;
        let current = |epoch| child.current(epoch) && self.owns_cell(active);
        let permit = child
            .input
            .capture(child.configuration.epoch, &current)
            .map_err(ViewError::Input)?;
        let outcome = child
            .input
            .set_selection_intent(
                &permit,
                MarkdownTextSelection::caret(head),
                Some(pointer.hit.head_affinity.is_upstream()),
                None,
                &current,
            )
            .map_err(ViewError::Input)?;
        if let NativeMarkdownPagedInputOutcome::Committed(commit) = outcome {
            child.view()?.finish_word_delete_commit(&commit)?;
        } else {
            child.reveal_current_selection()?;
        }
        Ok(())
    }

    pub(super) fn accept_cell_pointer(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        pointer: NativeMarkdownPagedCellPointer,
        permit: NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        if self.input.is_cell()
            || self.presentation_mode() != MarkdownPresentationMode::InlinePreview
        {
            return Ok(false);
        }
        self.input
            .check_permit(&permit, &|epoch| self.current(epoch))?;
        if self.input.state()? != request.input {
            return Err(ViewError::Revoked);
        }
        if self
            .pending_cell
            .borrow()
            .as_ref()
            .is_some_and(|pending| match &**pending {
                PendingCell::Pointer { .. } => false,
                PendingCell::ParentPointer {
                    request: original,
                    pointer: original_pointer,
                    ..
                } => {
                    !original.borrow().same(request)
                        || !Weak::ptr_eq(&original_pointer.borrow().intent, &pointer.intent)
                }
                _ => true,
            })
        {
            return Err(ViewError::Pending);
        }
        let document = self
            .document
            .borrow()
            .as_ref()
            .expect("格指针属于正文")
            .clone();
        let target_range = pointer
            .hit
            .source
            .canonical_range_for(&document.borrow())
            .map_err(|cause| cell_view_error(cause.into()))?
            .clone();
        let (_, _, column) = pointer
            .hit
            .source
            .coordinate_for(&document.borrow())
            .map_err(|cause| cell_view_error(cause.into()))?;
        let active = self.active_cell.borrow().clone();
        if let Some(active) = active {
            let same = active
                .source
                .canonical_range_for(&document.borrow())
                .map_err(|cause| cell_view_error(cause.into()))?
                == &target_range
                && active
                    .source
                    .coordinate_for(&document.borrow())
                    .map_err(|cause| cell_view_error(cause.into()))?
                    == pointer
                        .hit
                        .source
                        .coordinate_for(&document.borrow())
                        .map_err(|cause| cell_view_error(cause.into()))?;
            // 父 View 已接纳的 Word 终点仍归父手势。即使目标和首组在同一个 raw，
            // 首组 Ready 后也须发布 canonical 扩选；重新把同一个 Up 送回 child 会形成自循环。
            // child 自身 Down 的局部选词不经过这个父 ParentPointer 槽，原编辑/历史角色不变。
            let parent_word_endpoint = pointer.dragging
                && pointer.clicks == 2
                && matches!(
                    pointer.word_anchor,
                    Some(
                        crate::native_surface::paged_text_view::NativeMarkdownPagedWordAnchor::Group { .. }
                            | crate::native_surface::paged_text_view::NativeMarkdownPagedWordAnchor::Selection(_)
                    )
                )
                && self.pending_cell.borrow().as_ref().is_some_and(|pending| {
                    matches!(&**pending, PendingCell::ParentPointer {
                        request: original,
                        pointer: endpoint,
                        child: None,
                        ..
                    } if original.borrow().same(request)
                        && Weak::ptr_eq(&endpoint.borrow().intent, &pointer.intent)
                        && (endpoint.borrow().dragging || endpoint.borrow().mouse_up))
                });
            if !same || parent_word_endpoint {
                if pointer.extending {
                    use crate::native_surface::paged_text_view::NativeMarkdownPagedCellExtension as Extension;
                    // 跨格仍保首击的真实选择粒度；双击词拖选不能在这里退化为逐字符caret扩选。
                    let extension = if pointer.clicks == 2 {
                        Extension::Word {
                            selection: crate::native_surface::paged_text_view::NativeMarkdownPagedWordSelection {
                                head_utf16: pointer.hit.head_utf16,
                                backward_bias: pointer.hit.backward_bias,
                                anchor: pointer.word_anchor,
                                cell: pointer.hit.word_target,
                            },
                            intent: pointer.intent.clone(),
                        }
                    } else {
                        Extension::Selection {
                            selection: MarkdownTextSelection {
                                anchor_utf16: pointer
                                    .anchor
                                    .unwrap_or(request.input.selection.anchor_utf16),
                                head_utf16: pointer.hit.head_utf16,
                            },
                            head_affinity: pointer.hit.head_affinity,
                            intent: pointer.intent.clone(),
                        }
                    };
                    return self.extend_cell_selection(request, extension, permit);
                }
                let position = pointer.hit.row.source_range().start;
                let committed = self
                    .commit_owned_cell(&active, None, None, None, None, None)
                    .map_err(cell_view_error)?;
                let pointer =
                    map_pointer(pointer.into(), &committed.outcome).map_err(cell_view_error)?;
                let position =
                    map_position(position, &committed.outcome).map_err(cell_view_error)?;
                let state = self.input.state()?;
                *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::Reopen {
                    state,
                    position,
                    column,
                    next_row: false,
                    destination: RefCell::new(None),
                    target: RefCell::new(None),
                    pointer: Some(pointer),
                }));
                self.schedule().map_err(cell_view_error)?;
                return Ok(true);
            }
            *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::Pointer {
                owner: Rc::downgrade(&active),
                pointer,
                applied: Cell::new(false),
                endpoint: RefCell::new(None),
            }));
        } else {
            // 跨格 Shift 已有正文锚点时保真实正文扩选，不把不在 raw 内的锚点塞进局部 Input。
            if pointer.extending
                && pointer
                    .anchor
                    .is_some_and(|anchor| anchor < target_range.start || anchor > target_range.end)
            {
                return Ok(false);
            }
            let active = match self.mount_cell(request, &pointer.hit) {
                Ok(active) => active,
                Err(NativeMarkdownPagedBodyError::View(ViewError::Pending)) => {
                    // 行顶padding、已排完短格下方或上一张像素票尚未归还时，原Down不能丢失。
                    // 只登记真实行/列查询，仍由同一Request与SourceTarget先取得可见内容框。
                    *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::Reopen {
                        state: request.input,
                        position: pointer.hit.row.source_range().start,
                        column,
                        next_row: false,
                        destination: RefCell::new(None),
                        target: RefCell::new(None),
                        pointer: Some(pointer.into()),
                    }));
                    self.schedule().map_err(cell_view_error)?;
                    return Ok(true);
                }
                Err(cause) => return Err(cell_view_error(cause)),
            };
            self.seed_cell_pointer(&active, &pointer)
                .map_err(cell_view_error)?;
            *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::Pointer {
                owner: Rc::downgrade(&active),
                pointer,
                applied: Cell::new(false),
                endpoint: RefCell::new(None),
            }));
        }
        self.finish_cell_work().map_err(cell_view_error)?;
        Ok(true)
    }

    /// 父子实际 Ready 后只恢复已接纳的数值意图。原 Up 可以先到，完成终点选择不复活拖选槽。
    pub(super) fn finish_cell_work(&self) -> Result<()> {
        if self.input.is_cell() {
            let pending = self.pending_cell.borrow().clone();
            if let Some(pending) = pending
                && matches!(&*pending, PendingCell::ParentPointer { .. })
            {
                self.finish_deferred_cell_pointer(&pending)?;
            } // 当前子 Source 同样可等待自己的真实 Scene；取消只撤本域，不能转父撤掉已移交的手势。
            if let Some(parent) = self.cell_parent.borrow().upgrade() {
                parent.finish_cell_work()?;
            }
            return Ok(());
        }
        let pending = self.pending_cell.borrow().clone();
        let Some(pending) = pending else {
            return self.reposition_cell();
        };
        match &*pending {
            PendingCell::ParentPointer { .. } => {
                self.finish_deferred_cell_pointer(&pending)?;
            }
            PendingCell::Pointer {
                owner,
                pointer,
                applied,
                endpoint,
            } => {
                let Some(active) = owner.upgrade().filter(|active| self.owns_cell(active)) else {
                    self.clear_cell_pending(&pending);
                    return Ok(());
                };
                if active.body.preparing.get() || applied.get() {
                    return Ok(()); // 首查询已真正接纳，只等它的真实Group，不重送初Down。
                }
                let document = self.document.borrow().as_ref().expect("实际父Doc").clone();
                let range = active
                    .source
                    .canonical_range_for(&document.borrow())?
                    .clone();
                match active
                    .body
                    .view()?
                    .apply_cell_pointer(pointer, range.clone())
                {
                    Ok(true) => {
                        if pointer.clicks == 2 {
                            applied.set(true);
                        } else {
                            // 先结束终点槽的可变借用，再读可重入Native几何；回调可更新同一手势。
                            let latest = endpoint.borrow_mut().take();
                            if let Some(pointer) = latest {
                                let request = self.view()?.capture_scene_request()?;
                                *self.pending_cell.borrow_mut() =
                                    Some(Rc::new(PendingCell::ParentPointer {
                                        request: RefCell::new(request),
                                        pointer: RefCell::new(pointer),
                                        down: RefCell::new(None),
                                        waiting_group: Cell::new(false),
                                        child: None,
                                    }));
                                self.schedule()?;
                            } else {
                                self.clear_cell_pending(&pending);
                            }
                        }
                    }
                    Ok(false) | Err(ViewError::Pending | ViewError::Busy) => {}
                    Err(cause) => return Err(cause.into()),
                }
            }
            PendingCell::Navigation {
                owner,
                action,
                result,
                ..
            } => {
                let Some(result) = result.borrow_mut().take() else {
                    return Ok(());
                };
                let Some(active) = owner.upgrade().filter(|active| self.owns_cell(active)) else {
                    self.clear_cell_pending(&pending);
                    return Ok(());
                };
                self.finish_cell_navigation(&pending, &active, *action, result)?;
            }
            PendingCell::Reopen {
                state,
                destination,
                target,
                pointer,
                ..
            } => {
                if pointer
                    .as_ref()
                    .is_some_and(|pointer| pointer.cancelled.get())
                {
                    return Ok(()); // 已取消的点击只剩原来源归槽责任，不能借旧Ready重新挂载。
                }
                if self.cell_close.borrow().is_some() {
                    return Ok(());
                }
                if self.input.state().map_err(ViewError::Input)? != *state {
                    self.clear_cell_pending(&pending);
                    return Ok(());
                }
                let original_target = target.borrow().clone();
                let ready = original_target
                    .as_ref()
                    .is_some_and(|target| !target.pending());
                let empty = match &original_target {
                    Some(target) => self.reopening_empty_cell_target(target)?,
                    None => false,
                };
                if !ready && !empty {
                    return Ok(());
                }
                let scene = self.scene.borrow().clone();
                let Some(scene) = scene else {
                    return Ok(());
                };
                let hit = {
                    let destination = destination.borrow();
                    let Some(destination) = destination.as_ref() else {
                        return Ok(());
                    };
                    scene.cell_source_hit(&destination.source)?
                };
                let Some(hit) = hit else {
                    return Ok(());
                };
                let active = self.mount_cell(&scene.key().request, &hit)?;
                if empty && let Some(target) = original_target {
                    // 真空格只消费刚核同 Doc/坐标/格种类且与视口相交的实际内容框。
                    // 挂载成功后退休这个精确导航目标，不伪造父图不存在的 glyph/Find proof；
                    // 子 Source 随后仍须真实准备自己的空行、caret 与输入协议。
                    target.revoke();
                    let mut slot = self.source_target.borrow_mut();
                    if slot
                        .as_ref()
                        .is_some_and(|actual| Rc::ptr_eq(actual, &target))
                    {
                        slot.take();
                    }
                }
                if let Some(pointer) = pointer {
                    let down = pointer.pointer(hit);
                    self.seed_cell_pointer(&active, &down)?;
                    // seed包含Native通知；原槽若被重入的新Down取消，不能覆盖后继或复活旧拖动。
                    let original = self
                        .pending_cell
                        .borrow()
                        .as_ref()
                        .is_some_and(|actual| Rc::ptr_eq(actual, &pending));
                    if !original || pointer.cancelled.get() {
                        return Ok(());
                    }
                    let endpoint = pointer.endpoint.borrow_mut().take();
                    *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::Pointer {
                        owner: Rc::downgrade(&active),
                        pointer: down,
                        applied: Cell::new(false),
                        endpoint: RefCell::new(endpoint),
                    }));
                } else {
                    self.clear_cell_pending(&pending);
                    active.body.view()?.command(objc2::sel!(selectAll:))?; // 原表格键盘导航选中目标格全部 raw，不新增快捷键。
                }
            }
            PendingCell::Closing { owner, drained } => {
                if !drained.get() {
                    return Ok(()); // RequiredRow/CellInline 仍占原请求，下一原量子只负责归还它。
                }
                if let Some(active) = owner.upgrade().filter(|active| self.owns_cell(active)) {
                    if active.body.preflight_release().is_err() {
                        return Ok(());
                    }
                    self.detach_cell(&active)?;
                    self.focus_cell_parent()?;
                }
                self.clear_cell_pending(&pending);
            }
        }
        self.reposition_cell()
    }

    fn clear_cell_pending(&self, pending: &Rc<PendingCell>) {
        let mut slot = self.pending_cell.borrow_mut();
        if slot
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, pending))
        {
            slot.take();
        }
    }

    /// 已解码终点只进入原手势当前一个槽；原Down和最新终点各有真实职责，不能彼此覆盖。
    pub(super) fn defer_cell_pointer(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        pointer: NativeMarkdownPagedPointerLocation,
        permit: NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        self.input
            .check_permit(&permit, &|epoch| self.current(epoch))?;
        let view = self.view().map_err(cell_view_error)?;
        if !view.cell_pointer_is_current(&pointer.intent)
            || !view.capture_scene_request()?.same(request)
            || !view.cell_pointer_is_current(&pointer.intent)
        {
            return Err(ViewError::Revoked);
        }
        self.input
            .check_permit(&permit, &|epoch| self.current(epoch))?;
        // 同点 Up 不再重新命中，但首个 Select 可能还占原 Word producer。
        // 结束事实必须等它交回真实组；否则空操作会过早清槽，新 Down 也就无法撤旧查询。
        let waiting_word = self.has_pending_pointer_word(&pointer.intent);
        let occupied = self.pending_cell.borrow().clone();
        if let Some(pending) = occupied {
            match &*pending {
                PendingCell::Pointer {
                    pointer: down,
                    endpoint,
                    ..
                } if Weak::ptr_eq(&down.intent, &pointer.intent) => {
                    if endpoint.borrow().as_ref().is_some_and(|end| end.mouse_up) {
                        return Err(ViewError::Revoked);
                    }
                    *endpoint.borrow_mut() = Some(pointer);
                    self.schedule().map_err(cell_view_error)?;
                    return Ok(true); // child首查询未完时只更新终点，原raw/首击仍归原Down。
                }
                PendingCell::ParentPointer {
                    request: original,
                    pointer: endpoint,
                    waiting_group,
                    child: None,
                    ..
                } if original.borrow().same(request)
                    && Weak::ptr_eq(&endpoint.borrow().intent, &pointer.intent)
                    && !endpoint.borrow().mouse_up =>
                {
                    *endpoint.borrow_mut() = pointer;
                    if waiting_word {
                        waiting_group.set(true);
                    }
                    self.schedule().map_err(cell_view_error)?;
                    return Ok(true);
                }
                PendingCell::Reopen {
                    pointer: Some(reopen),
                    state,
                    ..
                } => {
                    if reopen.cancelled.get() {
                        if pointer.dragging || pointer.mouse_up {
                            let original =
                                reopen.replacement.borrow().as_ref().is_some_and(|down| {
                                    down.request.same(request)
                                        && Weak::ptr_eq(&down.location.intent, &pointer.intent)
                                });
                            if !original
                                || reopen
                                    .endpoint
                                    .borrow()
                                    .as_ref()
                                    .is_some_and(|end| end.mouse_up)
                            {
                                return Err(ViewError::Revoked);
                            }
                            *reopen.endpoint.borrow_mut() = Some(pointer);
                        } else {
                            *reopen.replacement.borrow_mut() = Some(Box::new(ReopenDown {
                                request: request.clone(),
                                location: pointer,
                            }));
                            reopen.endpoint.borrow_mut().take();
                        }
                    } else {
                        if request.input != *state
                            || !Weak::ptr_eq(&reopen.intent, &pointer.intent)
                            || reopen
                                .endpoint
                                .borrow()
                                .as_ref()
                                .is_some_and(|end| end.mouse_up)
                        {
                            return Err(ViewError::Revoked);
                        }
                        *reopen.endpoint.borrow_mut() = Some(pointer);
                    }
                    self.schedule().map_err(cell_view_error)?;
                    return Ok(true); // 同槽只保一个真实首Down及最新终点，原来源仍在先完成归还。
                }
                _ => return Err(ViewError::Pending),
            }
        }
        let down = (!pointer.dragging && !pointer.mouse_up).then(|| pointer.clone());
        let pending = Rc::new(PendingCell::ParentPointer {
            request: RefCell::new(request.clone()),
            pointer: RefCell::new(pointer),
            down: RefCell::new(down),
            waiting_group: Cell::new(waiting_word),
            child: None,
        });
        *self.pending_cell.borrow_mut() = Some(pending.clone());
        if let Err(cause) = self.schedule() {
            self.clear_cell_pending(&pending);
            return Err(cell_view_error(cause));
        }
        Ok(true)
    }

    /// 新Down/输入明确撤原终点；只取消该Weak对应的纯选词，不碰已承诺的文字或导航。
    pub(super) fn cancel_deferred_cell_pointer(&self) -> bool {
        let pending = self.pending_cell.borrow().clone();
        let Some(pending) = pending else {
            if self.input.is_cell()
                && let Some(parent) = self.cell_parent.borrow().upgrade()
            {
                parent.cancel_child_pointer(self);
            }
            return false;
        };
        let (intent, child) = match &*pending {
            PendingCell::Reopen {
                pointer: Some(pointer),
                ..
            } => {
                // 已启动来源必须先实际归槽；这里只撤旧指针业务意图，不把取消当作释放完成。
                pointer.cancelled.set(true);
                pointer.endpoint.borrow_mut().take();
                pointer.replacement.borrow_mut().take();
                drop(self.selection_reveal.borrow_mut().take());
                self.revoke_source_target(); // 新真实输入撤旧位置意图；来源作业仍由下方原量子drain。
                if let Err(cause) = self.schedule() {
                    self.fail(cause);
                }
                return true;
            }
            PendingCell::Pointer { pointer, owner, .. } => {
                (pointer.intent.clone(), owner.upgrade())
            }
            PendingCell::ParentPointer { pointer, child, .. } => (
                pointer.borrow().intent.clone(),
                child.as_ref().and_then(Weak::upgrade),
            ),
            _ => return false,
        };
        self.clear_cell_pending(&pending);
        let result = if let Some(child) = child {
            child.body.cancel_pointer_word(&intent)
        } else {
            self.cancel_pointer_word(&intent)
        };
        if let Err(cause) = result {
            self.fail(cause);
        }
        if self.input.is_cell()
            && let Some(parent) = self.cell_parent.borrow().upgrade()
        {
            parent.cancel_child_pointer(self);
        }
        false
    }

    fn cancel_child_pointer(&self, child: &Self) {
        let pending = self.pending_cell.borrow().clone();
        let Some(pending) = pending else {
            return;
        };
        let owner = match &*pending {
            PendingCell::Pointer { owner, .. } => owner.upgrade(),
            PendingCell::ParentPointer {
                child: Some(owner), ..
            } => owner.upgrade(),
            _ => None,
        };
        if owner.is_some_and(|owner| Weak::ptr_eq(&owner.body.weak, &child.weak)) {
            self.cancel_deferred_cell_pointer();
        }
    }

    /// 原 Word 查询因真实输入/几何撤权而结束时，同手势的待组事实也结束，不能永远等待不会再来的 Ready。
    /// 只清具体已接纳查询的元数据；活动格、Reopen 与在途 RequiredRow/CellInline 仍由原 owner 归还。
    pub(super) fn cancel_waiting_pointer_word(&self, intent: &Weak<()>) {
        let pending = self.pending_cell.borrow().clone();
        if let Some(pending) = pending
            && let PendingCell::ParentPointer {
                pointer,
                waiting_group,
                child: None,
                ..
            } = &*pending
            && waiting_group.get()
            && Weak::ptr_eq(&pointer.borrow().intent, intent)
        {
            self.clear_cell_pending(&pending);
        }
        if !self.input.is_cell() {
            return;
        }
        let Some(parent) = self.cell_parent.borrow().upgrade() else {
            return;
        };
        let pending = parent.pending_cell.borrow().clone();
        let Some(pending) = pending else {
            return;
        };
        let owner = match &*pending {
            PendingCell::Pointer {
                owner,
                pointer,
                applied,
                ..
            } if applied.get() && Weak::ptr_eq(&pointer.intent, intent) => owner.upgrade(),
            PendingCell::ParentPointer {
                pointer,
                waiting_group,
                child: Some(owner),
                ..
            } if waiting_group.get() && Weak::ptr_eq(&pointer.borrow().intent, intent) => {
                owner.upgrade()
            }
            _ => None,
        };
        if owner.is_some_and(|owner| {
            parent.owns_cell(&owner) && Weak::ptr_eq(&owner.body.weak, &self.weak)
        }) {
            parent.clear_cell_pending(&pending);
        } // 后继 Down 即使选区未变也有独立 Weak；它不会被这次旧查询的清理取走。
    }

    /// 真帧发布后只消费一次既有意图；child局部终点必须先沿原父子提交，不能直接落父Input。
    fn finish_deferred_cell_pointer(&self, pending: &Rc<PendingCell>) -> Result<()> {
        let PendingCell::ParentPointer {
            request,
            pointer,
            down,
            waiting_group,
            child,
        } = &**pending
        else {
            unreachable!("只推进原指针续体");
        };
        let request = request.borrow().clone();
        let pointer = pointer.borrow().clone();
        if let Some(child) = child {
            let Some(active) = child.upgrade().filter(|active| self.owns_cell(active)) else {
                self.clear_cell_pending(pending);
                return Ok(());
            };
            if waiting_group.get() || active.body.preparing.get() || active.body.has_pending_input()
            {
                return Ok(()); // 真实首组/文字仍占原child，原完成通知才恢复，不轮询或假签Ready。
            }
            let view = active.body.view()?;
            if !view.capture_scene_request()?.same(&request) {
                self.clear_cell_pending(pending);
                return Ok(());
            }
            let parent_request = self.view()?.capture_scene_request()?;
            match self.ready_scene(&parent_request) {
                Ok(_) => {}
                Err(ViewError::Pending) => return Ok(()),
                Err(cause) => return Err(cause.into()),
            }
            let permit = active
                .body
                .input
                .capture(active.body.configuration.epoch, &|epoch| {
                    active.body.current(epoch) && self.owns_cell(&active)
                })
                .map_err(ViewError::Input)?;
            self.apply_cell_drag(&active.body, &request, pointer, permit)?;
            self.clear_cell_pending(pending);
            return Ok(());
        }
        if waiting_group.get() {
            return Ok(());
        }
        if self
            .active_cell
            .borrow()
            .as_ref()
            .is_some_and(|active| active.body.preparing.get() || active.body.has_pending_input())
        {
            // 首组回交先于 child 的本步显示通知/队首归还；原完成出口还会回到这里。
            // 这个已接纳父终点等待同一真实输入结束，不能把中途 Busy/Pending 当正文 fatal。
            return Ok(());
        }
        let initial = down.borrow().clone();
        let actual = initial.as_ref().unwrap_or(&pointer);
        // Up已由同一实际owner接纳时，初Down仍须完成首次命中；不改Down角色，也不重新授予拖动。
        let accepted_up = (initial.is_some() && pointer.mouse_up).then_some(&pointer);
        let result = self
            .view()?
            .apply_deferred_pointer(&request, actual, accepted_up);
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?;
        }
        match result {
            Ok(true) => {
                let successor = self.pending_cell.borrow().clone();
                if let Some(successor) = successor
                    && !Rc::ptr_eq(&successor, pending)
                {
                    // 首Down刚取得真实Grid命中时，原owner可把同槽转成Pointer。
                    // 早到Drag/Up仍属于这个Down，须交给新阶段，不能随旧Rc退出而丢掉终点。
                    if let PendingCell::Pointer {
                        pointer: original,
                        endpoint,
                        ..
                    } = &*successor
                        && Weak::ptr_eq(&original.intent, &pointer.intent)
                        && (pointer.dragging || pointer.mouse_up)
                    {
                        *endpoint.borrow_mut() = Some(pointer.clone());
                    }
                    if let PendingCell::Reopen {
                        pointer: Some(reopen),
                        ..
                    } = &*successor
                        && Weak::ptr_eq(&reopen.intent, &pointer.intent)
                        && (pointer.dragging || pointer.mouse_up)
                    {
                        *reopen.endpoint.borrow_mut() = Some(pointer.clone());
                    } // reveal同样继承这个已接纳终点，不能因尚无文字交集就忘掉原Up。
                    return Ok(());
                }
                if initial.as_ref().is_some_and(|down| down.clicks == 2) {
                    down.borrow_mut().take();
                    waiting_group.set(true); // 原首Query已接纳，不能再把这个初Down提交第二次。
                } else {
                    self.clear_cell_pending(pending);
                }
            }
            Ok(false) => {}
            Err(ViewError::Revoked) => self.clear_cell_pending(pending),
            Err(cause) => return Err(cause.into()),
        }
        Ok(())
    }

    /// 只有当前重挂续体绑定的真实零长 raw 格才没有父图字形可证明。
    /// 原 Find/模式目标不属于此槽，不能借这个判定取得空的字符可见证明。
    pub(super) fn reopening_empty_cell_target(
        &self,
        original: &Rc<target::SourceTargetRequest>,
    ) -> Result<bool> {
        let pending = self.pending_cell.borrow().clone();
        let Some(pending) = pending else {
            return Ok(false);
        };
        let PendingCell::Reopen {
            destination,
            target,
            ..
        } = &*pending
        else {
            return Ok(false);
        };
        if !target
            .borrow()
            .as_ref()
            .is_some_and(|target| Rc::ptr_eq(target, original))
        {
            return Ok(false);
        }
        let destination = destination.borrow();
        let Some(destination) = destination.as_ref() else {
            return Ok(false);
        };
        let document = self
            .document
            .borrow()
            .as_ref()
            .expect("重挂仍归原文档")
            .clone();
        let document = document.try_borrow().map_err(|_| ViewError::Busy)?;
        Ok(destination
            .source
            .canonical_range_for(&document)?
            .is_empty())
    }

    /// 父请求原量子先处理实际导航；Busy只让已有Font/CellInline继续，Pending不偷推进第二份语义。
    pub(super) fn advance_pending_cell(
        &self,
        producer: &mut Producer,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<bool> {
        let pending = self.pending_cell.borrow().clone();
        let Some(pending) = pending else {
            return Ok(false);
        };
        match &*pending {
            PendingCell::Navigation {
                owner,
                action,
                result,
                ..
            } => {
                if result.borrow().is_some() {
                    return Ok(true);
                }
                let Some(active) = owner.upgrade().filter(|active| self.owns_cell(active)) else {
                    self.clear_cell_pending(&pending);
                    return Ok(false);
                };
                let document = self
                    .document
                    .borrow()
                    .as_ref()
                    .expect("原格导航文档")
                    .clone();
                let (_, _, column) = active.source.coordinate_for(&document.borrow())?;
                let step = producer.navigate_cell(self, &active.row, column, *action)?;
                match step {
                    semantics::CellNavigationStep::Occupied => return Ok(false),
                    semantics::CellNavigationStep::Pending => self.schedule()?,
                    step => *result.borrow_mut() = Some(step),
                }
                Ok(true)
            }
            PendingCell::Reopen {
                state,
                position,
                column,
                next_row,
                destination,
                target,
                pointer,
            } => {
                if let Some(pointer) = pointer
                    && pointer.cancelled.get()
                {
                    // drain消费原请求已经启动的量子及其take结果；不创建替代来源、不把标记当完成。
                    if !producer.drain_cell_navigation(self)? {
                        self.schedule()?;
                        return Ok(true);
                    }
                    let original_target = target.borrow_mut().take();
                    if let Some(original) = original_target {
                        original.revoke();
                        let mut slot = self.source_target.borrow_mut();
                        if slot
                            .as_ref()
                            .is_some_and(|actual| Rc::ptr_eq(actual, &original))
                        {
                            slot.take();
                        }
                    }
                    let replacement = pointer.replacement.borrow_mut().take();
                    let endpoint = pointer.endpoint.borrow_mut().take();
                    if let Some(replacement) = replacement
                        && replacement.request.same(request)
                    {
                        // 同槽只保这一次最新Down及终点。归槽完成后原几何仍相同才允许继续，
                        // 外部编辑/resize已经撤权时不从当前request补签旧窗口点。
                        let end = endpoint.unwrap_or_else(|| replacement.location.clone());
                        *self.pending_cell.borrow_mut() =
                            Some(Rc::new(PendingCell::ParentPointer {
                                request: RefCell::new(replacement.request),
                                pointer: RefCell::new(end),
                                down: RefCell::new(Some(replacement.location)),
                                waiting_group: Cell::new(false),
                                child: None,
                            }));
                    } else {
                        self.clear_cell_pending(&pending);
                    }
                    self.schedule()?;
                    return Ok(true); // 本回合只归还旧来源；新Down仍在下一原量子开始，预算不叠加。
                }
                if request.input != *state {
                    self.clear_cell_pending(&pending);
                    return Ok(false);
                }
                if destination.borrow().is_none() {
                    match producer.activate_cell(self, *position, *column, *next_row)? {
                        semantics::CellNavigationStep::Occupied => return Ok(false),
                        semantics::CellNavigationStep::Pending => {
                            self.schedule()?;
                            return Ok(true);
                        }
                        semantics::CellNavigationStep::Ready(ready) => {
                            *destination.borrow_mut() = Some(ready)
                        }
                        _ => return Err(ViewError::Revoked.into()),
                    }
                }
                if target.borrow().is_none() {
                    let document = self
                        .document
                        .borrow()
                        .as_ref()
                        .expect("新格来源仍归同一Doc")
                        .clone();
                    let start = destination
                        .borrow()
                        .as_ref()
                        .expect("真实格已Ready")
                        .source
                        .canonical_range_for(&document.borrow())?
                        .start;
                    let position = pointer.as_ref().map_or(start, |pointer| pointer.head);
                    let source = producer.target_source_authority(request, self.serial.get())?;
                    let desired = self.admit_source_target(
                        request,
                        source,
                        NSRange::new(position, 0),
                        &|| {
                            self.pending_cell
                                .borrow()
                                .as_ref()
                                .is_some_and(|actual| Rc::ptr_eq(actual, &pending))
                        },
                    )?;
                    self.install_source_target(desired.clone())?;
                    *target.borrow_mut() = Some(desired);
                }
                Ok(false) // 实际目标沿同一个Producer继续行/字形定位，Ready之后才挂新输入。
            }
            PendingCell::Closing { drained, .. } => {
                if drained.get() {
                    return Ok(false);
                }
                if producer.drain_cell_navigation(self)? {
                    drained.set(true);
                } else {
                    self.schedule()?;
                }
                Ok(true) // 此轮只交回原任务，不再启动下一格或让普通布局抢同一来源槽。
            }
            PendingCell::Pointer { .. } | PendingCell::ParentPointer { .. } => Ok(false),
        }
    }

    pub(super) fn accept_cell_action(
        &self,
        child: &NativeMarkdownPagedBody,
        action: NativeMarkdownPagedCellAction,
        permit: NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<(), ViewError> {
        let active = self
            .active_cell
            .borrow()
            .clone()
            .ok_or(ViewError::Revoked)?;
        if !Weak::ptr_eq(&active.body.weak, &child.weak) {
            return Err(ViewError::Revoked);
        }
        child
            .input
            .check_permit(&permit, &|epoch| child.current(epoch))?;
        if action == NativeMarkdownPagedCellAction::Cancel {
            child.cancel_cell(permit)?;
            // 原草稿确已取消才替换业务意图；取消若因 Busy/撤权拒绝，原导航和焦点继续保留。
            // RootLeaf 仍持同一实际在途请求，Closing 只排空，不再接纳下一格或追加行。
            *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::Closing {
                owner: Rc::downgrade(&active),
                drained: Cell::new(false),
            }));
            return self.schedule().map_err(cell_view_error); // 真正input栈退出后才移除Native子图，不把取消后的Busy当未取消。
        }
        if self.pending_cell.borrow().is_some() || child.has_pending_input() {
            return Err(ViewError::Pending);
        }
        *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::Navigation {
            owner: Rc::downgrade(&active),
            permit: RefCell::new(Some(permit)),
            action,
            result: RefCell::new(None),
        }));
        self.schedule().map_err(cell_view_error)
    }

    fn finish_cell_navigation(
        &self,
        pending: &Rc<PendingCell>,
        active: &Rc<ActiveCell>,
        action: NativeMarkdownPagedCellAction,
        step: semantics::CellNavigationStep,
    ) -> Result<()> {
        let (position, column, terminal, next_row) = match step {
            semantics::CellNavigationStep::Ready(destination) => {
                let document = self.document.borrow().as_ref().expect("原导航Doc").clone();
                let (_, _, column) = destination.source.coordinate_for(&document.borrow())?;
                (destination.row.source_range().start, column, None, false)
            }
            semantics::CellNavigationStep::End {
                terminal,
                authority,
            } if action == NativeMarkdownPagedCellAction::Next => (
                active.row.source_range().start,
                0,
                Some((terminal, authority)),
                true,
            ),
            semantics::CellNavigationStep::End { .. } | semantics::CellNavigationStep::Boundary => {
                self.clear_cell_pending(pending);
                return Ok(()); // Enter没有下一行、首格反向均保留原草稿和本地历史。
            }
            _ => return Err(ViewError::Revoked.into()),
        };
        let PendingCell::Navigation { permit, .. } = &**pending else {
            unreachable!("原导航结果只交原导航槽");
        };
        let permit = permit.borrow_mut().take().expect("原输入票只消费一次");
        active
            .body
            .input
            .check_permit(&permit, &|epoch| active.body.current(epoch))
            .map_err(ViewError::Input)?;
        let append = terminal
            .as_ref()
            .map(|(terminal, authority)| (terminal, *authority));
        let committed =
            match self.commit_owned_cell(active, append, Some(&permit), None, None, None) {
                Ok(committed) => committed,
                Err(cause) => {
                    self.clear_cell_pending(pending);
                    crate::native_surface::present_native_markdown_error(&cause.to_string());
                    return Ok(()); // 草稿发布前拒绝不停止整个Body；原草稿和焦点仍归同owner。
                }
            };
        let position = map_position(position, &committed.outcome)?;
        let state = self.input.state().map_err(ViewError::Input)?;
        *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::Reopen {
            state,
            position,
            column,
            next_row,
            destination: RefCell::new(None),
            target: RefCell::new(None),
            pointer: None,
        }));
        self.focus_cell_parent()?;
        self.schedule()
    }

    fn focus_cell_parent(&self) -> Result<()> {
        let view = self.view()?;
        if let Some(window) = view.window()
            && !window.makeFirstResponder(Some(&**view))
        {
            return Err(ViewError::Revoked.into());
        }
        Ok(())
    }

    /// 子图本次真实越界事件只同步查询父图命中；不转发 mouse 方法，也不保存或重放事件。
    /// 源读取和 Native 转换之间不持 Doc/槽借用，前后仍核同 child 输入票与父 Scene 原几何。
    fn cell_drag_target(
        &self,
        active: &Rc<ActiveCell>,
        permit: &NativeMarkdownPagedInputPermit,
        pointer: &NativeMarkdownPagedPointerLocation,
    ) -> std::result::Result<
        crate::native_surface::paged_text_view::NativeMarkdownPagedCellExtension,
        ViewError,
    > {
        let parent = self.view().map_err(cell_view_error)?;
        let request = parent.capture_scene_request()?;
        let scene = self.ready_scene(&request)?;
        let current = || self.scene_current(scene.key()) && self.owns_cell(active);
        scene.validate_ready(&current)?;
        let point = pointer.window_point;
        scene.validate_ready(&current)?;
        active.body.input.check_permit(permit, &|epoch| {
            active.body.current(epoch) && self.owns_cell(active)
        })?;
        let local = parent.convertPoint_fromView(point, None);
        scene.validate_ready(&current)?;
        if !local.x.is_finite() || !local.y.is_finite() {
            return Err(ViewError::InvalidGeometry);
        }
        let actual = parent.capture_scene_request();
        scene.validate_ready(&current)?; // Native/current 撤权不能覆盖原源的真实首错。
        if !actual?.same(&request) {
            return Err(ViewError::Revoked);
        }
        active.body.input.check_permit(permit, &|epoch| {
            active.body.current(epoch) && self.owns_cell(active)
        })?;
        use crate::native_surface::paged_text_view::{
            NativeMarkdownPagedCellDragAnchor as Anchor,
            NativeMarkdownPagedCellExtension as Extension,
            NativeMarkdownPagedWordSelection as WordSelection,
        };
        let target = match pointer.anchor.ok_or(ViewError::Pending)? {
            Anchor::Caret { .. } => {
                let (selection, head_affinity) =
                    scene.mouse_selection(local, pointer.clicks, None, false)?;
                Extension::Selection {
                    selection,
                    head_affinity,
                    intent: pointer.intent.clone(),
                }
            }
            Anchor::Word(anchor) => {
                // 真实Grid命中同时交付本行/列查询意图；裸canonical位置不能授予另一格的显示域。
                let (head_utf16, backward_bias, cell) = scene.word_hit(local)?;
                Extension::Word {
                    selection: WordSelection {
                        head_utf16,
                        backward_bias,
                        anchor: Some(anchor),
                        cell,
                    },
                    intent: pointer.intent.clone(),
                }
            }
        };
        scene.validate_ready(&current)?;
        Ok(target) // Scene/Reader强引用在返回时归还，随后才允许提交同一次canonical。
    }

    /// 真实Word结果才能把有限终点换到after；普通输入/新Down不能借当前状态给旧request续权。
    pub(super) fn finish_pointer_word_group(
        &self,
        before: &NativeMarkdownPagedSceneRequest,
        after: &NativeMarkdownPagedSceneRequest,
        anchor: crate::native_surface::paged_text_view::NativeMarkdownPagedWordAnchor,
        intent: &Weak<()>,
        commit: Option<&crate::native_surface::paged_input::NativeMarkdownPagedInputCommit>,
    ) -> Result<()> {
        if !before.target.same(&after.target)
            || !Weak::ptr_eq(&before.document, &after.document)
            || before.input.generation != after.input.generation
        {
            return Err(ViewError::Revoked.into());
        }
        if let Some(commit) = commit {
            commit
                .check_current(&|epoch| self.current(epoch))
                .map_err(ViewError::Input)?;
            if commit.state != after.input || commit.invalidation.characters_changed {
                return Err(ViewError::Revoked.into());
            }
        } else if !before.same(after) {
            return Err(ViewError::Revoked.into());
        }
        let pending = self.pending_cell.borrow().clone();
        if let Some(pending) = pending
            && let PendingCell::ParentPointer {
                request,
                pointer,
                waiting_group,
                child: None,
                ..
            } = &*pending
            && request.borrow().same(before)
            && Weak::ptr_eq(&pointer.borrow().intent, intent)
        {
            let finished = {
                let mut pointer = pointer.borrow_mut();
                pointer.anchor = Some(
                    crate::native_surface::paged_text_view::NativeMarkdownPagedCellDragAnchor::Word(
                        anchor,
                    ),
                );
                !pointer.dragging && !pointer.mouse_up
            };
            *request.borrow_mut() = after.clone();
            waiting_group.set(false);
            if finished {
                self.clear_cell_pending(&pending);
            } else {
                self.schedule()?;
            }
        }
        if self.input.is_cell()
            && let Some(parent) = self.cell_parent.borrow().upgrade()
        {
            parent.accept_child_word_group(self, before, after, anchor, intent)?;
        }
        Ok(())
    }

    /// 父Down只把未改raw的真实组映到canonical；child自身Down保持局部域等待同趟sanitizer。
    /// 已接受Up的终点由原槽证明，不能因View的Down Rc已清除就丢掉刚完成的首词组。
    fn accept_child_word_group(
        &self,
        child: &Self,
        before: &NativeMarkdownPagedSceneRequest,
        after: &NativeMarkdownPagedSceneRequest,
        anchor: crate::native_surface::paged_text_view::NativeMarkdownPagedWordAnchor,
        intent: &Weak<()>,
    ) -> Result<()> {
        use crate::native_surface::paged_text_view::{
            NativeMarkdownPagedCellDragAnchor as Drag, NativeMarkdownPagedWordAnchor as Anchor,
        };
        let pending = self.pending_cell.borrow().clone();
        let Some(pending) = pending else {
            return Ok(());
        };
        let active = self.active_cell.borrow().clone();
        let Some(active) = active.filter(|active| Weak::ptr_eq(&active.body.weak, &child.weak))
        else {
            return Ok(());
        };
        if !child.view()?.capture_scene_request()?.same(after) || !self.owns_cell(&active) {
            return Err(ViewError::Revoked.into());
        }
        match &*pending {
            PendingCell::Pointer {
                owner,
                pointer,
                applied,
                endpoint,
            } if applied.get()
                && Weak::ptr_eq(&pointer.intent, intent)
                && owner
                    .upgrade()
                    .is_some_and(|owner| Rc::ptr_eq(&owner, &active)) =>
            {
                if after.input.generation != 0 {
                    return Err(ViewError::Pending.into());
                }
                let range = {
                    let document = self
                        .document
                        .borrow()
                        .as_ref()
                        .expect("原raw仍属于父Doc")
                        .clone();
                    active
                        .source
                        .canonical_range_for(&document.borrow())?
                        .clone()
                };
                let position = |position: usize| {
                    if position > range.end - range.start {
                        Err(ViewError::InvalidRange)
                    } else {
                        Ok(range.start + position)
                    }
                };
                let canonical = match anchor {
                    Anchor::Selection(anchor) => Anchor::Selection(position(anchor)?),
                    Anchor::Group {
                        from_utf16,
                        to_utf16,
                        origin_utf16,
                    } => Anchor::Group {
                        from_utf16: position(from_utf16)?,
                        to_utf16: position(to_utf16)?,
                        origin_utf16: position(origin_utf16)?,
                    },
                    Anchor::Word { .. } => unreachable!("只接原UBA实际完成的词组"),
                };
                let view = self.view()?;
                let request = view.capture_scene_request()?;
                if request.input != active.parent_request.input {
                    return Err(ViewError::Revoked.into());
                }
                if let Anchor::Group {
                    from_utf16,
                    to_utf16,
                    ..
                } = canonical
                {
                    view.accept_pointer_word_group(&request, from_utf16..to_utf16, intent)?;
                }
                let endpoint = endpoint.borrow_mut().take();
                if let Some(mut endpoint) = endpoint {
                    endpoint.anchor = Some(Drag::Word(canonical));
                    *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::ParentPointer {
                        request: RefCell::new(request),
                        pointer: RefCell::new(endpoint),
                        down: RefCell::new(None),
                        waiting_group: Cell::new(false),
                        child: None,
                    }));
                    self.schedule()?;
                } else {
                    self.clear_cell_pending(&pending);
                }
            }
            PendingCell::ParentPointer {
                request,
                pointer,
                waiting_group,
                child: Some(owner),
                ..
            } if owner
                .upgrade()
                .is_some_and(|owner| Rc::ptr_eq(&owner, &active))
                && request.borrow().same(before)
                && Weak::ptr_eq(&pointer.borrow().intent, intent) =>
            {
                *request.borrow_mut() = after.clone();
                pointer.borrow_mut().anchor = Some(Drag::Word(anchor));
                waiting_group.set(false);
                self.schedule()?;
            }
            _ => {} // 原Up后新Down已撤原槽；迟到结果不能重新创建终点或拖动寿命。
        }
        Ok(())
    }

    /// 子格越界仍保持原Down的Caret/Word粒度；首词组必须已经真实完成，不能用裸种子替代。
    /// 同一草稿只发布一次；清洗收据同时映射选择、首词组和首击点，父图不重扩旧词。
    pub(super) fn accept_cell_drag(
        &self,
        child: &Self,
        request: &NativeMarkdownPagedSceneRequest,
        pointer: NativeMarkdownPagedPointerLocation,
        permit: NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        let active = self
            .active_cell
            .borrow()
            .clone()
            .filter(|active| Weak::ptr_eq(&active.body.weak, &child.weak))
            .ok_or(ViewError::Revoked)?;
        child.input.check_permit(&permit, &|epoch| {
            child.current(epoch) && self.owns_cell(&active)
        })?;
        let view = child.view().map_err(cell_view_error)?;
        if !view.cell_pointer_is_current(&pointer.intent)
            || !view.capture_scene_request()?.same(request)
            || !view.cell_pointer_is_current(&pointer.intent)
        {
            return Err(ViewError::Revoked);
        }
        let pending = self.pending_cell.borrow().clone();
        if let Some(pending) = pending {
            if let PendingCell::ParentPointer {
                request: original,
                pointer: latest,
                child: Some(owner),
                ..
            } = &*pending
                && owner
                    .upgrade()
                    .is_some_and(|owner| Rc::ptr_eq(&owner, &active))
                && original.borrow().same(request)
                && Weak::ptr_eq(&latest.borrow().intent, &pointer.intent)
                && !latest.borrow().mouse_up
            {
                *latest.borrow_mut() = pointer;
                self.schedule().map_err(cell_view_error)?;
                return Ok(true);
            }
            return Err(ViewError::Pending);
        }
        let waiting = matches!(
            pointer.anchor,
            None | Some(
                crate::native_surface::paged_text_view::NativeMarkdownPagedCellDragAnchor::Word(
                    crate::native_surface::paged_text_view::NativeMarkdownPagedWordAnchor::Word { .. }
                )
            )
        );
        *self.pending_cell.borrow_mut() = Some(Rc::new(PendingCell::ParentPointer {
            request: RefCell::new(request.clone()),
            pointer: RefCell::new(pointer),
            down: RefCell::new(None),
            waiting_group: Cell::new(waiting),
            child: Some(Rc::downgrade(&active)),
        }));
        self.schedule().map_err(cell_view_error)?;
        Ok(true) // 只接纳原终点；真实Group/父Scene完成后由同槽消费，不保留或重播event。
    }

    fn apply_cell_drag(
        &self,
        child: &Self,
        request: &NativeMarkdownPagedSceneRequest,
        pointer: NativeMarkdownPagedPointerLocation,
        permit: NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        use crate::native_surface::paged_text_view::{
            NativeMarkdownPagedCellDragAnchor as Drag,
            NativeMarkdownPagedCellExtension as Extension, NativeMarkdownPagedWordAnchor as Anchor,
        };
        let active = self
            .active_cell
            .borrow()
            .clone()
            .ok_or(ViewError::Revoked)?;
        if !Weak::ptr_eq(&active.body.weak, &child.weak) {
            return Err(ViewError::Revoked);
        }
        child
            .input
            .check_permit(&permit, &|epoch| child.current(epoch))?;
        if !request.target.same(&child.input.target_domain()?)
            || (!pointer.mouse_up
                && !child
                    .view()
                    .map_err(cell_view_error)?
                    .cell_pointer_is_current(&pointer.intent))
        {
            return Err(ViewError::Revoked);
        }
        let state = child.input.state()?;
        if !state.selection_is_directed {
            return Err(ViewError::Revoked);
        }
        let pending = self.pending_cell.borrow().clone();
        let accepted = pending.as_ref().is_some_and(|pending| {
            matches!(&**pending,
            PendingCell::ParentPointer { pointer: actual, child: Some(owner), .. }
                if Weak::ptr_eq(&actual.borrow().intent, &pointer.intent)
                    && owner.upgrade().is_some_and(|owner| Rc::ptr_eq(&owner, &active)))
        });
        if !accepted || child.has_pending_input() {
            return Err(ViewError::Pending); // 不撤尚在完成的首组，也不能把未提交状态当跨格成功。
        }
        if pointer.mouse_up && !pointer.dragging {
            // 原 Down 与相同物理点的 Up 没有扩选动作；真实首组完成后只结束手势。
            // 活动 raw 的来源仍先核同 Doc/root，不能因省略新 hit 而掩盖实际 mmap/FD 首错。
            let original = self
                .document
                .borrow()
                .as_ref()
                .expect("原格 Up 仍属于父文档")
                .clone();
            active
                .source
                .canonical_range_for(&original.borrow())
                .map_err(|cause| cell_view_error(cause.into()))?;
            return Ok(true); // 不提交草稿、不把显隐后移动的字形当成用户又拖到了另一位置。
        }
        let drag_anchor = pointer.anchor.ok_or(ViewError::Pending)?;
        let word_anchor = match drag_anchor {
            Drag::Caret { anchor_utf16 } => {
                if state.selection.anchor_utf16 != anchor_utf16 {
                    return Err(ViewError::Revoked);
                }
                None // Caret真实有向anchor已由输入票与同趟selection收据负责映射。
            }
            Drag::Word(Anchor::Word { .. }) => return Err(ViewError::Pending),
            Drag::Word(Anchor::Selection(position)) => Some(MarkdownCellDraftWordAnchor {
                range: MarkdownTextSelection::caret(position),
                origin_utf16: position,
            }),
            Drag::Word(Anchor::Group {
                from_utf16,
                to_utf16,
                origin_utf16,
            }) => Some(MarkdownCellDraftWordAnchor {
                range: MarkdownTextSelection {
                    anchor_utf16: from_utf16,
                    head_utf16: to_utf16,
                },
                origin_utf16,
            }),
        };
        let extension = self.cell_drag_target(&active, &permit, &pointer)?;
        let head = match &extension {
            Extension::Selection { selection, .. } => selection.head_utf16,
            Extension::Word { selection, .. } => selection.head_utf16,
        };
        let range = {
            let document = self
                .document
                .borrow()
                .as_ref()
                .expect("原格拖选正文")
                .clone();
            let document = document.borrow();
            active
                .source
                .canonical_range_for(&document)
                .map_err(|cause| cell_view_error(cause.into()))?
                .clone()
        };
        if range.contains(&head) {
            return Err(ViewError::Pending); // 父旧显示域不能定位同raw内已经修改过的子Source。
        }
        let committed = self
            .commit_owned_cell(
                &active,
                None,
                Some(&permit),
                word_anchor,
                Some((&pointer.intent, drag_anchor)),
                None,
            )
            .map_err(cell_view_error)?;
        // 原Native鼠标栈尚占child，拆图已排在其归还之后；这里仅消费同一次真实发布的结果。
        // 后继输入撤权不恢复或重放旧草稿，任何失败仍表达已提交后的后果。
        let effects = (|| -> std::result::Result<bool, ViewError> {
            let current = |epoch| {
                self.current(epoch)
                    && match &committed.outcome {
                        NativeMarkdownPagedInputOutcome::Committed(commit) => {
                            commit.check_current(&|epoch| self.current(epoch)).is_ok()
                        }
                        NativeMarkdownPagedInputOutcome::Unchanged(state) => {
                            self.input.state().is_ok_and(|actual| actual == *state)
                        }
                    }
            };
            let permit = self.input.capture(self.configuration.epoch, &current)?;
            match extension {
                Extension::Selection {
                    selection,
                    head_affinity,
                    ..
                } => {
                    let head = map_position(selection.head_utf16, &committed.outcome)
                        .map_err(cell_view_error)?;
                    let outcome = self.input.set_selection_intent(
                        &permit,
                        MarkdownTextSelection {
                            anchor_utf16: committed.receipt.selection.anchor_utf16,
                            head_utf16: head,
                        },
                        Some(head_affinity.is_upstream()),
                        None,
                        &current,
                    )?;
                    if let NativeMarkdownPagedInputOutcome::Committed(commit) = outcome {
                        let view = self.view().map_err(cell_view_error)?;
                        view.finish_word_delete_commit(&commit)?;
                        view.adopt_cell_pointer_selection(&commit, &pointer.intent, None)?;
                    }
                }
                Extension::Word {
                    mut selection,
                    intent,
                } => {
                    selection.head_utf16 = map_position(selection.head_utf16, &committed.outcome)
                        .map_err(cell_view_error)?;
                    selection.cell = map_cell_word_target(selection.cell, &committed.outcome)
                        .map_err(cell_view_error)?;
                    let mapped = committed
                        .receipt
                        .word_anchor
                        .expect("原Word三点已在发布前真实映射");
                    selection.anchor = Some(match drag_anchor {
                        Drag::Word(Anchor::Selection(_)) => {
                            Anchor::Selection(mapped.range.anchor_utf16)
                        }
                        Drag::Word(Anchor::Group { .. }) => Anchor::Group {
                            from_utf16: mapped.range.anchor_utf16,
                            to_utf16: mapped.range.head_utf16,
                            origin_utf16: mapped.origin_utf16,
                        },
                        _ => unreachable!("只有已完成的原Word入口产生Word目标"),
                    });
                    let view = self.view().map_err(cell_view_error)?;
                    let request = view.capture_scene_request()?;
                    self.select_word(&request, selection, Some(intent), permit)?;
                }
            }
            Ok(true)
        })();
        effects.map_err(|cause| ViewError::CommittedEffect(Box::new(cause)))
    }

    /// 原指针跨出活动 raw 域后先提交同一草稿，再沿真正源变更恢复原有向选择。
    /// 替换内部只消费 Core 的实际端点映射；外部锚点才允许使用这次真实 delta。
    pub(super) fn extend_cell_selection(
        &self,
        request: &NativeMarkdownPagedSceneRequest,
        extension: crate::native_surface::paged_text_view::NativeMarkdownPagedCellExtension,
        permit: NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<bool, ViewError> {
        use crate::native_surface::paged_text_view::{
            NativeMarkdownPagedCellExtension as Extension, NativeMarkdownPagedWordAnchor as Anchor,
        };
        let active = self.active_cell.borrow().clone();
        let Some(active) = active else {
            return Ok(false);
        };
        self.input
            .check_permit(&permit, &|epoch| self.current(epoch))?;
        if self.input.state()? != request.input || active.body.has_pending_input() {
            return Err(ViewError::Pending);
        }
        let document = self.document.borrow().as_ref().expect("原扩选Doc").clone();
        let range = active
            .source
            .canonical_range_for(&document.borrow())
            .map_err(|cause| cell_view_error(cause.into()))?
            .clone();
        // 原首组在活动格内时，必须把真实两端交同一次清洗；当前选区可能已含多个词，不能代替首组。
        // 未Ready的种子明确等待，原组横跨未授权raw边界也拒绝，均不提前发布草稿。
        let word_anchor = match &extension {
            Extension::Word { selection, .. } => match selection.anchor {
                Some(Anchor::Word { head_utf16, .. }) if range.contains(&head_utf16) => {
                    return Err(ViewError::Pending);
                }
                Some(Anchor::Selection(position))
                    if range.start <= position && position <= range.end =>
                {
                    Some(MarkdownCellDraftWordAnchor {
                        range: MarkdownTextSelection::caret(position - range.start),
                        origin_utf16: position - range.start,
                    })
                }
                Some(Anchor::Group {
                    from_utf16,
                    to_utf16,
                    origin_utf16,
                }) if range.start <= from_utf16 && to_utf16 <= range.end => {
                    Some(MarkdownCellDraftWordAnchor {
                        range: MarkdownTextSelection {
                            anchor_utf16: from_utf16 - range.start,
                            head_utf16: to_utf16 - range.start,
                        },
                        origin_utf16: origin_utf16 - range.start,
                    })
                }
                Some(Anchor::Group {
                    from_utf16,
                    to_utf16,
                    ..
                }) if from_utf16 < range.end && range.start < to_utf16 => {
                    return Err(ViewError::InvalidRange);
                }
                _ => None, // 格外首组使用这次真实替换delta；没有借入活动raw内部的位置。
            },
            Extension::Selection { .. } => None,
        };
        let intent = match &extension {
            Extension::Selection { intent, .. } | Extension::Word { intent, .. } => intent,
        };
        let original_pointer = self.pending_cell.borrow().clone().filter(|pending| {
            matches!(&**pending, PendingCell::ParentPointer {
                request: original,
                pointer,
                child: None,
                ..
            } if original.borrow().same(request) && Weak::ptr_eq(&pointer.borrow().intent, intent))
        });
        if original_pointer.is_none() {
            self.pending_cell.borrow_mut().take();
        } // 真实父终点保住唯一槽直到发布；新 Down 即使 Input 未变，也会撤掉这张具体 scope。
        let result = self.commit_owned_cell(
            &active,
            None,
            None,
            word_anchor,
            None,
            original_pointer.as_ref(),
        );
        if let Some(original) = &original_pointer {
            self.clear_cell_pending(original);
        } // 只取走这个已消费的原终点，Native 重入安装的新槽不能被旧出口清除。
        let committed = result.map_err(cell_view_error)?;
        let view = self.view().map_err(cell_view_error)?;
        let current = |epoch| {
            self.current(epoch)
                && match &committed.outcome {
                    NativeMarkdownPagedInputOutcome::Committed(commit) => {
                        commit.check_current(&|epoch| self.current(epoch)).is_ok()
                    }
                    NativeMarkdownPagedInputOutcome::Unchanged(state) => {
                        self.input.state().is_ok_and(|actual| actual == *state)
                    }
                }
        };
        let permit = self.input.capture(self.configuration.epoch, &current)?;
        match extension {
            Extension::Selection {
                selection,
                head_affinity,
                intent,
            } => {
                let head = map_position(selection.head_utf16, &committed.outcome)
                    .map_err(cell_view_error)?;
                let anchor = if selection.anchor_utf16 == selection.head_utf16 {
                    head // 原新Down的空caret不继承上一格草稿的旧anchor。
                } else if range.start <= selection.anchor_utf16
                    && selection.anchor_utf16 <= range.end
                {
                    committed.receipt.selection.anchor_utf16
                } else {
                    map_position(selection.anchor_utf16, &committed.outcome)
                        .map_err(cell_view_error)?
                };
                let outcome = self.input.set_selection_intent(
                    &permit,
                    MarkdownTextSelection {
                        anchor_utf16: anchor,
                        head_utf16: head,
                    },
                    Some(head_affinity.is_upstream()),
                    None,
                    &current,
                )?;
                if let NativeMarkdownPagedInputOutcome::Committed(commit) = outcome {
                    view.finish_word_delete_commit(&commit)?;
                    view.adopt_cell_pointer_selection(&commit, &intent, None)?;
                }
            }
            Extension::Word {
                mut selection,
                intent,
            } => {
                selection.head_utf16 = map_position(selection.head_utf16, &committed.outcome)
                    .map_err(cell_view_error)?;
                selection.cell = map_cell_word_target(selection.cell, &committed.outcome)
                    .map_err(cell_view_error)?;
                selection.anchor = selection
                    .anchor
                    .map(|anchor| {
                        if let Some(mapped) = committed.receipt.word_anchor {
                            return Ok(match anchor {
                                Anchor::Selection(_) => {
                                    Anchor::Selection(mapped.range.anchor_utf16)
                                }
                                Anchor::Group { .. } => Anchor::Group {
                                    from_utf16: mapped.range.anchor_utf16,
                                    to_utf16: mapped.range.head_utf16,
                                    origin_utf16: mapped.origin_utf16,
                                },
                                Anchor::Word { .. } => unreachable!("未Ready种子在提交前已拒绝"),
                            });
                        }
                        map_word_anchor(anchor, &committed.outcome)
                    })
                    .transpose()
                    .map_err(cell_view_error)?;
                let request = view.capture_scene_request()?;
                self.select_word(&request, selection, Some(intent), permit)?;
                // 后继目标组仍由同一原手势交付；实际Ready之前不制造Commit或重建已经结束的Down。
            }
        }
        self.focus_cell_parent().map_err(cell_view_error)?;
        Ok(true)
    }

    /// 有限编辑框跟随原 virtual viewport 的实际平移；没有新测量就只移动已知前缀框，
    /// 不能因行离屏而丢草稿或把别行当前 frame 当作原格。
    fn reposition_cell(&self) -> Result<()> {
        let active = self.active_cell.borrow().clone();
        let Some(active) = active else {
            return Ok(());
        };
        let request = self.view()?.capture_scene_request()?;
        let before = &active.parent_request;
        let delta = NSPoint::new(
            before.virtual_viewport.document_origin.x - request.virtual_viewport.document_origin.x,
            request.viewport.size.height - before.viewport.size.height
                + request.virtual_viewport.document_origin.y
                - before.virtual_viewport.document_origin.y,
        );
        let mut frame = active.scroll.frame();
        frame.origin = NSPoint::new(
            active.content_frame.origin.x + delta.x,
            active.content_frame.origin.y + delta.y,
        );
        if active.scroll.frame() != frame {
            active.scroll.setFrame(frame);
        }
        Ok(())
    }
}

pub(super) fn cell_view_error(cause: NativeMarkdownPagedBodyError) -> ViewError {
    match cause {
        NativeMarkdownPagedBodyError::View(cause) => cause,
        NativeMarkdownPagedBodyError::Busy => ViewError::Busy,
        cause => {
            crate::native_surface::present_native_markdown_error(&cause.to_string());
            ViewError::Revoked
        }
    }
}

/// 只映真实替换之外的既有锚点；替换内部必须使用 Core 同趟产生的 receipt.selection，不能猜。
fn map_position(position: usize, outcome: &NativeMarkdownPagedInputOutcome) -> Result<usize> {
    let NativeMarkdownPagedInputOutcome::Committed(commit) = outcome else {
        return Ok(position);
    };
    let Some(range) = &commit.invalidation.replaced_utf16_range else {
        return Ok(position);
    };
    if position <= range.start {
        return Ok(position);
    }
    if position < range.end {
        return Err(ViewError::InvalidRange.into());
    }
    position
        .checked_add_signed(commit.invalidation.change_in_utf16_length)
        .ok_or(ViewError::InvalidRange.into())
}

fn map_pointer(
    mut pointer: ReopenPointer,
    outcome: &NativeMarkdownPagedInputOutcome,
) -> Result<ReopenPointer> {
    pointer.head = map_position(pointer.head, outcome)?;
    pointer.anchor = pointer
        .anchor
        .map(|position| map_position(position, outcome))
        .transpose()?;
    pointer.word_anchor = pointer
        .word_anchor
        .map(|anchor| map_word_anchor(anchor, outcome))
        .transpose()?;
    Ok(pointer)
}

/// 行列仅是这次真实命中的查询意图；跨提交只平移已知行位置，不借旧Source票续权。
/// 新一代Producer仍须按这个位置和列重新取得真实行、raw与display，才能查询目标词组。
fn map_cell_word_target(
    target: Option<crate::native_surface::paged_text_view::NativeMarkdownPagedCellWordTarget>,
    outcome: &NativeMarkdownPagedInputOutcome,
) -> Result<Option<crate::native_surface::paged_text_view::NativeMarkdownPagedCellWordTarget>> {
    target
        .map(|mut target| {
            target.row_start = map_position(target.row_start, outcome)?;
            Ok(target)
        })
        .transpose()
}

fn map_word_anchor(
    anchor: crate::native_surface::paged_text_view::NativeMarkdownPagedWordAnchor,
    outcome: &NativeMarkdownPagedInputOutcome,
) -> Result<crate::native_surface::paged_text_view::NativeMarkdownPagedWordAnchor> {
    use crate::native_surface::paged_text_view::NativeMarkdownPagedWordAnchor as Anchor;
    Ok(match anchor {
        Anchor::Selection(position) => Anchor::Selection(map_position(position, outcome)?),
        Anchor::Group {
            from_utf16,
            to_utf16,
            origin_utf16,
        } => Anchor::Group {
            from_utf16: map_position(from_utf16, outcome)?,
            to_utf16: map_position(to_utf16, outcome)?,
            origin_utf16: map_position(origin_utf16, outcome)?,
        },
        Anchor::Word {
            head_utf16,
            backward_bias,
            cell,
        } => Anchor::Word {
            head_utf16: map_position(head_utf16, outcome)?,
            backward_bias,
            cell: map_cell_word_target(cell, outcome)?,
        },
    })
}

impl NativeMarkdownPagedBody {
    /// 核同一个实际 owner 后移出槽，Native拆图在所有借用之外发生；收据沿原父图合并。
    fn detach_cell(&self, active: &Rc<ActiveCell>) -> Result<Rc<NativeMarkdownPagedCloseReceipt>> {
        if !self.owns_cell(active) {
            return Err(ViewError::Revoked.into());
        }
        let receipt = active.body.begin_close()?;
        self.active_cell.borrow_mut().take();
        active.scroll.removeFromSuperview();
        *self.cell_close.borrow_mut() = Some(receipt.clone());
        let weak = MainThreadBound::new(self.weak.clone(), self.mtm);
        let original = Rc::downgrade(&receipt);
        receipt.when_released(move || {
            let mtm = MainThreadMarker::new().expect("格像素归还仍在原主队列");
            let Some(parent) = weak.get(mtm).upgrade() else {
                return;
            };
            let same = parent
                .cell_close
                .borrow()
                .as_ref()
                .is_some_and(|receipt| Weak::ptr_eq(&original, &Rc::downgrade(receipt)));
            if same {
                let receipt = parent.cell_close.borrow_mut().take().expect("已核原关闭票");
                if receipt.verify_released().is_err() {
                    *parent.cell_close.borrow_mut() = Some(receipt);
                    parent.fail(NativeMarkdownPagedBodyError::ResourceReleaseFailed);
                } else if parent.current_body()
                    && let Err(cause) = parent.schedule()
                {
                    parent.fail(cause);
                }
            }
        });
        self.notify_local_cell_input();
        Ok(receipt)
    }

    /// 真实鼠标回调尚占 child Native 栈时，Core 可以先 seal，物理拆图必须等该栈归还。
    /// 弱任务在任何提交通知排布局之前登记；只处理这一实际 owner，不占业务指针续体槽。
    fn queue_finished_cell_detach(&self, active: &Rc<ActiveCell>) {
        let original = MainThreadBound::new((self.weak.clone(), Rc::downgrade(active)), self.mtm);
        super::super::enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("格拆图仍归原主队列");
            let (parent, active) = original.get(mtm);
            let (Some(parent), Some(active)) = (parent.upgrade(), active.upgrade()) else {
                return;
            };
            if !parent.current_body() || !parent.owns_cell(&active) {
                return; // 正式关闭已接管同图时，不替后继格重复 remove 或领取释放收据。
            }
            let result = parent
                .detach_cell(&active)
                .and_then(|_| parent.focus_cell_parent());
            if let Err(cause) = result {
                parent.fail(cause); // 这是实际清理失败，不把任务排队或 Core seal 当作资源已归还。
            }
        });
    }

    /// 保存/借出/模式前置只接同一实际草稿；尚有选择或导航时明确拒绝，不能把 Pending 当保存成功。
    /// 成功结果包含原 canonical receipt，外部命令须核它，不能捕获较新的 after 替旧动作补权。
    pub(in crate::native_surface) fn commit_active_cell(
        &self,
    ) -> Result<Option<NativeMarkdownPagedCellCommit>> {
        if self.preparing.get() || self.pending_cell.borrow().is_some() {
            return Err(NativeMarkdownPagedBodyError::Busy);
        }
        let Some(active) = self.active_cell.borrow().clone() else {
            return Ok(None);
        };
        active.body.preflight_release()?;
        if active.body.has_pending_input() {
            return Err(NativeMarkdownPagedBodyError::Busy);
        }
        active.body.view()?.commit_composition_before_release()?;
        self.commit_owned_cell(&active, None, None, None, None, None)
            .map(Some)
    }

    /// 同一 prepared 先完成，再归还父子全部读图，最后一次 publish。退休会改变显示 serial，
    /// current 仍核原父子 Input 与唯一槽归属，不拿已经退休的 Scene nonce 当编辑许可。
    fn commit_owned_cell(
        &self,
        active: &Rc<ActiveCell>,
        append: Option<(
            &omni_markdown_core::markdown_document_buffer::VerifiedMarkdownTableRowSource,
            omni_markdown_core::MarkdownViewportSourceAuthority,
        )>,
        original_cell: Option<&NativeMarkdownPagedInputPermit>,
        word_anchor: Option<MarkdownCellDraftWordAnchor>,
        transfer: Option<(
            &Weak<()>,
            crate::native_surface::paged_text_view::NativeMarkdownPagedCellDragAnchor,
        )>,
        parent_pointer: Option<&Rc<PendingCell>>,
    ) -> Result<NativeMarkdownPagedCellCommit> {
        if self.preparing.get() || active.body.preparing.get() || !self.owns_cell(active) {
            return Err(NativeMarkdownPagedBodyError::Busy);
        }
        let pointer_current = || {
            // 原父 Up 已结束可拖寿命，却仍有这一个已接纳终点。实际 publish 紧前核同槽，
            // 不能从 Weak 是否仍存活或相同 InputState 推断新 Down 尚未撤销它。
            parent_pointer.is_none_or(|original| {
                self.pending_cell
                    .borrow()
                    .as_ref()
                    .is_some_and(|actual| Rc::ptr_eq(actual, original))
            }) && transfer.is_none_or(|(intent, _)| {
            active.body.view().is_ok_and(|view| view.cell_pointer_is_current(intent))
                || self.pending_cell.borrow().as_ref().is_some_and(|pending| matches!(&**pending,
                    PendingCell::ParentPointer { pointer, child: Some(owner), .. }
                        if pointer.borrow().mouse_up && Weak::ptr_eq(&pointer.borrow().intent, intent)
                            && owner.upgrade().is_some_and(|owner| Rc::ptr_eq(&owner, active))))
        })
        }; // 真实Up终点与仍可拖动是不同事实；不可逆publish前仍须原槽未被新Down取消。
        let body_current =
            |epoch| self.current(epoch) && self.owns_cell(active) && pointer_current();
        let cell_current =
            |epoch| active.body.current(epoch) && self.owns_cell(active) && pointer_current();
        let body_permit = self
            .input
            .capture(self.configuration.epoch, &body_current)
            .map_err(ViewError::Input)?;
        let cell_permit = active
            .body
            .input
            .capture(active.body.configuration.epoch, &cell_current)
            .map_err(ViewError::Input)?;
        let prepared = active
            .body
            .input
            .prepare_cell_commit(
                original_cell.unwrap_or(&cell_permit),
                append,
                word_anchor,
                &cell_current,
            )
            .map_err(ViewError::Input)?;
        active.body.release_cell_readers()?;
        if let Err(cause) = self.release_cell_readers() {
            active.body.schedule()?;
            self.schedule()?;
            return Err(cause);
        }
        let mut committed = match active.body.input.commit_cell_to_body(
            prepared,
            &self.input,
            &body_permit,
            &cell_current,
            &body_current,
        ) {
            Ok(committed) => committed,
            Err(cause) => {
                // Core拒绝没有封闭草稿；同 owner重建原读图，禁止重新prepare或重放提交。
                let restored =
                    producer::merge_preparation_cleanup(active.body.schedule(), self.schedule());
                return producer::merge_preparation_cleanup(
                    Err(NativeMarkdownPagedBodyError::CellCommit(cause)),
                    restored,
                )
                .map(|()| unreachable!("合并的原操作失败不能变为成功"));
            }
        };
        // Core已提交，此后所有失败都是已提交后的Native后果，不能恢复或重播旧草稿。
        if transfer.is_some() {
            self.queue_finished_cell_detach(active);
        } // 原 mouse 栈退出后才拆物理图；任务先于下面通知产生的父布局，仍保真实关闭收据。
        let canonical_invalidation = match &committed.outcome {
            NativeMarkdownPagedInputOutcome::Committed(commit) => Some(commit.invalidation.clone()),
            NativeMarkdownPagedInputOutcome::Unchanged(_) => None,
        };
        let effects = (|| -> std::result::Result<(), ViewError> {
            match &committed.outcome {
                NativeMarkdownPagedInputOutcome::Committed(commit) => {
                    self.view()
                        .map_err(|_| ViewError::Revoked)?
                        .finish_word_delete_commit(commit)?;
                    commit.check_current(&body_current)?;
                }
                NativeMarkdownPagedInputOutcome::Unchanged(state) => {
                    if self.input.state()? != *state {
                        return Err(ViewError::Revoked);
                    }
                }
            }
            let permit = self
                .input
                .capture(self.configuration.epoch, &body_current)?;
            let mut mapped = self.input.set_selection_intent(
                &permit,
                committed.receipt.selection,
                None,
                None,
                &body_current,
            )?;
            if let NativeMarkdownPagedInputOutcome::Committed(commit) = &mapped {
                self.view()
                    .map_err(|_| ViewError::Revoked)?
                    .finish_word_delete_commit(commit)?;
                commit.check_current(&body_current)?;
            }
            if let NativeMarkdownPagedInputOutcome::Committed(commit) = &mut mapped {
                if let Some(original) = &canonical_invalidation {
                    commit.invalidation.replaced_utf16_range =
                        original.replaced_utf16_range.clone();
                    commit.invalidation.change_in_utf16_length = original.change_in_utf16_length;
                    commit.invalidation.characters_changed = original.characters_changed;
                    commit.invalidation.folds_changed |= original.folds_changed;
                }
                committed.outcome = mapped;
            } // Unchanged 不丢原文字提交；新选择 receipt 则保这次完整源变更的真实范围与delta。
            Ok(())
        })();
        let effects = effects.and_then(|()| -> std::result::Result<(), ViewError> {
            if let Some((intent, anchor)) = transfer {
                use crate::native_surface::paged_text_view::{
                    NativeMarkdownPagedCellDragAnchor as Drag,
                    NativeMarkdownPagedWordAnchor as Anchor,
                };
                let word_anchor = match anchor {
                    Drag::Caret { .. } => None,
                    Drag::Word(anchor) => {
                        let mapped = committed
                            .receipt
                            .word_anchor
                            .expect("原首词组已经由同趟sanitizer映射");
                        Some(match anchor {
                            Anchor::Selection(_) => Anchor::Selection(mapped.range.anchor_utf16),
                            Anchor::Group { .. } => Anchor::Group {
                                from_utf16: mapped.range.anchor_utf16,
                                to_utf16: mapped.range.head_utf16,
                                origin_utf16: mapped.origin_utf16,
                            },
                            Anchor::Word { .. } => {
                                unreachable!("未完成的首组不得取得canonical提交票")
                            }
                        })
                    }
                };
                active
                    .body
                    .view()
                    .map_err(cell_view_error)?
                    .transfer_cell_pointer_to(
                        &self.view().map_err(cell_view_error)?,
                        &committed.outcome,
                        intent,
                        word_anchor,
                    )?;
            }
            Ok(())
        });
        let detached = if transfer.is_some() {
            Ok(()) // 仅表示原同步提交已完成；队列实际归还之前，父 owner 仍持这一 child 图。
        } else {
            self.detach_cell(active).map(|_| ())
        };
        if let Err(cause) = effects {
            let original = ViewError::CommittedEffect(Box::new(cause));
            return producer::merge_preparation_cleanup(Err(original.into()), detached)
                .map(|()| unreachable!("合并的原操作失败不能变为成功"));
        }
        detached.map_err(|cause| {
            ViewError::CommittedEffect(Box::new(match cause {
                NativeMarkdownPagedBodyError::View(cause) => cause,
                _ => ViewError::Revoked,
            }))
        })?;
        Ok(committed)
    }
}

impl NativeMarkdownPagedBody {
    /// 归还这一个输入图的全部显示读取；正式格提交必须分别调用父、子两图，
    /// 但保留原 Input/session 和已显示像素，失败仍可恢复同一草稿。
    pub(super) fn release_cell_readers(&self) -> Result<()> {
        let next = self
            .serial
            .get()
            .checked_add(1)
            .ok_or(NativeMarkdownPagedBodyError::CounterExhausted)?;
        self.view()?.release_scene_readers()?; // 正在同步paint时明确Busy，未发生任何草稿封闭。
        self.serial.set(next);
        self.scheduled.set(None);
        self.deferred.set(false);
        drop(self.pending_vertical.borrow_mut().take());
        drop(self.pending_page_scroll.borrow_mut().take());
        drop(self.scene.borrow_mut().take());
        let producer = self.producer.borrow_mut().take();
        let producer_result = producer.map_or(Ok(()), Producer::retire);
        let worker = self.worker.borrow_mut().take();
        let worker_result = worker.map_or(Ok(()), FontWorker::close);
        self.font_notices.borrow_mut().clear();
        // 两类旧结果都实际消费后才允许下一capture/提交，前一步失败也必须join原worker。
        producer::merge_preparation_cleanup(producer_result, worker_result)
    }

    /// 原Cell与既有canonical Input各执原票，只提交一次；这不是正式表格UI挂载或Tab导航。
    pub fn commit_cell_to_body(
        &self,
        body: &Rc<NativeMarkdownPagedInput>,
        body_permit: &NativeMarkdownPagedInputPermit,
        body_current: &(impl Fn(u64) -> bool + ?Sized),
    ) -> Result<NativeMarkdownPagedCellCommit> {
        if self.preparing.get() || !self.input.is_cell() {
            return Err(NativeMarkdownPagedBodyError::Busy);
        }
        let view = self.view()?;
        let request = view.capture_scene_request()?;
        let current = |epoch| {
            self.current(epoch)
                && view
                    .capture_scene_request()
                    .is_ok_and(|actual| actual.same(&request))
        };
        let permit = self
            .input
            .capture(self.configuration.epoch, &current)
            .map_err(ViewError::Input)?;
        // 原票、marked、文档预算或候选存储准备失败在此返回；
        // 原Scene和原帧均不动，不先为提交清空显示，也不因草稿长而截断内容。
        let prepared = self
            .input
            .prepare_cell_commit(&permit, None, None, &current)
            .map_err(ViewError::Input)?;
        self.release_cell_readers()?;
        let result =
            self.input
                .commit_cell_to_body(prepared, body, body_permit, &current, body_current);
        match result {
            Ok(outcome) => {
                drop(self.selection_reveal.borrow_mut().take());
                self.close_finished_cell();
                Ok(outcome) // Core已seal原Cell；只有真实成功后才撤无reader的原caret意图。
            }
            Err(cause) => {
                let error = NativeMarkdownPagedBodyError::CellCommit(cause);
                if let Err(rebuild) = self.schedule() {
                    self.fail(rebuild); // 恢复显示失败保留为原owner首错，不覆盖这次真实提交失败。
                } // 原草稿未改，沿同一Input重建真实Source；不重放已消费的prepared。
                Err(error)
            }
        }
    }

    /// Esc只消费原Cell输入票；原绘图/字体归还后取消同token，Native图在原回调退出后关闭。
    pub(super) fn cancel_cell(
        &self,
        permit: NativeMarkdownPagedInputPermit,
    ) -> std::result::Result<(), ViewError> {
        if self.preparing.get() {
            return Err(ViewError::Busy);
        }
        self.input
            .check_permit(&permit, &|epoch| self.current(epoch))?;
        self.release_cell_readers().map_err(|cause| {
            self.fail(cause);
            ViewError::Revoked
        })?;
        // 真实context/字体析构可重入输入；同一原票必须仍有效，不能取消后继草稿。
        if let Err(cause) = self
            .input
            .check_permit(&permit, &|epoch| self.current(epoch))
        {
            if let Err(rebuild) = self.schedule() {
                self.fail(rebuild); // 析构重入已改变输入时只恢复显示，不重放取消或给旧票续权。
            }
            return Err(cause.into());
        }
        let session = self
            .cell_session
            .borrow()
            .clone()
            .ok_or(ViewError::Closed)?;
        session.close()?;
        drop(self.selection_reveal.borrow_mut().take()); // 同token真实取消以后才撤原意图；拒绝仍保后继显示。
        self.input.close()?;
        self.close_finished_cell();
        Ok(())
    }

    /// 原token已提交或取消后立即撤回调权限；Native拆图等当前输入栈退出，不能再读已归还草稿。
    fn close_finished_cell(&self) {
        self.active.set(false); // 此后旧clip/输入/worker回调均不可重建，实际Native关闭等原栈退出。
        let weak = MainThreadBound::new(self.weak.clone(), self.mtm);
        super::super::enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("单格关闭只在主线程完成");
            if let Some(owner) = weak.get(mtm).upgrade()
                && let Err(cause) = owner.begin_close()
            {
                owner.fail(cause);
            }
        });
    }
}
