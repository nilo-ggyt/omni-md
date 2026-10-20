//! 模式及Find栏布局只保有限原源锚点；位置恢复仍使用唯一Producer及其原Source目标续体。

use super::NativeMarkdownPagedBodyError as Error;
use super::*;
use crate::native_surface::editor::viewport::ModeSourceAnchor;
use crate::native_surface::paged_input::NativeMarkdownPagedInputState;
use crate::native_surface::{
    NativeMarkdownController, find_bar::NativeMarkdownFindBar,
    native_markdown_controller_has_foreground_authority,
    workspace_menu_intent::NativeMarkdownWorkspacePagedEditIntent,
};
use objc2::{ClassType, DefinedClass};
use objc2_foundation::{NSPoint, NSRange, NSRect, NSSize};
use omni_markdown_core::MarkdownPresentationMode;

/// 没有Source/Reader/Scene/Native强引用；目标票也只持弱身份和有限标量。
pub(super) struct PendingModeViewport {
    anchor: ModeSourceAnchor,
    state: NativeMarkdownPagedInputState,
    target: Option<Rc<target::SourceTargetRequest>>,
    controller: Option<Rc<ModeController>>,
    // 只有同 Source/宽度的真实 Scene 已测得首字形后才存在；不是滚动已完成的标志。
    // 模式保有它直到新 Scene 核准原屏内位置，普通整行导航不得覆盖这个实际字形锚。
    measured_glyph: Option<MeasuredModeGlyph>,
}

/// 同一 Scene 的文档上沿和完整字形高度；只保有限实际几何，不保字体、行或 Source。
#[derive(Clone, Copy)]
struct MeasuredModeGlyph {
    document_top: f64,
    height: f64,
}

impl MeasuredModeGlyph {
    /// 模式字号/行高改变时只做完整显示所必需的最近调整；原位置可显示则精确保留。
    /// 超过视口的真实字形只能露出首部，不缩小字形或伪造一个较矮的测量框。
    fn requested_origin(self, screen_y: f64, viewport_height: f64) -> f64 {
        let last_top = (viewport_height - self.height).max(0.0);
        let visible_y = screen_y.clamp(0.0, last_top);
        (self.document_top - visible_y).max(0.0)
    }
}

/// 模式位置属于原窗口的本次真实输入；只有弱Native身份和原controller已有revision。
struct ModeController {
    controller: objc2::rc::Weak<NativeMarkdownController>,
    find_input: u64,
    header: u64,
    workspace_input: u64,
}
impl ModeController {
    /// 只核原控制器的纯代次；调用方已经持有这一个真实controller，不在Native步骤之间另领弱票。
    fn permits(&self, controller: &NativeMarkdownController) -> bool {
        native_markdown_controller_has_foreground_authority(controller)
            && controller.find_input_intent() == self.find_input
            && controller.ivars().header_layout_revision.get() == self.header
            && controller.workspace_query_intent() == self.workspace_input
    }

    /// 仅在Native边界借原controller；纯Core current不调用Weak.load，也不重领较新的输入票。
    fn current_native(&self) -> bool {
        self.controller
            .load()
            .is_some_and(|controller| self.permits(&controller))
    }
}

/// 模式按钮尚待原排版Ready时唯一保留的业务动作；没有Scene、Source、Reader或Native强边。
/// publication只记录真实发布的弱key，queued只表示这一次已交原主队列，二者都不代表切换完成。
pub(super) struct PendingModeAction {
    original: Rc<NativeMarkdownWorkspacePagedEditIntent>,
    controller: Rc<ModeController>,
    revision: u64,
    request: NativeMarkdownPagedSceneRequest,
    previous: MarkdownPresentationMode,
    destination: MarkdownPresentationMode,
    publication: RefCell<Option<NativeMarkdownPagedSceneKey>>,
    queued: Cell<bool>,
    cancelled: Cell<bool>,
}

impl PendingModeAction {
    /// 输入撤销后只允许补偿这个仍在前台的原Doc/Body按钮，不把同控制器后继笔记当作旧目标。
    fn owns_surface(
        &self,
        owner: &NativeMarkdownPagedBody,
        controller: &NativeMarkdownController,
    ) -> bool {
        crate::native_surface::ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
            active.as_ref().is_some_and(|surface| {
                std::ptr::eq(&*surface.controller, controller)
                    && native_markdown_controller_has_foreground_authority(controller)
                    && surface.editor.as_ref().is_some_and(|editor| {
                        Weak::ptr_eq(&self.request.document, &Rc::downgrade(editor.document()))
                            && std::ptr::eq(&*editor.body, owner)
                    })
            })
        })
    }

    /// 排队期间必须仍是原文稿、完整父子Input、原模式和几何；当前Ready不能替旧意图补权。
    fn current(
        &self,
        owner: &NativeMarkdownPagedBody,
        controller: &NativeMarkdownController,
    ) -> bool {
        owner.current_body()
            && owner.mode_action_revision.get() == self.revision
            && owner.presentation_mode() == self.previous
            && self.original.current()
            && self.controller.permits(controller)
            && owner.view().is_ok_and(|view| {
                view.capture_scene_request()
                    .is_ok_and(|actual| actual.same(&self.request))
            })
            && self.original.current()
            && self.controller.permits(controller)
            && owner.mode_action_revision.get() == self.revision
            && owner.presentation_mode() == self.previous
    }
}

