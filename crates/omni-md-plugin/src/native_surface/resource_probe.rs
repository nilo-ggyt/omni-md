//! 真实 Host 已挂载笔记上的九项资源场景；这里只观察原组合根，不实现替代 Host 或生命周期。

use super::*;
use crate::native_surface::paged_body::NativeMarkdownPagedBody;
use crate::native_surface::paged_text_view::{
    NativeMarkdownPagedTextView, NativeMarkdownPagedTextViewDelegate,
};
use core_foundation::runloop::{CFRunLoop, kCFRunLoopDefaultMode};
use objc2_app_kit::NSWindow;
use omni_plugin_sdk::PluginCloseIntent;
use std::io::{Read, Seek, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// 各场景保留原客户端、关闭及冲突前提；Host 为每案重新正式激活独立 A.md。
pub enum NativeMarkdownHostedResourceCase {
    /// 未提交的真实搜索字段仍拥有共享 editor；窗口关闭必须先辞任再退出到 Launcher。
    UnsubmittedSearch,
    /// 已提交工作区查询须在关闭时真正取消其后台扫描和旧结果权限。
    SubmittedSearch,
    /// Find 客户正在编辑，关闭后字段、共享 editor 和字段历史都不得跨会话存活。
    FindField,
    /// 同一事件先后输入两个 Replace 客户，第一字段的历史不能接管第二字段。
    ReplaceFields,
    /// 单格原始草稿仅在关闭提交一次；此前的局部历史不许提前改动父正文。
    CellDraft,
    /// 已接受的笔记交接尚未装配后继时关闭，旧图与在途交接均须纳入同一释放事务。
    PendingNoteClose,
    /// 正文有未保存字符且磁盘已外改：保存拒绝不能关闭或丢弃仍可编辑的草稿。
    BodyConflict,
    /// 同样的磁盘冲突发生在格草稿，预检允许一次真实 canonical 提交但不允许丢用户内容。
    CellConflict,
    /// 两个真实事件分别编辑 Replace 客户，并在转客户前实际往返本地 Undo/Redo。
    SequentialReplaceFields,
}
use NativeMarkdownHostedResourceCase as Case;

/// 原 Host 独占窗口、文稿和会话。观察者仅保存弱身份、有限收据及独立匿名基线 FD；
/// 实际 finish_close、Launcher 恢复和关闭请求匹配继续由生产 Host 验证。
pub struct NativeMarkdownHostedResourceProbe {
    // 路径只限定 Host 预声明的独立合成笔记；不会扫描或清理用户库。
    path: PathBuf,
    case: Case,
    // 同一路径重新打开也不能继承旧验收；两个 Weak 分别约束 canonical 根和实际分页 owner。
    document: WeakMarkdownDocument,
    body: std::rc::Weak<NativeMarkdownPagedBody>,
    // 控制器和窗口只用原生弱身份；不因等待下一事件延长真实 Native 图的生命周期。
    controller: Weak<NativeMarkdownController>,
    window: Weak<NativeMarkdownPanel>,
    view: Weak<NativeMarkdownPagedTextView>,
    // 这个匿名 FD 仅保存独立测试基线，按8 KiB流式还原外改反例。
    // 它不借产品 Source/Reader，也不纳入产品释放角色；观察者 Drop 时由 File 真正关闭。
    baseline: RefCell<std::fs::File>,
    // 摘要只在实际草稿提交后更新，用来核已经保存的字节；从不代替 Host 关闭回执。
    expected_saved: Cell<omni_markdown_core::MarkdownDocumentRevision>,
    // 首图收据补充检查原图弱身份；后建字段、child和工作线程仍由真实产品完整 close门负责。
    owned: NativeMarkdownOwnedSnapshot,
    backing: NativeMarkdownOwnedSnapshot,
    // child只保存共享等待器证明的原域 Weak，后继同坐标重新激活的格不能补签旧输入。
    child: RefCell<Option<std::rc::Weak<NativeMarkdownPagedBody>>>,
    input_started_close: Cell<bool>,
    field_input_step: Cell<usize>,
    // 最后一个真实字段的历史事实跨事件保留；任何 Native 调用前先复制并结束槽借用。
    field_history: RefCell<Option<ResourceFieldHistoryObservation>>,
}

#[derive(Clone)]
/// 字段历史仅弱观察当前实际 field editor；不把共享编辑器或 Undo manager 保留到关闭之后。
struct ResourceFieldHistoryObservation {
    field: Weak<NSTextField>,
    editor: Weak<NSTextView>,
    manager: Option<Weak<objc2_foundation::NSUndoManager>>,
    owned_at_input: bool,
}

impl NativeMarkdownHostedResourceProbe {
    /// 只认 Host 已正式打开的确切路径和当前整 CPU 帧，不创建 workspace、surface 或 Native 图。
    pub fn observe_active_document(path: &Path, case: Case) -> Result<Self, String> {
        MainThreadMarker::new().ok_or("资源验收必须在 AppKit 主线程执行")?;
        let path = path.canonicalize().map_err(|_| "无法解析合成资源笔记")?;
        // 激活返回并不表示首帧已发布。先固定实际原 Doc/Body/Input，随后每个真实事件只核这份票。
        let (document, body, input) = with_active_native_markdown_surface(|surface| {
            let editor = surface
                .editor
                .as_ref()
                .ok_or("资源验收缺少真实 Host 正文")?;
            if editor.document().borrow().path() != path {
                return Err("资源验收不是预声明的合成笔记".into());
            }
            Ok((
                Rc::downgrade(editor.document()),
                Rc::downgrade(&editor.body),
                editor
                    .text_view
                    .input_state()
                    .map_err(|cause| cause.to_string())?,
            ))
        })?;
        // 原三秒包括首次资源准备；事件未完成始终失败，不以队列轮数或时间流逝推成功。
        let started = Instant::now();
        loop {
            let ready = with_active_native_markdown_surface(|surface| {
                let editor = surface
                    .editor
                    .as_ref()
                    .ok_or("首帧完成前原资源正文已结束")?;
                if !std::rc::Weak::ptr_eq(&document, &Rc::downgrade(editor.document()))
                    || !std::rc::Weak::ptr_eq(&body, &Rc::downgrade(&editor.body))
                    || editor
                        .text_view
                        .input_state()
                        .map_err(|cause| cause.to_string())?
                        != input
                {
                    return Err("首帧等待中原文稿或完整输入已经改变".into());
                }
                if let Some(cause) = editor.body.take_failure() {
                    return Err(cause.to_string());
                }
                let key = match editor.text_view.capture_ready_scene_key() {
                    Ok(key) => key,
                    Err(crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending | crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Busy) => return Ok(false),
                    Err(cause) => return Err(cause.to_string()),
                };
                Ok(editor
                    .text_view
                    .probe_cpu_published_frame()
                    .is_some_and(|(cpu, paints)| {
                        cpu.same(&key) && editor.text_view.cpu_paints() == Some(paints)
                    })
                    && editor.text_view.cpu_paint_pending() == Some(false))
            })?;
            if ready {
                // 本轮 Native 查询也计入原期限；恰在超时之后就绪不能反签为三秒内完成。
                if started.elapsed() > Duration::from_secs(3) {
                    return Err("资源原完整 CPU 帧超过三秒期限".into());
                }
                break;
            }
            // 每轮沿原 started 扣除真实耗时，不因收到无关事件重新获得三秒。
            let remaining = Duration::from_secs(3)
                .checked_sub(started.elapsed())
                .ok_or("资源原首屏三秒内没有完整 CPU 帧")?;
            // Active 租约及 Native claim 已全部结束；原安全封装只消费同次剩余期限的真实事件。
            // SAFETY: CoreFoundation 导出的默认 mode 为进程期不可变 CFString；只借其原静态值。
            let mode = unsafe { kCFRunLoopDefaultMode };
            CFRunLoop::run_in_mode(mode, remaining, true);
        }
        with_active_native_markdown_surface(|surface| {
            let editor = surface
                .editor
                .as_ref()
                .ok_or("资源验收缺少真实 Host 正文")?;
            if !std::rc::Weak::ptr_eq(&document, &Rc::downgrade(editor.document()))
                || !std::rc::Weak::ptr_eq(&body, &Rc::downgrade(&editor.body))
                || editor
                    .text_view
                    .input_state()
                    .map_err(|cause| cause.to_string())?
                    != input
                || editor.document().borrow().path() != path
                || editor.presentation_mode() != MarkdownPresentationMode::InlinePreview
            {
                return Err("资源验收必须从指定笔记的真实预览开始".into());
            }
            let key = editor
                .text_view
                .capture_ready_scene_key()
                .map_err(|cause| cause.to_string())?;
            let (cpu, _) = editor
                .text_view
                .probe_cpu_published_frame()
                .ok_or("资源验收缺少实际 CPU 帧")?;
            if !cpu.same(&key) || editor.text_view.cpu_paint_pending() != Some(false) {
                return Err("资源验收不能把 Ready 或旧 CPU 帧当作首屏完成".into());
            }
            let mut baseline = tempfile::tempfile().map_err(|_| "无法建立资源场景独立基线 FD")?;
            editor
                .document()
                .borrow()
                .write_to(&mut baseline)
                .map_err(|cause| cause.to_string())?;
            let revision = editor
                .document()
                .borrow()
                .current_revision()
                .map_err(|cause| cause.to_string())?;
            Ok(Self {
                path: path.clone(),
                case,
                document: Rc::downgrade(editor.document()),
                body: Rc::downgrade(&editor.body),
                controller: Weak::from_retained(&surface.controller),
                window: Weak::from_retained(&surface.panel),
                view: Weak::from(&**editor.text_view),
                baseline: RefCell::new(baseline),
                expected_saved: Cell::new(revision),
                owned: editor.owned_ledger.snapshot(),
                backing: editor.document_resources.clone(),
                child: RefCell::new(None),
                input_started_close: Cell::new(false),
                field_input_step: Cell::new(0),
                field_history: RefCell::new(None),
            })
        })
    }

    /// 每次 Native 操作仍核原弱身份；同路径重新打开或同地址重用都不能接管旧场景。
    fn owns(&self, surface: &NativeMarkdownWindowSurface) -> bool {
        surface.editor.as_ref().is_some_and(|editor| {
            std::rc::Weak::ptr_eq(&self.document, &Rc::downgrade(editor.document()))
                && std::rc::Weak::ptr_eq(&self.body, &Rc::downgrade(&editor.body))
                && self
                    .controller
                    .load()
                    .is_some_and(|owner| std::ptr::eq(&*owner, &*surface.controller))
                && self
                    .window
                    .load()
                    .is_some_and(|window| std::ptr::eq(&*window, &*surface.panel))
                && native_markdown_controller_has_foreground_authority(&surface.controller)
        })
    }

    /// 配置动作只借出原控件；真正激活格时退还 Active 租约后才等待原 child CPU。
    pub fn configure(&self) -> Result<(), String> {
        match self.case {
            Case::UnsubmittedSearch | Case::SubmittedSearch => {
                let field = with_active_native_markdown_surface(|surface| {
                    if !self.owns(surface) {
                        return Err("搜索场景原窗口已经撤销".into());
                    }
                    search_field(surface)
                })?;
                // SAFETY: 活主线程字段使用原公开 AppKit 焦点入口，不替换 delegate 或 field editor。
                unsafe {
                    field.selectText(None);
                }
                Ok(())
            }
            Case::FindField | Case::ReplaceFields | Case::SequentialReplaceFields => {
                with_active_native_markdown_surface(|surface| {
                    self.owns(surface)
                        .then_some(())
                        .ok_or("Find 场景原窗口已经撤销".into())
                })?;
                perform_native_markdown_workspace_action(if self.case == Case::FindField {
                    7
                } else {
                    8
                })
            }
            Case::CellDraft | Case::CellConflict => self.open_cell(),
            // 交接和冲突必须在后继输入事件实际触发，不能在构造时伪造 pending 或 dirty。
            Case::PendingNoteClose | Case::BodyConflict => Ok(()),
        }
    }

    fn open_cell(&self) -> Result<(), String> {
        let started = Instant::now();
        // 原资源回归先保真实 Multi/未知方向；格内激活必须明确收拢，不能借历史 memento 猜 head。
        show_native_markdown_find_bar(false)?;
        let bar = with_active_native_markdown_surface(|surface| {
            if !self.owns(surface) {
                return Err("资源格多选原票已失效".into());
            }
            surface
                .editor
                .as_ref()
                .expect("原身份已核")
                .find_bar
                .borrow()
                .clone()
                .ok_or("资源格缺少实际 Find 栏".into())
        })?;
        bar.fields()[0].setStringValue(ns_string!("#10"));
        if !matches!(
            perform_native_markdown_find_action(MarkdownFindAction::SelectAll)?,
            NativeMarkdownFindExecution::Complete
        ) {
            return Err("固定资源格四处 Find 多选未同步完成原事务".into());
        }
        let input = with_active_native_markdown_surface(|surface| {
            if !self.owns(surface) {
                return Err("资源格多选发布后原窗口已撤销".into());
            }
            let input = surface
                .editor
                .as_ref()
                .expect("原身份已核")
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?;
            if input.selection_is_directed || input.selection_range_count != 4 {
                return Err("资源格前置 Find 没有保留四处真实 Multi/未知 head".into());
            }
            Ok(input)
        })?;
        perform_native_markdown_find_action(MarkdownFindAction::Hide)?;
        drop(bar); // 栏仍归正式会话，测试不将 Native 控件保留到后继量子或关闭。
        loop {
            let ready = with_active_native_markdown_surface(|surface| {
                if !self.owns(surface) {
                    return Err("资源格多选后原窗口已经撤销".into());
                }
                let editor = surface.editor.as_ref().expect("原身份已核");
                if editor
                    .text_view
                    .input_state()
                    .map_err(|cause| cause.to_string())?
                    != input
                {
                    return Err("资源格多选在准备中被后继输入替代".into());
                }
                if let Some(cause) = editor.body.take_failure() {
                    return Err(cause.to_string());
                }
                let key = match editor.text_view.capture_ready_scene_key() {
                    Ok(key) => key,
                    Err(crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Pending | crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError::Busy) => return Ok(false),
                    Err(cause) => return Err(cause.to_string()),
                };
                Ok({
                    editor
                        .text_view
                        .probe_cpu_published_frame()
                        .is_some_and(|(cpu, paints)| {
                            cpu.same(&key) && editor.text_view.cpu_paints() == Some(paints)
                        })
                        && editor.text_view.cpu_paint_pending() == Some(false)
                })
            })?;
            if ready {
                // 本轮 Native 查询也计入原期限；恰在超时之后就绪不能反签为三秒内完成。
                if started.elapsed() > Duration::from_secs(3) {
                    return Err("资源原完整 CPU 帧超过三秒期限".into());
                }
                break;
            }
            // 每轮沿原 started 扣除真实耗时，不因收到无关事件重新获得三秒。
            let remaining = Duration::from_secs(3)
                .checked_sub(started.elapsed())
                .ok_or("资源格原多选首帧三秒内未完成")?;
            // 全部 Active/字段/Scene 借用已结束；安全封装接真实 Duration，不重新开启三秒。
            // SAFETY: CoreFoundation 导出的默认 mode 为进程期不可变 CFString；只借其原静态值。
            let mode = unsafe { kCFRunLoopDefaultMode };
            CFRunLoop::run_in_mode(mode, remaining, true);
        }
        let (view, point, number) = with_active_native_markdown_surface(|surface| {
            if !self.owns(surface) {
                return Err("资源格场景原窗口已经撤销".into());
            }
            let editor = surface.editor.as_ref().expect("原正文身份已核");
            let position = editor
                .document()
                .borrow()
                .find_utf16("#101", 0, false, true)
                .map_err(|cause| cause.to_string())?
                .ok_or("固定夹具没有首个真实内容格")?
                .start;
            let key = editor
                .text_view
                .capture_ready_scene_key()
                .map_err(|cause| cause.to_string())?;
            let scene =
                NativeMarkdownPagedTextViewDelegate::ready_scene(&*editor.body, &key.request)
                    .map_err(|cause| cause.to_string())?;
            let hit = scene
                .first_rect(NSRange::new(position, 1))
                .map_err(|cause| cause.to_string())?;
            let point = editor.text_view.convertPoint_toView(
                NSPoint::new(
                    hit.rect.origin.x + hit.rect.size.width / 2.0,
                    hit.rect.origin.y + hit.rect.size.height / 2.0,
                ),
                None,
            );
            if !editor
                .text_view
                .capture_ready_scene_key()
                .is_ok_and(|value| value.same(&key))
            {
                return Err("资源格真实几何在鼠标动作前已撤销".into());
            }
            Ok((
                editor.text_view.clone(),
                point,
                surface.panel.windowNumber(),
            ))
        })?;
        // 只解释这一对真实公开 NSResponder 事件；待首帧时不保存或重播 NSEvent。
        // Up仍沿原命令结束手势，后继Source量子只完成已接纳的格位置意图。
        for event_type in [
            objc2_app_kit::NSEventType::LeftMouseDown,
            objc2_app_kit::NSEventType::LeftMouseUp,
        ] {
            // SAFETY: 单次合成事件精确属于原真实窗口；不注入系统输入源或用户设备事件。
            let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                event_type, point, NSEventModifierFlags::empty(), 1.0, number, None, 1, 1, 1.0,
            ).ok_or("无法构造原资源格鼠标事件")?;
            // SAFETY: 同一实际主线程 NSResponder、原窗口位置及活事件，只调用对应公开鼠标 selector 一次。
            unsafe {
                if event_type == objc2_app_kit::NSEventType::LeftMouseDown {
                    let _: () = msg_send![&**view, mouseDown: &*event];
                } else {
                    let _: () = msg_send![&**view, mouseUp: &*event];
                }
            }
        }
        let input = view.input_state().map_err(|cause| cause.to_string())?;
        drop(view); // 动作后态来自本次真实 Input；不保 Native claim 跨 runloop。
        let child = super::probe::wait_for_paged_cell(
            self.body.clone(),
            self.document.clone(),
            input,
            None,
            started,
        )?;
        self.child.replace(Some(child));
        Ok(())
    }

    /// 合成文字只交真正的 NSTextInputClient，不读取或替换整篇正文。
    pub fn input(&self) -> Result<(), String> {
        match self.case {
            Case::UnsubmittedSearch | Case::SubmittedSearch => {
                let (field, controller) = with_active_native_markdown_surface(|surface| {
                    if !self.owns(surface) {
                        return Err("资源搜索输入原票已失效".into());
                    }
                    Ok((search_field(surface)?, surface.controller.clone()))
                })?;
                self.field_history.replace(Some(input_actual_field(
                    &field,
                    &controller,
                    "Probe资源",
                )?));
                if self.case == Case::SubmittedSearch {
                    // SAFETY: 原 controller 的公开 selector 使用同一个实际字段，提交真实可取消查询。
                    unsafe {
                        let _: () = msg_send![&*controller, searchMarkdownWorkspace: &*field];
                    }
                }
                Ok(())
            }
            Case::FindField | Case::ReplaceFields | Case::SequentialReplaceFields => {
                let (fields, controller) = with_active_native_markdown_surface(|surface| {
                    if !self.owns(surface) {
                        return Err("资源 Find 输入原票已失效".into());
                    }
                    let editor = surface.editor.as_ref().expect("原正文身份已核");
                    editor.capture_find_resources()?;
                    let fields = editor
                        .find_resources
                        .borrow()
                        .fields()
                        .into_iter()
                        .filter(|field| field.isEditable() && !field.isHidden())
                        .collect::<Vec<_>>();
                    Ok((fields, surface.controller.clone()))
                })?;
                let required = if self.case == Case::FindField { 1 } else { 2 };
                if fields.len() < required {
                    return Err("实际 Find/Replace 输入字段不完整".into());
                }
                // 同事件两字段与分事件两字段各保原边界；不能用多一次消息循环掩盖共享 editor 归属。
                let fields = if self.case == Case::SequentialReplaceFields {
                    let step = self.field_input_step.get();
                    if step > 1 {
                        return Err("分事件 Replace 输入超出原两步".into());
                    }
                    if step == 1 {
                        let original = self.field_history.borrow().clone();
                        verify_focused_field_history(original.as_ref())?;
                    }
                    self.field_input_step.set(step + 1);
                    vec![fields.into_iter().nth(step).ok_or("缺少原 Replace 字段")?]
                } else {
                    fields
                };
                for field in fields {
                    // SAFETY: 只向当前原窗口拥有的实际字段交付标准聚焦和输入。
                    unsafe {
                        field.selectText(None);
                    }
                    self.field_history.replace(Some(input_actual_field(
                        &field,
                        &controller,
                        "Probe查找",
                    )?));
                }
                Ok(())
            }
            Case::CellDraft | Case::CellConflict => {
                // 原 child 已实际获得焦点及完整像素；这里只升级该 Weak，不寻找当前任意格续权。
                let child = self
                    .child
                    .borrow()
                    .as_ref()
                    .and_then(std::rc::Weak::upgrade)
                    .ok_or("原资源格尚未激活")?;
                let view = child.view().map_err(|cause| cause.to_string())?;
                view.command(sel!(selectAll:))
                    .map_err(|cause| cause.to_string())?;
                let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**view);
                // SAFETY: 仅用固定有限原始格草稿覆盖真实 local selection；父 canonical 仍不改变。
                unsafe {
                    client.insertText_replacementRange(
                        ns_string!("resource-cell-draft"),
                        NSRange::new(NSNotFound as usize, 0),
                    );
                }
                view.command(sel!(undo:))
                    .map_err(|cause| cause.to_string())?;
                view.command(sel!(redo:))
                    .map_err(|cause| cause.to_string())?;
                let facts = view
                    .cell_history_facts()
                    .map_err(|cause| cause.to_string())?
                    .ok_or("资源格输入没有真实 local history")?;
                let mut actual = NSRange::new(NSNotFound as usize, 0);
                // 只请求独立固定的19个ASCII units；不得以整格/整篇字符串代替实际输入协议结果。
                let expected = ns_string!("resource-cell-draft");
                let range = NSRange::new(0, expected.length());
                // SAFETY: 原实际客户仍由当前 child 拥有，out 参数有效，有限范围不越固定期望。
                let text = unsafe {
                    client.attributedSubstringForProposedRange_actualRange(range, &mut actual)
                }
                .ok_or("资源格没有交付真实有限草稿")?;
                if !facts.can_undo
                    || actual != range
                    || !text.string().isEqualToString(expected)
                    || view
                        .input_state()
                        .map_err(|cause| cause.to_string())?
                        .utf16_len
                        != range.length
                {
                    return Err("资源格 local 输入或 Undo/Redo 没有恢复固定原草稿".into());
                }
                let parent = self
                    .document
                    .upgrade()
                    .ok_or("资源格编辑期间原正文已释放")?;
                if parent
                    .borrow()
                    .current_revision()
                    .map_err(|cause| cause.to_string())?
                    != self.expected_saved.get()
                {
                    return Err("格内 local 输入或历史提前改变了父 canonical 字节".into());
                }
                Ok(())
            }
            Case::PendingNoteClose => {
                let index = with_active_native_markdown_surface(|surface| {
                    if !self.owns(surface) {
                        return Err("旧笔记交接原窗口已撤销".into());
                    }
                    surface
                        .sidebar_rows
                        .iter()
                        .position(|row| row.relative_path == "B.md")
                        .ok_or("原后继 B.md 不存在".into())
                })?;
                // 采用产品侧栏的保存→旧图归还→目标挂载事务，不能直接写 pending 字段。
                // 随后的关闭属于同一原事件，必须接管真实在途交接，不能等它自然完成后冒充覆盖。
                activate_native_markdown_sidebar_row(index)?;
                if !ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
                    active
                        .as_ref()
                        .is_some_and(|surface| surface.pending_note_switch.is_some())
                }) {
                    return Err("旧笔记真实交接没有进入 pending，不能冒充普通关闭".into());
                }
                self.request_close()?;
                self.input_started_close.set(true);
                Ok(())
            }
            Case::BodyConflict => {
                let view = with_active_native_markdown_surface(|surface| {
                    if !self.owns(surface) {
                        return Err("正文冲突原窗口已撤销".into());
                    }
                    Ok(surface
                        .editor
                        .as_ref()
                        .expect("原身份已核")
                        .text_view
                        .clone())
                })?;
                let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**view);
                // SAFETY: 固定小串插在合法 BOF，原 source/历史由正式 Input 事务维护。
                unsafe {
                    client.insertText_replacementRange(
                        ns_string!("resource-unsaved-draft\n"),
                        NSRange::new(0, 0),
                    );
                }
                Ok(())
            }
        }
    }

    pub fn more_input_events(&self) -> bool {
        self.case == Case::SequentialReplaceFields && self.field_input_step.get() == 1
    }
    pub fn input_started_close(&self) -> bool {
        self.input_started_close.get()
    }

    /// 冲突必须先被真实保存预检拒绝且保留原草稿；恢复独立 FD 基线后才另行显式请求关闭。
    pub fn request_close(&self) -> Result<(), String> {
        if matches!(
            self.case,
            Case::UnsubmittedSearch
                | Case::SubmittedSearch
                | Case::FindField
                | Case::ReplaceFields
                | Case::SequentialReplaceFields
        ) {
            let observation = self.field_history.borrow().clone();
            verify_focused_field_history(observation.as_ref())?;
        }
        if matches!(
            self.case,
            Case::ReplaceFields | Case::SequentialReplaceFields
        ) {
            let first = with_active_native_markdown_surface(|surface| {
                if !self.owns(surface) {
                    return Err("Replace 关闭前原窗口已撤销".into());
                }
                surface
                    .editor
                    .as_ref()
                    .expect("原身份已核")
                    .find_resources
                    .borrow()
                    .fields()
                    .into_iter()
                    .next()
                    .ok_or("原 Find 首字段已结束".into())
            })?;
            if first.currentEditor().is_some()
                || !first.stringValue().isEqualToString(ns_string!("Probe查找"))
            {
                return Err("结束首字段在后继 Replace 客户历史中被改动或复活".into());
            }
        }
        let request = PluginCloseRequest {
            intent: PluginCloseIntent::DismissAndResume,
            scope: PluginCloseScope::ForegroundTransition,
        };
        // 两类冲突都发生在真实磁盘基线之后，正文与格客户端分别走原保存前置事务。
        // 若关闭已错误接受，立即保留失败，不再把恢复文件或第二次关闭解释为首次成功。
        if matches!(self.case, Case::BodyConflict | Case::CellConflict) {
            let before = self
                .document
                .upgrade()
                .ok_or("冲突场景原文稿已释放")?
                .borrow()
                .generation();
            std::fs::write(&self.path, "# External resource-probe revision\n")
                .map_err(|cause| cause.to_string())?;
            if request_native_markdown_close(request).is_ok() {
                return Err("真实保存冲突错误地接受了关闭".into());
            }
            with_active_native_markdown_surface(|surface| {
                if !self.owns(surface) {
                    return Err("保存冲突销毁或替换了原业务图".into());
                }
                let editor = surface.editor.as_ref().expect("原身份已核");
                if !editor.text_view.session_is_current()
                    || editor.document().borrow().generation() < before
                    || editor
                        .document()
                        .borrow()
                        .find_utf16(
                            if self.case == Case::CellConflict {
                                "resource-cell-draft"
                            } else {
                                "resource-unsaved-draft"
                            },
                            0,
                            false,
                            true,
                        )
                        .map_err(|cause| cause.to_string())?
                        .is_none()
                {
                    return Err("保存冲突没有保留原可编辑 canonical 草稿".into());
                }
                Ok(())
            })?;
            // 只还原 Host 声明的这个合成文件，独立匿名 FD 分块读取，不截取或复制长正文。
            let mut baseline = self.baseline.borrow_mut();
            baseline.rewind().map_err(|cause| cause.to_string())?;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&self.path)
                .map_err(|cause| cause.to_string())?;
            let mut bytes = [0u8; 8192];
            loop {
                let count = baseline
                    .read(&mut bytes)
                    .map_err(|cause| cause.to_string())?;
                if count == 0 {
                    break;
                }
                file.write_all(&bytes[..count])
                    .map_err(|cause| cause.to_string())?;
            }
            file.sync_all().map_err(|cause| cause.to_string())?;
        }
        // 主动结束真正格草稿后捕本次保存摘要；外部 Host admission 和资源证明仍只走原关闭事务。
        if self.case != Case::PendingNoteClose {
            with_active_native_markdown_surface(|surface| {
                if !self.owns(surface) {
                    return Err("关闭前原资源窗口已撤销".into());
                }
                let editor = surface.editor.as_ref().expect("原身份已核");
                editor.commit_marked_text(false)?;
                self.expected_saved.set(
                    editor
                        .document()
                        .borrow()
                        .current_revision()
                        .map_err(|cause| cause.to_string())?,
                );
                Ok(())
            })?;
        }
        if self.case == Case::UnsubmittedSearch {
            let panel = self.window.load().ok_or("原关闭窗口已释放")?;
            panel.performClose(None); // 只此原窗口动作具有 ExitToLauncher 意图。
            Ok(())
        } else {
            request_native_markdown_close(request)
        }
    }

    /// Host 已收到生产 matching lifecycle 后调用；独立测试 FD 不列为产品泄漏，图弱票必须真实失效。
    pub fn verify_closed(&self) -> Result<(), String> {
        MainThreadMarker::new().ok_or("资源关闭检查必须在 AppKit 主线程执行")?;
        if ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(Option::is_some)
            || CLOSING_NATIVE_MARKDOWN_SURFACE.with_borrow(Option::is_some)
            || FINALIZING_NATIVE_MARKDOWN_CLOSE.with_borrow(Option::is_some)
            || self.document.upgrade().is_some()
            || self.body.upgrade().is_some()
            || self.view.load().is_some()
            || self
                .child
                .borrow()
                .as_ref()
                .is_some_and(|child| child.upgrade().is_some())
        {
            return Err("真实资源场景仍持业务图或原文稿".into());
        }
        // 不能只用父窗口 nil 推测所有业务载荷已经结束。
        // 原初始 Native/映射收据逐项核对；Host另外完成其完整 accepted→finish_close 资源证明。
        self.owned.verify_released()?;
        self.backing.verify_released()?;
        // 只有原图和 mmap 已真正退出后，才独立打开保存文件核内容摘要。
        // 这份只读核验模型在当前同步栈结束，不成为新的产品正文或后台图。
        let saved = MarkdownDocumentBuffer::open(&self.path, 2 * 1024 * 1024)
            .map_err(|cause| cause.to_string())?;
        if saved
            .current_revision()
            .map_err(|cause| cause.to_string())?
            != self.expected_saved.get()
        {
            return Err("真实关闭没有保存本次已接受的 canonical 字节".into());
        }
        Ok(())
    }
}

