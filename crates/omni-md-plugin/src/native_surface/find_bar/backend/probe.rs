//! 原Owner lease上的真实Find组件控制；没有旧正文TextKit图或假Visible结果。

use super::*;
use crate::native_surface::find_bar::{
    NativeMarkdownFindExecution, NativeMarkdownFindExecutionError, NativeMarkdownPagedFindBackend,
    NativeMarkdownPagedFindRevealPort, NativeMarkdownPagedFindRevealProgress,
    NativeMarkdownPagedFindRevealRequest,
};
use crate::native_surface::paged_text_view::{
    NativeMarkdownPagedCloseReceipt, NativeMarkdownPagedSceneKey, NativeMarkdownPagedSceneRequest,
    NativeMarkdownPagedTextScene, NativeMarkdownPagedTextViewDelegate,
    NativeMarkdownPagedTextViewError as ViewError, NativeMarkdownPagedVirtualViewport,
};
use std::io::Write;

#[path = "probe/ready_scene.rs"]
mod ready_scene;
#[cfg(feature = "native-paged-body-probe")]
#[path = "probe/source_body.rs"]
mod source_body;

/// 原父窗口仍存活时订阅本次组件释放；仅真实资源事件可结束这一次有限等待。
fn wait_closed_view(receipt: &Rc<NativeMarkdownPagedCloseReceipt>) -> Result<(), String> {
    let arrived = Rc::new(Cell::new(false));
    let observed = Rc::clone(&arrived);
    let run_loop = objc2_core_foundation::CFRunLoop::current()
        .ok_or_else(|| "当前主线程没有原RunLoop".to_string())?;
    receipt.when_released(move || {
        observed.set(true);
        run_loop.stop(); // 真正的最后资源归还才终止本次RunLoop，不以空转或窗口关闭冒充完成。
    });
    // SAFETY: 验收位于主线程，常量mode有效；三秒是原控制的失败上限，不是释放条件。
    unsafe {
        objc2_core_foundation::CFRunLoop::run_in_mode(
            objc2_core_foundation::kCFRunLoopDefaultMode,
            3.0,
            false,
        );
    }
    if !arrived.get() {
        return Err("原Find视图在三秒内没有真实资源完成事件".into());
    }
    receipt.verify_released()
}

struct PendingDelegate {
    controller: Weak<NativeMarkdownController>,
    fail_next_commit: Cell<bool>,
}
impl NativeMarkdownPagedTextViewDelegate for PendingDelegate {
    fn current(&self, epoch: u64) -> bool {
        epoch == 7
            && self.controller.load().is_some_and(|controller| {
                native_markdown_controller_has_foreground_authority(&controller)
            })
    }
    fn virtual_viewport(
        &self,
        epoch: u64,
        bounds_width: f64,
    ) -> Result<NativeMarkdownPagedVirtualViewport, ViewError> {
        if !self.current(epoch) {
            return Err(ViewError::Revoked);
        }
        Ok(NativeMarkdownPagedVirtualViewport {
            document_origin: NSPoint::ZERO,
            layout_origin_x: 0.0,
            layout_width: bounds_width,
            scroll_epoch: 1,
        })
    }
    fn ready_scene(
        &self,
        _: &NativeMarkdownPagedSceneRequest,
    ) -> Result<Rc<dyn NativeMarkdownPagedTextScene>, ViewError> {
        Err(ViewError::Pending)
    }
    fn scene_current(&self, _: &NativeMarkdownPagedSceneKey) -> bool {
        false
    }
    fn committed(&self, _: &NativeMarkdownPagedInputCommit) -> Result<(), ViewError> {
        if self.fail_next_commit.replace(false) {
            return Err(ViewError::Pending);
        }
        Ok(())
    }
    fn reveal_committed_selection(
        &self,
        _: &NativeMarkdownPagedInputCommit,
    ) -> Result<(), ViewError> {
        // 本有限组件没有正文Producer/reveal续体，不能把选择提交伪装成实际可见。
        Err(ViewError::Pending)
    }
    fn copy_selection(&self, _: &NativeMarkdownPagedSceneRequest) -> Result<(), ViewError> {
        Err(ViewError::Pending)
    }
}