impl NativeMarkdownPagedBody {
    /// 点击在首Native前已绑定原Input；这里只取同一原几何和真实按钮状态，Pending不先辞任或提交。
    pub(in crate::native_surface) fn accept_presentation_mode_click(
        &self,
        controller: &NativeMarkdownController,
        original: NativeMarkdownWorkspacePagedEditIntent,
        sender: &objc2::runtime::AnyObject,
    ) {
        // 模式点击是独立于文字输入的真实意图；原getter内再次点击同一按钮也必须撤销外层动作。
        // 代次在首Native前推进，仅用来确认最后一次点击，不表示模式/排版已经完成。
        let revision = self
            .mode_action_revision
            .get()
            .checked_add(1)
            .expect("mode action revision overflow");
        self.mode_action_revision.set(revision);
        let find_input = controller.find_input_intent();
        let header = controller.ivars().header_layout_revision.get();
        let workspace_input = controller.workspace_query_intent();
        let old = self.pending_mode_action.borrow_mut().take();
        let original = Rc::new(original);
        let scope = Rc::new(ModeController {
            find_input,
            header,
            workspace_input,
            controller: objc2::rc::Weak::from(controller),
        });
        // 首次Native weak构造前已固定全部代次；旧weak的析构同样可能重入，不能在drop后重领输入。
        // 新点击撤上一动作，包括选回当前实际模式；此时不恢复按钮而覆盖刚收到的新状态。
        drop(old);
        let admitted = self.prepare_presentation_mode_click(
            controller,
            Rc::clone(&original),
            Rc::clone(&scope),
            revision,
            sender,
        );
        if let Err(cause) = admitted {
            // 同步拒绝同样只能反馈原Input/原Doc；新输入或getter内后继点击不能被旧错误回滚按钮。
            let current = || {
                self.mode_action_revision.get() == revision
                    && original.current()
                    && scope.permits(controller)
                    && self.pending_mode_action.borrow().is_none()
            };
            if current() {
                controller.select_presentation_mode(self.presentation_mode());
                if current() {
                    controller.present_error(&cause);
                }
            }
        }
    }

    /// 原几何与真实目标完整捕获后才安装一个动作；尚无Ready只登记等待，不执行模式事务。
    fn prepare_presentation_mode_click(
        &self,
        controller: &NativeMarkdownController,
        original: Rc<NativeMarkdownWorkspacePagedEditIntent>,
        scope: Rc<ModeController>,
        revision: u64,
        sender: &objc2::runtime::AnyObject,
    ) -> std::result::Result<(), String> {
        let previous = self.presentation_mode();
        let current = || {
            self.current_body()
                && self.mode_action_revision.get() == revision
                && self.presentation_mode() == previous
                && original.current()
                && scope.permits(controller)
        };
        if !current() {
            return Err("展示模式动作已失效".into());
        }
        let view = self.view().map_err(|cause| cause.to_string())?;
        // 这是原View公开的请求捕获，不要求Scene存在；无合法几何仍真实拒绝，不能猜一个旧锚点。
        let request = crate::native_surface::native_markdown_table_intent_step(&current, || {
            view.capture_scene_request()
        })
        .map_err(|_| "展示模式捕获期间原输入已改变")?
        .map_err(|cause| cause.to_string())?;
        let button = crate::native_surface::native_markdown_table_intent_step(&current, || {
            sender.downcast_ref::<objc2_app_kit::NSButton>()
        })
        .map_err(|_| "展示模式选择期间原输入已改变")?
        .ok_or("展示模式动作缺少原源码按钮")?;
        let owned = controller
            .ivars()
            .presentation_mode_button
            .get()
            .expect("原窗口已装配源码按钮");
        if !std::ptr::eq(button, &**owned) {
            return Err("展示模式动作不属于原源码按钮".into());
        }
        let state =
            crate::native_surface::native_markdown_table_intent_step(&current, || button.state())
                .map_err(|_| "展示模式选择期间原输入已改变")?;
        let destination = if state == objc2_app_kit::NSControlStateValueOn {
            MarkdownPresentationMode::Source
        } else if state == objc2_app_kit::NSControlStateValueOff {
            MarkdownPresentationMode::InlinePreview
        } else {
            return Err("展示模式选择无效".into());
        };
        if destination == previous {
            controller.select_presentation_mode(previous);
            return Ok(());
        }
        let pending = Rc::new(PendingModeAction {
            original,
            controller: scope,
            revision,
            request,
            previous,
            destination,
            publication: RefCell::new(None),
            queued: Cell::new(false),
            cancelled: Cell::new(false),
        });
        if !pending.current(self, controller) {
            return Err("展示模式捕获的原几何或输入已改变".into());
        }
        *self.pending_mode_action.borrow_mut() = Some(Rc::clone(&pending));
        match view.capture_ready_scene_key() {
            Ok(key) => *pending.publication.borrow_mut() = Some(key),
            Err(ViewError::Pending) => {} // 原ready_scene已唤醒唯一Producer，只等它真实发布，不再排重试。
            Err(cause) => {
                drop(self.take_mode_action(&pending));
                return Err(cause.to_string());
            }
        }
        self.deliver_pending_mode_action();
        Ok(())
    }