fn search_field(surface: &NativeMarkdownWindowSurface) -> Result<Retained<NSTextField>, String> {
    surface
        .controller
        .ivars()
        .workspace_controls
        .get()
        .into_iter()
        .flatten()
        .find_map(|control| control.clone().downcast::<NSTextField>().ok())
        .ok_or_else(|| "actual workspace search field is missing".into())
}

fn input_actual_field(
    field: &NSTextField,
    controller: &NativeMarkdownController,
    text: &str,
) -> Result<ResourceFieldHistoryObservation, String> {
    let editor = field
        .currentEditor()
        .and_then(|editor| editor.downcast::<NSTextView>().ok())
        .ok_or("the focused field has no actual native editor")?;
    let input = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*editor);
    let text = NSString::from_str(text);
    // SAFETY: 主线程真实 field editor 实现该公开输入协议，合成字符串在同步调用中保持有效。
    unsafe {
        input.insertText_replacementRange(&text, NSRange::new(NSNotFound as usize, 0));
    }
    if editor.string().length() == 0 {
        return Err("the actual field editor did not accept synthetic input".into());
    }
    // 这是实际输入事务后的历史初始化路径，不是在关闭观测中创建 manager。
    controller.observe_native_field_editor(&editor);
    let observation = ResourceFieldHistoryObservation {
        field: Weak::from(field),
        editor: Weak::from_retained(&editor),
        // 此处属于刚完成的真实输入功能路径；失败诊断只升级此弱引用，不再查询潜在创建式 getter。
        manager: editor.undoManager().as_ref().map(Weak::from_retained),
        owned_at_input: editor
            .downcast_ref::<NativeMarkdownSessionFieldEditor>()
            .is_some(),
    };
    let borrowed = controller.ivars().borrowed.borrow().clone();
    if borrowed.failure().is_some() || controller.ivars().undo.preflight().is_err() {
        // observe 的 Objective-C 回调通道没有 Result；夹具须传播已保存失败，不能把输入接受当成归属证明。
        let panel = field.window();
        return Err(format!(
            "resource field input failed its session ownership checks; {}",
            field_history_failure_diagnostic(Some(&observation), panel.as_deref(), controller)
        ));
    }
    Ok(observation)
}