#[derive(Default)]
struct PendingPort(RefCell<Option<Rc<NativeMarkdownPagedFindRevealRequest>>>);
impl NativeMarkdownPagedFindRevealPort for PendingPort {
    fn cancel_pending(&self) {
        let previous = self.0.borrow_mut().take();
        if let Some(previous) = previous {
            previous.cancel();
        }
    }
    fn begin(
        &self,
        request: Rc<NativeMarkdownPagedFindRevealRequest>,
    ) -> Result<NativeMarkdownPagedFindRevealProgress, String> {
        if !request.is_current() {
            return Err("原Pending请求已经失效".into());
        }
        *self.0.borrow_mut() = Some(request);
        Ok(NativeMarkdownPagedFindRevealProgress::Pending) // 本控制无Composer，禁止签Visible或“找到匹配”。
    }
}

/// 只验证有限合成事务与Pending撤权；真正绘图、定位和整产品峰值仍须后续原窗口验收。
#[cfg(feature = "native-paged-body-probe")]
pub fn run_native_paged_find_body_transaction_control(mtm: MainThreadMarker) -> Result<(), String> {
    let application = NSApplication::sharedApplication(mtm);
    if !application.isRunning() {
        application.finishLaunching(); // 与原控制相同的实际AppKit初始化，不创建正文替身。
    }
    source_body::run_transactions(mtm)
}