    /// 只由两个原Scene成功发布出口调用；记录Copy/弱key，不在Producer或Native借用栈切换模式。
    pub(super) fn mode_scene_published(&self) {
        let pending = self.pending_mode_action.borrow().clone();
        let Some(pending) =
            pending.filter(|pending| !pending.queued.get() && !pending.cancelled.get())
        else {
            return;
        };
        let key = self
            .scene
            .borrow()
            .as_ref()
            .map(|scene| scene.key().clone());
        // 后继输入的真实发布也只是一条通知；下面交付时仍按原request核对，不能接纳它的新权限。
        *pending.publication.borrow_mut() = key;
    }

    /// 原advance和所有同步借用归还后才交一次owned队列；没有真实发布就无操作，不轮询Ready。
    pub(super) fn deliver_pending_mode_action(&self) {
        if self.preparing.get() {
            return;
        }
        let pending = self.pending_mode_action.borrow().clone();
        let Some(pending) = pending.filter(|pending| !pending.queued.get()) else {
            return;
        };
        let key = pending.publication.borrow_mut().take();
        if key.is_none() && !pending.cancelled.get() {
            return;
        }
        pending.queued.set(true);
        let Some(controller) = pending.controller.controller.load() else {
            drop(self.take_mode_action(&pending));
            return;
        };
        let body = MainThreadBound::new(self.weak.clone(), self.mtm);
        let pending = MainThreadBound::new(Rc::downgrade(&pending), self.mtm);
        let key = MainThreadBound::new(key, self.mtm);
        // 原模式动作已经由本 Body 的弱文稿、完整 Input、几何和 revision 接纳。
        // AppKit 嵌套循环可暂借 Active；通用 enqueue_owned 只认槽内 owner，会在本闭包之前
        // 静默丢掉合法动作并留下 queued=true。这里直交原主队列，由下面原 pending/槽身份
        // 完整裁定执行或取消；真正 Busy 仍沿原事务失败，不重放、延时或借后继票补权。
        crate::native_surface::enqueue_native_markdown_main_task(move || {
            let mtm = MainThreadMarker::new().expect("原模式动作只在主队列交付");
            let (Some(owner), Some(pending)) =
                (body.get(mtm).upgrade(), pending.get(mtm).upgrade())
            else {
                return;
            };
            let Some(controller) = pending.controller.controller.load() else {
                drop(owner.take_mode_action(&pending));
                return;
            };
            let valid = !pending.cancelled.get()
                && pending.current(&owner, &controller)
                && key.get(mtm).as_ref().is_some_and(|key| {
                    key.presentation_mode == pending.previous
                        && key.request.same(&pending.request)
                        && owner.view().is_ok_and(|view| {
                            view.capture_ready_scene_key()
                                .is_ok_and(|actual| actual.same(key))
                        })
                })
                && pending.current(&owner, &controller);
            // 槽身份是最后的接纳门；getter重入的新click已经替换它，旧闭包绝不能清掉新动作。
            let Some(accepted) = owner.take_mode_action(&pending) else {
                return;
            };
            drop(accepted);
            if !valid {
                if pending.owns_surface(&owner, &controller)
                    && owner.pending_mode_action.borrow().is_none()
                {
                    controller.select_presentation_mode(owner.presentation_mode());
                }
                return;
            }
            // 从这里才执行原辞任、单格提交和模式事务一次；失败沿原入口呈现，不重复本动作。
            if let Err(cause) = crate::native_surface::set_active_native_markdown_presentation_mode(
                pending.destination,
            ) && pending.owns_surface(&owner, &controller)
                && owner.mode_action_revision.get() == pending.revision
                && owner.pending_mode_action.borrow().is_none()
            {
                // 本事务可合法推进Input/Find；失败反馈只核原Doc/Body与本次click，不能写入后继笔记。
                controller.present_error(&cause);
            }
        });
        // 与原 owned 入队保持相同临时强边；原队列只携弱身份，控制器析构在所有槽借用之外。
        drop(controller);
    }

    /// 只取走本动作的原槽；返回值在槽借用结束后析构，避免Native weak控制块跨RefMut释放。
    fn take_mode_action(&self, original: &Rc<PendingModeAction>) -> Option<Rc<PendingModeAction>> {
        let mut slot = self.pending_mode_action.borrow_mut();
        if slot
            .as_ref()
            .is_some_and(|actual| Rc::ptr_eq(actual, original))
        {
            slot.take()
        } else {
            None
        }
    }

    /// 真实输入或外部滚动只撤旧业务票；仍保同一槽到原队列消费，才能核定是否有后继点击并恢复按钮。
    /// 这里只安排一次取消反馈；失败/关闭直接释放该槽，不创建另一条模式或重试排版管线。
    pub(super) fn cancel_pending_mode_action(&self) {
        let pending = self.pending_mode_action.borrow().clone();
        if let Some(pending) = pending {
            pending.cancelled.set(true);
            let publication = pending.publication.borrow_mut().take();
            drop(publication);
            self.deliver_pending_mode_action();
        }
    }