/// 验证当前真实 field editor 的历史往返；不冒充硬件 Cmd-Z 或系统 IME 验收。
fn verify_focused_field_history(
    observation: Option<&ResourceFieldHistoryObservation>,
) -> Result<(), String> {
    let (panel, controller) = with_active_native_markdown_surface(|surface| {
        Ok((surface.panel.clone(), surface.controller.clone()))
    })?;
    let editor = panel
        .firstResponder()
        .and_then(|responder| {
            responder
                .downcast::<NativeMarkdownSessionFieldEditor>()
                .ok()
        })
        .ok_or_else(|| {
            // 不重新聚焦或改用另一个输入对象重试；保留跨事件真实 responder 失败与原字段弱身份。
            format!(
                "resource field history lost its owned editor responder; {}",
                field_history_failure_diagnostic(observation, Some(&panel), &controller)
            )
        })?;
    controller.observe_native_field_editor(&editor);
    controller.ivars().undo.preflight()?;
    let after = editor.string();
    if after.length() > 1_024 {
        // 合成字段有固定长度；不为测试读取或复制任意大的用户 native 字符串。
        return Err("resource field history exceeds its synthetic text budget".into());
    }
    // NSText.string 可能指向可变 backing string；有限复制期望，避免 undo 同时改变比较基准。
    let after = NSString::from_str(&after.to_string());
    let manager = editor
        .undoManager()
        .ok_or("resource field history has no actual undo manager")?;
    if !manager.canUndo() {
        // 活动阶段禁止通过关闭 Undo 换取清理成功；这应作为功能失败保留。
        return Err("resource field history did not register native input undo".into());
    }
    manager.undo();
    if editor.string().isEqualToString(&after) || !manager.canRedo() {
        return Err("resource field undo did not restore previous editable data".into());
    }
    manager.redo();
    if !editor.string().isEqualToString(&after) {
        return Err("resource field redo did not restore accepted input".into());
    }
    Ok(())
}