/// 原控制完整保留，独立事务入口不会改变此处的远端/完整选择和原资源门。
pub fn run_native_paged_find_backend_control(mtm: MainThreadMarker) -> Result<(), String> {
    let (owned, backing, view, document, input, controller) =
        autoreleasepool(|_| -> Result<_, String> {
            let application = NSApplication::sharedApplication(mtm);
            if !application.isRunning() {
                application.finishLaunching();
            }
            let owned = Rc::new(NativeMarkdownOwnedLedger::default());
            let panel = NativeMarkdownPanel::new(mtm);
            // SAFETY: 原Panel由本控制持强引用；close不能消费Rust仍保留的独立claim。
            unsafe {
                panel.setReleasedWhenClosed(false);
            }
            let controller = NativeMarkdownController::new(&owned, &panel, mtm);
            let owner = crate::native_surface::probe::ProbeEditingOwnerLease::new(&controller)?;
            // 同一真实owner上先核Ready可见证明；有限glyph控制不冒充完整文档Composer。
            ready_scene::run_ready_control(&controller, mtm)?;
            let mut file = tempfile::NamedTempFile::new().map_err(|error| error.to_string())?;
            file.write_all("甲 x 🙂 x 尾".as_bytes())
                .map_err(|error| error.to_string())?;
            let document = Rc::new(RefCell::new(
                MarkdownDocumentBuffer::open(file.path(), 65536)
                    .map_err(|error| error.to_string())?,
            ));
            let original = MarkdownTextSelection {
                anchor_utf16: 10,
                head_utf16: 0,
            };
            document
                .borrow_mut()
                .set_selection(original)
                .map_err(|error| error.to_string())?;
            let backing = document
                .borrow()
                .original_backing_observer()
                .ok_or("合成控制必须有真实mmap")?;
            let input = NativeMarkdownPagedInput::new(Rc::downgrade(&document))
                .map_err(|error| error.to_string())?;
            let delegate = Rc::new(PendingDelegate {
                controller: Weak::new(&*controller),
                fail_next_commit: Cell::new(false),
            });
            let erased: Rc<dyn NativeMarkdownPagedTextViewDelegate> = delegate.clone();
            let frame = NSRect::new(NSPoint::ZERO, NSSize::new(720.0, 300.0));
            let view = NativeMarkdownPagedTextView::new(
                input.clone(),
                Rc::downgrade(&erased),
                7,
                frame,
                mtm,
            )
            .map_err(|error| error.to_string())?;
            let parent = NSView::initWithFrame(NSView::alloc(mtm), frame);
            let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
            parent.addSubview(&scroll);
            scroll.setDocumentView(Some(&view));
            let bar =
                NativeMarkdownFindBar::new(&controller, &owned, Rc::downgrade(&document), mtm)?;
            bar.show(&scroll, true, &controller)?;
            bar.query.setStringValue(ns_string!("x"));
            bar.replacement.setStringValue(ns_string!("中文"));
            bar.regex.setState(1); // 第一笔实际替换覆盖弱Doc虚拟NSString的ICU路径，不创建旧storage。
            let port = PendingPort::default();
            let backend = || {
                NativeMarkdownFindBackend::Paged(NativeMarkdownPagedFindBackend {
                    view: &view,
                    input: &input,
                    epoch: 7,
                    controller: &controller,
                    intent: controller.find_input_intent(),
                    reveal: &port,
                })
            };
            let execute = |action| execute_find_backend(&bar, backend(), action);
            if !matches!(
                execute(MarkdownFindAction::ReplaceAll),
                Ok(NativeMarkdownFindExecution::Complete)
            ) {
                return Err("Paged全部替换没有完成原事务".into());
            }
            if small_text(&document)? != "甲 中文 🙂 中文 尾"
                || document.borrow().generation() != 1
                || document.borrow().selection()
                    != (MarkdownTextSelection {
                        anchor_utf16: 12,
                        head_utf16: 0,
                    })
            {
                return Err("Paged批量替换丢失字节或反向选择".into());
            }
            let current = |epoch| {
                epoch == 7 && native_markdown_controller_has_foreground_authority(&controller)
            };
            let permit = input
                .capture(7, &current)
                .map_err(|error| error.to_string())?;
            let undo = input
                .history(
                    &permit,
                    crate::native_surface::paged_input::NativeMarkdownPagedInputHistory::Undo,
                    &current,
                )
                .map_err(|error| error.to_string())?;
            if let NativeMarkdownPagedInputOutcome::Committed(commit) = undo {
                view.apply_find_commit(&input, &commit)
                    .map_err(|error| error.to_string())?;
            } else {
                return Err("Paged批量替换没有唯一真实Undo".into());
            }
            if small_text(&document)? != "甲 x 🙂 x 尾"
                || document.borrow().selection() != original
                || document.borrow().can_undo()
            {
                return Err("Paged Undo未恢复原字节与方向".into());
            }
            bar.regex.setState(0); // 后续多选/导航覆盖Core字面扫描，不依赖原生正文字符串。
            if !matches!(
                execute(MarkdownFindAction::SelectAll),
                Ok(NativeMarkdownFindExecution::Complete)
            ) {
                return Err("Paged全部选择失败".into());
            }
            let state = input.state().map_err(|error| error.to_string())?;
            let permit = input
                .capture(7, &current)
                .map_err(|error| error.to_string())?;
            if state.directed_selection().is_some()
                || input
                    .selected_ranges(&permit, &current)
                    .map_err(|error| error.to_string())?
                    != vec![2..3, 7..8]
            {
                return Err("Paged多选范围不全或伪造主方向".into());
            }
            let request = match execute(MarkdownFindAction::Next) {
                Ok(NativeMarkdownFindExecution::Pending(request)) => request,
                _ => return Err("无Ready Scene的Paged Find不得签Visible".into()),
            };
            if request.range() != NSRange::new(2, 1)
                || request.input_state() != input.state().map_err(|error| error.to_string())?
                || request.epoch() != 7
                || !std::rc::Weak::ptr_eq(&request.document(), &Rc::downgrade(&document))
                || request.completed_visible()
                || bar.probe_found_feedback()
            {
                return Err("Paged Pending错误地完成或更换身份".into());
            }
            let weak_request = Rc::downgrade(&request);
            let permit = input
                .capture(7, &current)
                .map_err(|error| error.to_string())?;
            if let NativeMarkdownPagedInputOutcome::Committed(commit) = input
                .set_selection(&permit, MarkdownTextSelection::caret(0), &current)
                .map_err(|error| error.to_string())?
            {
                view.apply_find_commit(&input, &commit)
                    .map_err(|error| error.to_string())?;
            }
            if request.is_current()
                || !matches!(
                    request.error("原输入撤销".into()),
                    NativeMarkdownFindExecutionError::CommittedEffect { .. }
                )
            {
                return Err("Pending未撤旧输入或丢失原提交事实".into());
            }
            port.cancel_pending();
            drop(request);
            if weak_request.upgrade().is_some() {
                return Err("撤销后的原Pending仍被持有".into());
            }
            delegate.fail_next_commit.set(true);
            let generation = document.borrow().generation();
            match execute(MarkdownFindAction::ReplaceAll) {
                Err(NativeMarkdownFindExecutionError::CommittedEffect { commit, .. })
                    if commit.state.generation == generation + 1 => {}
                _ => return Err("显示失败没有保留原批量提交收据".into()),
            }
            if small_text(&document)? != "甲 中文 🙂 中文 尾" {
                return Err("显示失败错误地回滚或重放正文".into());
            }
            let weak_view = Weak::new(&*view);
            let weak_document = Rc::downgrade(&document);
            let weak_input = Rc::downgrade(&input);
            let weak_controller = Weak::new(&*controller);
            owned.include("paged-find-control.view", &view)?;
            owned.include("paged-find-control.scroll", &scroll)?;
            owned.include("paged-find-control.parent", &parent)?;
            owned.include("paged-find-control.controller", &controller)?;
            owned.include("paged-find-control.panel", &panel)?;
            let mut released = owned.snapshot();
            released.seal(); // 首个拆图动作前固定真实角色；不能在Native回调后补签更小清单。
            bar.finish_accepted();
            scroll.setDocumentView(None);
            let close = view.begin_close().map_err(|error| error.to_string())?;
            wait_closed_view(&close)?;
            panel.close();
            drop(port);
            drop(bar);
            drop(scroll);
            drop(parent);
            drop(view);
            drop(erased);
            drop(delegate);
            drop(input);
            drop(document);
            drop(owner);
            drop(controller);
            drop(panel);
            drop(application);
            Ok((
                released,
                backing,
                weak_view,
                weak_document,
                weak_input,
                weak_controller,
            ))
        })?;
    if view.load().is_some()
        || document.upgrade().is_some()
        || input.upgrade().is_some()
        || controller.load().is_some()
        || !backing.unmap_succeeded()
        || backing.mapped_bytes() != 0
        || backing.failed()
    {
        return Err("Paged Find关闭后真实弱图或mmap未释放".into());
    }
    owned.verify_released()?;
    println!(
        "native_paged_find_backend exact_batch=true reversed_history=true full_ranges=true pending_only=true old_pending_revoked=true committed_effect_preserved=true actual_weak_nil=true actual_unmap=true no_body_textkit=true formal=false"
    );
    #[cfg(feature = "native-paged-body-probe")]
    source_body::run(mtm)?;
    Ok(())
}

fn small_text(document: &RefCell<MarkdownDocumentBuffer>) -> Result<String, String> {
    let document = document.borrow();
    let length = document.utf16_len();
    if length > 64 {
        return Err("组件oracle仅允许64个UTF16单元".into());
    }
    let mut units = [0; 64];
    document
        .copy_utf16_code_units(0, &mut units[..length])
        .map_err(|error| error.to_string())?;
    String::from_utf16(&units[..length]).map_err(|error| error.to_string())
}