    /// 当前Ready图只借一个实际字形；捕获不推进排版、不按行号估算，也不移动文稿选择。
    pub(in crate::native_surface) fn capture_mode_viewport(
        &self,
    ) -> Result<(ModeSourceAnchor, NativeMarkdownPagedSceneKey)> {
        let view = self.view()?;
        let key = view.capture_ready_scene_key()?;
        let scene = self.scene.borrow().clone().ok_or(ViewError::Pending)?;
        if !scene.key().same(&key) || !self.scene_current(&key) {
            return Err(ViewError::Revoked.into());
        }
        let document = self.document.borrow().clone().ok_or(ViewError::Revoked)?;
        let clip = key.request.viewport;
        for offset in scene.mode_source_candidates() {
            let offset = offset?; // 格显示目录的真实I/O错误不能被跳过候选的控制流掩盖。
            // 非空候选最多读取一个scalar；GCB几何仍由同一sealed Scene认证，绝不读取整行。
            let probe = {
                let document = document.borrow();
                if offset > key.request.input.utf16_len {
                    return Err(ViewError::InvalidRange.into());
                }
                if offset == key.request.input.utf16_len {
                    offset..offset
                } else {
                    let unit = document
                        .utf16_code_unit_at(offset)
                        .map_err(ViewError::Document)?;
                    let end = offset
                        + if (0xd800..=0xdbff).contains(&unit) {
                            2
                        } else {
                            1
                        };
                    document
                        .byte_offset_for_utf16(offset)
                        .map_err(ViewError::Document)?;
                    document
                        .byte_offset_for_utf16(end)
                        .map_err(ViewError::Document)?;
                    offset..end
                }
            };
            let first = match scene.first_rect(NSRange::new(probe.start, probe.len())) {
                Ok(first) => first,
                Err(_cause @ (ViewError::Pending | ViewError::InvalidRange)) => {
                    #[cfg(feature = "native-markdown-runtime-probe")]
                    if std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some() {
                        eprintln!(
                            "native_source_mode_capture candidate={}..{} error={_cause:?} primary={:?} canonical={:?}",
                            probe.start,
                            probe.end,
                            key.request.input.primary_range,
                            key.request.input.selection
                        );
                    }
                    continue;
                }
                Err(cause) => return Err(cause.into()),
            };
            let frame = first.rect;
            let end = first
                .actual_range
                .location
                .checked_add(first.actual_range.length)
                .ok_or(ViewError::InvalidRange)?;
            // 顶部偏移采用完整字形框；部分裁切行可继续找下一真实行，不能把裁切交集当完整glyph。
            if first.actual_range.location > probe.start
                || end < probe.end
                || ![
                    frame.origin.x,
                    frame.origin.y,
                    frame.size.width,
                    frame.size.height,
                    frame.origin.x + frame.size.width,
                    frame.origin.y + frame.size.height,
                ]
                .into_iter()
                .all(f64::is_finite)
                || frame.size.height <= 0.0
                // 与原TextKit minimal-scroll合同一致，半点只消除真实clip/glyph的浮点边缘取整。
                || frame.origin.y < clip.origin.y - 0.5
                || frame.origin.y + frame.size.height > clip.origin.y + clip.size.height + 0.5
                || frame.origin.x + frame.size.width < clip.origin.x
                || frame.origin.x > clip.origin.x + clip.size.width
            {
                #[cfg(feature = "native-markdown-runtime-probe")]
                if std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some() {
                    eprintln!(
                        "native_source_mode_capture candidate={}..{} accepted=false first={:?} actual_range={:?} clip={clip:?} top_clip={} bottom_clip={} primary={:?} canonical={:?}",
                        probe.start,
                        probe.end,
                        frame,
                        first.actual_range,
                        (frame.origin.y + frame.size.height) - (clip.origin.y + clip.size.height),
                        clip.origin.y - frame.origin.y,
                        key.request.input.primary_range,
                        key.request.input.selection
                    );
                }
                continue;
            }
            #[cfg(feature = "native-markdown-runtime-probe")]
            if std::env::var_os("OMNI_NATIVE_PROBE_TRACE").is_some() {
                eprintln!(
                    "native_source_mode_capture candidate={}..{} accepted=true first={frame:?} actual_range={:?} clip={clip:?} primary={:?} canonical={:?}",
                    probe.start,
                    probe.end,
                    first.actual_range,
                    key.request.input.primary_range,
                    key.request.input.selection
                );
            }
            if !self.scene_current(&key) || !view.capture_ready_scene_key()?.same(&key) {
                return Err(ViewError::Revoked.into());
            }
            return Ok((
                ModeSourceAnchor {
                    document: Rc::downgrade(&document),
                    generation: key.request.input.generation,
                    length: key.request.input.utf16_len,
                    selection: key.request.input.selection,
                    probe,
                    // Source为非翻转CG坐标，TextKit为向下源坐标；统一记录真实正文顶部偏移。
                    screen_y: clip.origin.y + clip.size.height - frame.origin.y - frame.size.height,
                },
                key,
            ));
        }
        Err(ViewError::Pending.into())
    }