/// 失败只报告有限弱角色的公开事实和身份关系；不读出正文、不打印类名、地址、路径或任意错误文本。
fn field_history_failure_diagnostic(
    observation: Option<&ResourceFieldHistoryObservation>,
    panel: Option<&NSWindow>,
    controller: &NativeMarkdownController,
) -> String {
    let field = observation.and_then(|value| value.field.load());
    let editor = observation.and_then(|value| value.editor.load());
    let manager = observation
        .and_then(|value| value.manager.as_ref())
        .and_then(Weak::load);
    let responder = panel.and_then(NSWindow::firstResponder);
    let current_editor = field.as_ref().and_then(|field| field.currentEditor());
    let body = ACTIVE_NATIVE_MARKDOWN_SURFACE.with_borrow(|active| {
        active
            .as_ref()?
            .editor
            .as_ref()
            .map(|editor| editor.text_view.clone())
    });
    let borrowed = controller.ivars().borrowed.borrow().clone();
    // 指针只在本次同步观测中比较活对象身份，从不输出或跨阶段缓存地址。
    let same = |left: *const (), right: *const ()| left == right;
    let responder_is_editor = responder.as_ref().is_some_and(|responder| {
        editor.as_ref().is_some_and(|editor| {
            same(
                std::ptr::from_ref(&**responder).cast(),
                std::ptr::from_ref(&**editor).cast(),
            )
        })
    });
    let responder_is_field = responder.as_ref().is_some_and(|responder| {
        field.as_ref().is_some_and(|field| {
            same(
                std::ptr::from_ref(&**responder).cast(),
                std::ptr::from_ref(&**field).cast(),
            )
        })
    });
    let responder_is_body = responder.as_ref().is_some_and(|responder| {
        body.as_ref().is_some_and(|body| {
            same(
                std::ptr::from_ref(&**responder).cast(),
                std::ptr::from_ref(&**body).cast(),
            )
        })
    });
    let current_is_editor = current_editor.as_ref().is_some_and(|current| {
        editor.as_ref().is_some_and(|editor| {
            same(
                std::ptr::from_ref(&**current).cast(),
                std::ptr::from_ref(&**editor).cast(),
            )
        })
    });
    let field_in_panel = field
        .as_ref()
        .and_then(|field| field.window())
        .is_some_and(|window| panel.is_some_and(|panel| std::ptr::eq(&*window, panel)));
    let editor_in_panel = editor
        .as_ref()
        .and_then(|editor| editor.window())
        .is_some_and(|window| panel.is_some_and(|panel| std::ptr::eq(&*window, panel)));
    format!(
        "input_observed={} input_owned={} field_live={} editor_live={} manager_live={} manager_owned={} responder_present={} responder_recorded_editor={} responder_recorded_field={} responder_body={} current_editor_present={} current_recorded_editor={} current_owned={} field_in_panel={} editor_in_panel={} field_editable={} field_enabled={} field_units={} editor_field={} editor_units={} undo={} redo={} undo_failed={} borrowed_failed={}",
        observation.is_some(),
        observation.is_some_and(|value| value.owned_at_input),
        field.is_some(),
        editor.is_some(),
        manager.is_some(),
        manager
            .as_ref()
            .is_some_and(|manager| { controller.ivars().undo.owns_observed_manager(manager) }),
        responder.is_some(),
        responder_is_editor,
        responder_is_field,
        responder_is_body,
        current_editor.is_some(),
        current_is_editor,
        current_editor.as_ref().is_some_and(|editor| editor
            .downcast_ref::<NativeMarkdownSessionFieldEditor>()
            .is_some()),
        field_in_panel,
        editor_in_panel,
        field.as_ref().is_some_and(|field| field.isEditable()),
        field.as_ref().is_some_and(|field| field.isEnabled()),
        field
            .as_ref()
            .map_or(0, |field| field.stringValue().length()),
        editor.as_ref().is_some_and(|editor| editor.isFieldEditor()),
        editor.as_ref().map_or(0, |editor| editor.string().length()),
        manager.as_ref().is_some_and(|manager| manager.canUndo()),
        manager.as_ref().is_some_and(|manager| manager.canRedo()),
        controller.ivars().undo.preflight().is_err(),
        borrowed.failure().is_some(),
    )
}