    /// Find隐藏增高视口时保留同源真实字形；已测前缀不能被原生clip误当全文尾部夹滚。
    /// 同步布局只借原preparing准入，后续补前缀及恢复仍归同一个PendingModeViewport和Composer。
    pub(in crate::native_surface) fn hide_find_bar(
        &self,
        bar: &Rc<NativeMarkdownFindBar>,
        scroll: &NSScrollView,
        controller: &NativeMarkdownController,
    ) -> std::result::Result<bool, String> {
        if !bar.is_visible() {
            return Ok(false);
        }
        let view = self.view().map_err(|cause| cause.to_string())?;
        // 已有producer借出时仍允许原Hide布局；不能取得别人的preparing后再擅自归还。
        // 只有本次真正取得准入才创建原guard，退出后用户滚动继续按原通知撤锚。
        let was_preparing = self.preparing.replace(true);
        let preparation =
            (!was_preparing).then(|| super::presentation::PresentationUse(&self.preparing));
        let mut installed = None;
        let result = (|| {
            let scope = Rc::new(ModeController {
                find_input: controller.find_input_intent(),
                header: controller.ivars().header_layout_revision.get(),
                workspace_input: controller.workspace_query_intent(),
                controller: objc2::rc::Weak::from(controller),
            });
            if !scope.permits(controller) {
                return Err(ViewError::Revoked.to_string());
            }
            self.cancel_pending_mode_action();
            // 正在恢复的真实位置属于原Source请求；Hide只移交本次Find权限，不重挑锚点或重建来源。
            let inherited = self.inherit_find_layout_anchor(&scope, controller);
            self.cancel_pending_find()
                .map_err(|cause| cause.to_string())?;
            if !inherited && !was_preparing && self.scene.borrow().is_some() {
                match self.begin_find_layout_anchor(&scope, controller) {
                    Ok(target) => installed = Some(target),
                    Err(Error::View(ViewError::Pending | ViewError::Busy | ViewError::Revoked)) => {
                        // 尚无可认证的当前字形时，Hide本身仍执行；不宣称恢复一个从未捕获的位置。
                    }
                    Err(cause) => return Err(cause.to_string()),
                }
            }
            let hidden = bar.hide(scroll, view.as_super(), controller)?;
            if installed
                .as_ref()
                .is_some_and(|target| !target.current_native(self))
            {
                return Err(ViewError::Revoked.to_string());
            }
            Ok(hidden)
        })();
        drop(preparation); // 所有同步Native及槽借用结束后，才恢复原唯一主队列量子。
        if was_preparing {
            // 外层原owner继续消费其worker完成和deferred；本次布局仅请求同一后继，不能清别人的槽。
            self.deferred.set(true);
            return result;
        }
        // 此准入只归本次布局；下面的一次原schedule同时消费布局和所有已登记deferred，
        // worker通知仍留在原队列，由正常schedule完成出口逐项领取，不在这里自行处理。
        self.deferred.set(false);
        // Native取消不得掩盖原冻结Source错误；真实失败沿Body统一失败/退休路径归还全部资源。
        let health = self
            .producer
            .borrow()
            .as_ref()
            .map_or(Ok(()), |producer| producer.target_source_health());
        if let Err(cause) = health {
            let message = cause.to_string();
            self.fail(cause);
            return Err(message);
        }
        if result.is_err()
            && let Some(target) = &installed
        {
            let owns = self.mode_viewport.borrow().as_ref().is_some_and(|pending| {
                pending
                    .target
                    .as_ref()
                    .is_some_and(|actual| Rc::ptr_eq(actual, target))
            });
            if owns {
                self.cancel_mode_viewport(); // 只撤本次已安装票；重入后继动作不得被旧失败清掉。
            }
        }
        if self.current_body() {
            self.schedule().map_err(|cause| cause.to_string())?;
        }
        result
    }

    /// 已有实际锚只移交同窗口Hide的Find scope；不改变原Source、target、测量或原字形位置。
    fn inherit_find_layout_anchor(
        &self,
        scope: &Rc<ModeController>,
        controller: &NativeMarkdownController,
    ) -> bool {
        let captured = self.mode_viewport.borrow().as_ref().map(|pending| {
            (
                pending.anchor.clone(),
                pending.state,
                pending.target.clone(),
                pending.controller.clone(),
            )
        });
        let Some((anchor, state, target, Some(original))) = captured else {
            return false;
        };
        // 弱Native加载在槽借用外；返回后还必须核原scope Rc及target身份，不能复活已替换或撤销的票。
        if !original
            .controller
            .load()
            .is_some_and(|actual| std::ptr::eq(&*actual, controller))
            || !scope.permits(controller)
            || !anchor.current_document()
            || self.input.state().ok() != Some(state)
            || target.as_ref().is_some_and(|target| !target.current(self))
        {
            return false;
        }
        let previous = {
            let mut slot = self.mode_viewport.borrow_mut();
            let Some(pending) = slot.as_mut() else {
                return false;
            };
            let same_target = match (&pending.target, &target) {
                (Some(actual), Some(original)) => Rc::ptr_eq(actual, original),
                (None, None) => true,
                _ => false,
            };
            if !same_target
                || !pending
                    .controller
                    .as_ref()
                    .is_some_and(|actual| Rc::ptr_eq(actual, &original))
            {
                return false;
            }
            pending.controller.replace(Rc::clone(scope))
        };
        drop(previous); // 原NativeWeak控制块归还不跨mode槽借用；后继输入仍能撤原固定槽。
        true
    }

    /// 只在实际Ready图中创建这次Find布局锚；既有Producer及来源保留，不另建Doc或Parser。
    fn begin_find_layout_anchor(
        &self,
        scope: &Rc<ModeController>,
        controller: &NativeMarkdownController,
    ) -> Result<Rc<target::SourceTargetRequest>> {
        let (anchor, key) = self.capture_mode_viewport()?;
        let scene = self.scene.borrow().clone().ok_or(ViewError::Pending)?;
        if !scene.key().same(&key) {
            return Err(ViewError::Revoked.into());
        }
        // 测量必须来自捕获锚点的同一nonce；字号、marker显隐和字形高度不从源范围或行号猜测。
        // 保存真实文档上沿后，Hide造成的短prefix夹滚不会覆盖这份原位置意图。
        let first = scene.first_rect(NSRange::new(anchor.probe.start, anchor.probe.len()))?;
        let measured = MeasuredModeGlyph {
            document_top: key.request.virtual_viewport.document_origin.y
                + key.request.viewport.origin.y
                + key.request.viewport.size.height
                - first.rect.origin.y
                - first.rect.size.height,
            height: first.rect.size.height,
        };
        drop(scene); // 跨布局只保原实际标量，旧Scene/Reader不因锚点而继续保活。
        if !scope.permits(controller)
            || !self
                .view()?
                .capture_ready_scene_key()
                .is_ok_and(|actual| actual.same(&key))
        {
            return Err(ViewError::Revoked.into());
        }
        // 原Producer公开入口先核Source健康和完整请求，再借身份；没有复制映射或另起解析请求。
        // 新视口需要的bottom仍由原目标按真实测量计算，只有Composer真EOF可以结束补前缀。
        let source = self
            .producer
            .borrow()
            .as_ref()
            .ok_or(ViewError::Pending)?
            .target_source_authority(&key.request, self.serial.get())?;
        let target = self.admit_source_target(
            &key.request,
            source,
            NSRange::new(anchor.probe.start, anchor.probe.len()),
            &|| anchor.current_document() && scope.permits(controller),
        )?;
        self.cancel_mode_viewport();
        self.install_source_target(Rc::clone(&target))?;
        // 安装只撤旧Reader并排原任务；若这一步重入已换输入，撤本票而不把新状态装回旧锚。
        if !target.current(self) || !scope.permits(controller) {
            target.revoke();
            return Err(ViewError::Revoked.into());
        }
        *self.mode_viewport.borrow_mut() = Some(PendingModeViewport {
            anchor,
            state: key.request.input,
            target: Some(Rc::clone(&target)),
            controller: Some(Rc::clone(scope)),
            measured_glyph: Some(measured),
        });
        Ok(target)
    }

    /// 候选尚未首Ready时登记一次；正式创建失败保旧图，不用临时选择或伪Undo驱动定位。
    pub(in crate::native_surface) fn restore_mode_viewport(
        &self,
        anchor: ModeSourceAnchor,
    ) -> Result<()> {
        if self.producer.borrow().is_some() || self.scene.borrow().is_some() {
            return Err(ViewError::Busy.into());
        }
        let state = self.input.state().map_err(ViewError::Input)?;
        if !anchor.current_document()
            || !Weak::ptr_eq(&anchor.document, &self.input.document_identity())
            || state.generation != anchor.generation
            || state.utf16_len != anchor.length
            || state.selection != anchor.selection
        {
            return Err(ViewError::Revoked.into());
        }
        *self.mode_viewport.borrow_mut() = Some(PendingModeViewport {
            anchor,
            state,
            target: None,
            controller: None,
            measured_glyph: None,
        });
        self.schedule()
    }

    /// 唯一新scroll已完成原布局/Find焦点交接；此处只签原controller权限，不再初始化正文图。
    pub(in crate::native_surface) fn bind_mode_viewport(
        &self,
        controller: &Retained<NativeMarkdownController>,
    ) -> Result<()> {
        let scope = ModeController {
            controller: objc2::rc::Weak::from_retained(controller),
            find_input: controller.find_input_intent(),
            header: controller.ivars().header_layout_revision.get(),
            workspace_input: controller.workspace_query_intent(),
        };
        if !scope.current_native() {
            self.cancel_mode_viewport();
            if self.current_body() {
                // 外层焦点/挂载失败也可能已消费首个准备任务；取消旧锚点后显式恢复当前同模式。
                self.schedule()?;
            }
            return Err(ViewError::Revoked.into());
        }
        let scope = Rc::new(scope);
        let previous = self
            .mode_viewport
            .borrow_mut()
            .as_mut()
            .and_then(|pending| pending.controller.replace(Rc::clone(&scope)));
        drop(previous); // 旧scope中的Objective-C弱槽在RefCell借用结束后销毁，不在槽内调用weak-copy或析构原语。
        self.schedule()
    }

    /// Native装配尚在本次挂载栈时不发布文首；旧controller输入则只撤旧mode目标，保留新输入。
    pub(super) fn mode_viewport_waits_for_mount(&self) -> bool {
        let scope = {
            let slot = self.mode_viewport.borrow();
            let Some(pending) = slot.as_ref() else {
                return false;
            };
            pending.controller.clone()
        };
        let Some(scope) = scope else {
            return true;
        };
        if !scope.current_native() {
            self.cancel_mode_viewport();
        }
        false
    }

    /// 只有原mode自己的target受此窗口票约束，普通Find/导航仍走它们自己的原始权限。
    pub(super) fn mode_target_current_native(&self, target: &target::SourceTargetRequest) -> bool {
        let scope = {
            let slot = self.mode_viewport.borrow();
            let Some(pending) = slot.as_ref() else {
                return true;
            };
            if !pending
                .target
                .as_ref()
                .is_some_and(|actual| std::ptr::eq(&**actual, target))
            {
                return true;
            }
            pending.controller.clone()
        };
        scope.is_some_and(|scope| scope.current_native())
    }

    /// 原clip的用户滚动/外部resize只能撤这一旧票；自有prepare中的真实高度/滚动不走这里。
    pub(super) fn cancel_mode_for_external_clip(&self) {
        if self.preparing.get() {
            return;
        }
        self.cancel_pending_mode_action();
        let admitted = self
            .mode_viewport
            .borrow()
            .as_ref()
            .is_some_and(|pending| pending.target.is_some());
        if admitted {
            self.cancel_mode_viewport();
        }
    }

    /// 先移出有限旧票，再只撤同指针target；不能清掉用户后来安装的Find或另一个Source动作。
    pub(super) fn cancel_mode_viewport(&self) {
        self.cancel_pending_mode_action();
        let pending = self.mode_viewport.borrow_mut().take();
        let Some(target) = pending.and_then(|pending| pending.target) else {
            return;
        };
        target.revoke();
        let previous = {
            let mut slot = self.source_target.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, &target))
            {
                slot.take()
            } else {
                None
            }
        };
        drop(previous);
    }

    /// 第一个真实Producer负责签发同Source目标；没有额外capture或文首Ready作为中转。
    pub(super) fn prepare_mode_viewport(
        &self,
        producer: &mut Producer,
        request: &NativeMarkdownPagedSceneRequest,
    ) -> Result<()> {
        let (state, anchor, target) = {
            let slot = self.mode_viewport.borrow();
            let Some(pending) = slot.as_ref() else {
                return Ok(());
            };
            (
                pending.state,
                pending.anchor.clone(),
                pending.target.clone(),
            )
        }; // 先结束初始化槽借用，后续Native调度和撤权可安全重入。

        if request.input != state || !anchor.current_document() {
            drop(self.mode_viewport.borrow_mut().take());
            return Ok(()); // 后继真实输入撤销旧位置；不从当前选择重新签发原模式动作。
        }
        if let Some(target) = target {
            if !target.matches(self, request) {
                drop(self.mode_viewport.borrow_mut().take());
            }
            return Ok(());
        }
        let source = producer.target_source_authority(request, self.serial.get())?;
        let target = self.admit_source_target(
            request,
            source,
            NSRange::new(anchor.probe.start, anchor.probe.len()),
            &|| anchor.current_document() && self.input.state().is_ok_and(|actual| actual == state),
        )?;
        self.install_source_target(Rc::clone(&target))?;
        if let Some(pending) = self.mode_viewport.borrow_mut().as_mut() {
            pending.target = Some(target);
        }
        Ok(())
    }

    /// 已实测目标行上沿决定所需下方正文；布局不足时续原前缀，不能用短prefix夹到页底冒充恢复。
    pub(super) fn mode_viewport_required_bottom(
        &self,
        target: &Rc<target::SourceTargetRequest>,
        line_top: f64,
        viewport_height: f64,
    ) -> Option<f64> {
        let slot = self.mode_viewport.borrow();
        let pending = slot.as_ref()?;
        pending
            .target
            .as_ref()
            .filter(|actual| Rc::ptr_eq(actual, target))?;
        // 首 Scene 前只能沿真实行顶补前缀；测得格内字形后必须使用它的实际文档位置。
        // 目标可能在超高格中部，整行顶部不能证明原屏内锚下方的可见内容已准备完成。
        let origin = pending.measured_glyph.map_or_else(
            || (line_top - pending.anchor.screen_y).max(0.0),
            |glyph| glyph.requested_origin(pending.anchor.screen_y, viewport_height),
        );
        Some(origin + viewport_height)
    }

    /// 只在原模式目标已有真实字形锚时接管后继滚动；首次远端定位仍由原目标准备路径完成。
    /// 此方法只查同指针票与有限测量值，不授予 Ready、可见性或外部操作已完成的证明。
    pub(super) fn mode_retains_measured_glyph(
        &self,
        target: &Rc<target::SourceTargetRequest>,
    ) -> bool {
        self.mode_viewport.borrow().as_ref().is_some_and(|pending| {
            pending.measured_glyph.is_some()
                && pending
                    .target
                    .as_ref()
                    .is_some_and(|actual| Rc::ptr_eq(actual, target))
        })
    }

    /// 首目标真正排版后只用真实glyph框恢复屏内Y；滚动后的旧Scene绝不签发最终可见收据。
    pub(super) fn finish_mode_viewport(&self) -> Result<bool> {
        let (anchor, state, target) = {
            let slot = self.mode_viewport.borrow();
            let Some(pending) = slot.as_ref() else {
                return Ok(false);
            };
            (
                pending.anchor.clone(),
                pending.state,
                pending.target.clone(),
            )
        }; // 实际glyph/clip查询不持初始化槽，失败清理也不递归借用同一RefCell。

        let Some(target) = target else {
            return Ok(false);
        };
        let scene = self.scene.borrow().clone();
        let Some(scene) = scene else {
            // 本回合的原controller/Input/Source撤权已由prepare及Producer原票负责，
            // 没有Scene就无真实glyph可完成，也不执行Native滚动；不为每个Core量子再捕整组几何。
            // 一旦发布Scene，下面仍完整核原输入和target，实际副作用前后的守卫不变。
            return Ok(false);
        };
        if self.input.state().map_err(ViewError::Input)? != state || !target.current_native(self) {
            drop(self.mode_viewport.borrow_mut().take());
            return Ok(false);
        }
        if !target.matches(self, &scene.key().request) || target.source != scene.key().source {
            return Ok(false);
        }
        let first = match scene.first_rect(NSRange::new(anchor.probe.start, anchor.probe.len())) {
            Ok(first) => first,
            Err(ViewError::Pending) => return Ok(false),
            Err(cause) => {
                // 只标明既有失败出口；不额外查询原 Scene/current，也不把旧坐标拒绝改成重试。
                #[cfg(feature = "native-close-stage-diagnostic")]
                eprintln!(
                    "native_mode_phase_failure phase=first_rect cause={:?}",
                    std::mem::discriminant(&cause)
                );
                return Err(cause.into());
            }
        };
        let request = &scene.key().request;
        let viewport = self.viewport.borrow().clone().ok_or(ViewError::Closed)?;
        let geometry = viewport.geometry().inspect_err(|cause| {
            #[cfg(feature = "native-close-stage-diagnostic")]
            eprintln!(
                "native_mode_phase_failure phase=geometry cause={:?}",
                std::mem::discriminant(cause)
            );
            let _ = cause;
        })?;
        let glyph_top = request.virtual_viewport.document_origin.y
            + request.viewport.origin.y
            + request.viewport.size.height
            - first.rect.origin.y
            - first.rect.size.height;
        let maximum = (geometry.content_height - geometry.body_frame.size.height).max(0.0);
        let measured = MeasuredModeGlyph {
            document_top: glyph_top,
            height: first.rect.size.height,
        };
        let requested = measured.requested_origin(anchor.screen_y, geometry.body_frame.size.height);
        let desired = requested.min(maximum);
        if !glyph_top.is_finite() || !desired.is_finite() {
            return Err(ViewError::InvalidGeometry.into());
        }
        // first_rect 已由当前同票 Scene 认证，才交接后继滚动的唯一归属。
        // Native reveal 可能换几何票；保留实际测量而不提前消费 mode，下一 Scene 仍须重新核验。
        if let Some(pending) = self.mode_viewport.borrow_mut().as_mut()
            && pending
                .target
                .as_ref()
                .is_some_and(|actual| Rc::ptr_eq(actual, &target))
        {
            pending.measured_glyph = Some(measured);
        }
        // 当前 Scene 可能刚好封存在长格中段；临时页底不能替代用户原来的屏内位置。
        // 先让唯一 Producer 补到实际目标下方的可见区，EOF 以外不接受 clamp 后的假完成。
        let required_bottom = requested + geometry.body_frame.size.height;
        let needs_prefix = self
            .producer
            .borrow()
            .as_ref()
            .expect("模式 Scene 仍归唯一实际 Producer")
            .needs_document_prefix(required_bottom);
        if needs_prefix {
            // publish 已标脏这一中间 Scene；位置还未恢复，必须先撤票，不能让 CPU observer
            // 抢在补前缀续体前把它当作模式完成帧。原测量与唯一 Producer 均继续保留。
            self.clear_scene()?;
            self.schedule()?;
            return Ok(true);
        }
        let changed = viewport.reveal_document_rect(
            NSRect::new(
                NSPoint::new(geometry.body_frame.origin.x, desired),
                NSSize::new(
                    1.0,
                    geometry.body_frame.size.height.min(geometry.content_height),
                ),
            ),
            &|| target.current_native(self),
        );
        // 普通前台取消不能盖掉原Source的首个I/O错误；Native操作已经消费，绝不重放。
        if let Some(producer) = self.producer.borrow().as_ref() {
            producer.target_source_health()?;
        }
        #[cfg(feature = "native-close-stage-diagnostic")]
        if let Err(cause) = &changed {
            // 原 Source 首错仍在上方优先返回；此处不提前消费或重放已发生的 Native reveal。
            eprintln!(
                "native_mode_phase_failure phase=reveal cause={:?}",
                std::mem::discriminant(cause)
            );
        }
        if changed? {
            self.schedule()?;
            return Ok(true);
        }
        let actual_y = request.viewport.origin.y + request.viewport.size.height
            - first.rect.origin.y
            - first.rect.size.height;
        let expected_y = glyph_top - desired;
        #[cfg(feature = "native-markdown-runtime-probe")]
        eprintln!(
            "native_source_mode_position scalar={}..{} old_screen_y={} glyph_document_top={glyph_top} requested_origin={} constrained_origin={desired} actual_origin={} actual_screen_y={actual_y} prefix_height={} glyph={:?} viewport={:?}",
            anchor.probe.start,
            anchor.probe.end,
            anchor.screen_y,
            requested,
            request.virtual_viewport.document_origin.y,
            geometry.content_height,
            first.rect,
            request.viewport
        );
        if !target.current_native(self) || (actual_y - expected_y).abs() > 0.5 {
            return Err(ViewError::Revoked.into());
        }
        self.view()?
            .verify_find_target_visible(scene.key(), target.range)
            .inspect_err(|cause| {
                #[cfg(feature = "native-close-stage-diagnostic")]
                eprintln!(
                    "native_mode_phase_failure phase=verify cause={:?}",
                    std::mem::discriminant(cause)
                );
                let _ = cause;
            })?;
        drop(self.mode_viewport.borrow_mut().take());
        Ok(false)
    }
}
