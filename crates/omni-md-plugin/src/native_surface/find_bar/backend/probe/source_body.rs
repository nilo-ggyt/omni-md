//! 原FindBar经真实Body/Composer完成远端定位；不复用PendingPort或有限替身Scene。

use super::*;
use crate::native_surface::paged_body::{
    NativeMarkdownPagedBody, NativeMarkdownPagedBodyColors, NativeMarkdownPagedBodyConfiguration,
    NativeMarkdownPagedBodyPaintObserver, NativeMarkdownPagedBodyPreparationProbe,
    NativeMarkdownPagedBodyPreparationResources, NativeMarkdownPagedBodyProgress,
    NativeMarkdownPagedBodyResourceReceipts,
};
use objc2::{rc::Weak as NativeWeak, runtime::ProtocolObject, sel};
use objc2_app_kit::{NSAutoresizingMaskOptions, NSFont, NSParagraphStyle, NSTextInputClient};
use objc2_core_foundation::{CFRetained, CFRunLoop, kCFRunLoopDefaultMode};
use std::time::{Duration, Instant};

#[path = "source_body/cases.rs"]
mod cases;
#[path = "source_body/cell.rs"]
mod cell;
#[path = "source_body/fixture.rs"]
mod fixture;
#[path = "source_body/frame.rs"]
mod frame;
#[path = "source_body/interleave.rs"]
mod interleave;
#[path = "source_body/metrics.rs"]
mod metrics;
#[path = "source_body/navigation.rs"]
mod navigation;
#[path = "source_body/release.rs"]
mod release;
#[path = "source_body/root_leaf.rs"]
mod root_leaf;
#[path = "source_body/selection.rs"]
mod selection;
#[path = "source_body/table_edge.rs"]
mod table_edge;
use frame::{observe, wait_frame};

const EPOCH: u64 = 83;

struct Rig {
    body: Rc<NativeMarkdownPagedBody>,
    view: Rc<Retained<NativeMarkdownPagedTextView>>,
    panel: Retained<NativeMarkdownPanel>,
    parent: Retained<NSView>,
    scroll: Retained<NSScrollView>,
    bar: Rc<NativeMarkdownFindBar>,
    controller: Retained<NativeMarkdownController>,
    lease: crate::native_surface::probe::ProbeEditingOwnerLease,
    ledger: Rc<NativeMarkdownOwnedLedger>,
    document: std::rc::Weak<RefCell<MarkdownDocumentBuffer>>,
    backing: omni_markdown_core::MarkdownOriginalBackingObserver,
    cell: Option<cell::Original>,
    _file: tempfile::NamedTempFile,
    requests: Vec<Rc<NativeMarkdownPagedFindRevealRequest>>,
    errors: Vec<NativeMarkdownFindExecutionError>,
    receipts: Vec<NativeMarkdownPagedBodyResourceReceipts>,
    retired: Vec<NativeMarkdownPagedBodyPreparationResources>,
}

impl Rig {
    fn new(mtm: MainThreadMarker) -> Result<Self, String> {
        metrics::sample("before_rig")?; // 只查当前进程公开内核字段，不读取Source或Native图。
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        fixture::write(&mut file)?;
        Self::attach_fixture(mtm, file, fixture::UTF16, 1)
    }

    /// 两种固定输入共享同一实际装配与失败清理；没有第二Input、替身Scene或并存的Rig。
    fn attach_fixture(
        mtm: MainThreadMarker,
        file: tempfile::NamedTempFile,
        expected_utf16: usize,
        expected_lines: usize,
    ) -> Result<Self, String> {
        Self::attach_target(mtm, file, expected_utf16, expected_lines, false)
    }

    /// 正文和原Cell会话共用唯一窗口、Input/View/Viewport装配；Cell只额外持原canonical owner。
    fn attach_target(
        mtm: MainThreadMarker,
        file: tempfile::NamedTempFile,
        expected_utf16: usize,
        expected_lines: usize,
        cell: bool,
    ) -> Result<Self, String> {
        Self::attach_mode(
            mtm,
            file,
            expected_utf16,
            expected_lines,
            cell,
            omni_markdown_core::MarkdownPresentationMode::Source,
            cell,
        )
    }

    /// 新RootLeaf控制显式InlinePreview；expected_complete来自固定夹具要求，长文首屏仍只允许真实前缀。
    /// 所有旧Body/Cell构造继续同一Source入口，不用当前progress反算期望或为了测试强迫全文准备。
    fn attach_mode(
        mtm: MainThreadMarker,
        file: tempfile::NamedTempFile,
        expected_utf16: usize,
        expected_lines: usize,
        cell: bool,
        presentation_mode: omni_markdown_core::MarkdownPresentationMode,
        expected_complete: bool,
    ) -> Result<Self, String> {
        let document = Rc::new(RefCell::new(
            MarkdownDocumentBuffer::open(file.path(), 8 * 1024 * 1024)
                .map_err(|cause| cause.to_string())?,
        ));
        if document.borrow().utf16_len() != expected_utf16
            || document.borrow().line_count() != expected_lines
        {
            return Err("真实Body合成文档没有保持本次固定UTF16/逻辑行输入".into());
        }
        let backing = document
            .borrow()
            .original_backing_observer()
            .ok_or("缺少真实原映射")?;
        let weak_document = Rc::downgrade(&document);
        let cell = cell
            .then(|| cell::Original::new(document.clone()))
            .transpose()?;
        let ledger = Rc::new(NativeMarkdownOwnedLedger::default());
        let panel = NativeMarkdownPanel::new(mtm);
        // SAFETY: 本控制保留原Retained到close之后，防止系统额外释放同一claim。
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setContentSize(NSSize::new(720.0, 348.0));
        let controller = NativeMarkdownController::new(&ledger, &panel, mtm);
        let lease = crate::native_surface::probe::ProbeEditingOwnerLease::new(&controller)?;
        // 原controller在Rig内持活，使用正式公开provider路由，真实查询不能退回系统默认字段编辑器。
        panel.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
        let bounds = NSRect::new(NSPoint::ZERO, NSSize::new(720.0, 348.0));
        let parent = NSView::initWithFrame(NSView::alloc(mtm), bounds);
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), bounds);
        parent.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        scroll.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        scroll.setHasVerticalScroller(true);
        parent.addSubview(&scroll);
        panel.setContentView(Some(&parent));
        let bar = NativeMarkdownFindBar::new(&controller, &ledger, Rc::downgrade(&document), mtm)?;
        bar.show(&scroll, true, &controller)?;
        let font = NSFont::monospacedSystemFontOfSize_weight(14.0, 0.0);
        let paragraph = NSParagraphStyle::defaultParagraphStyle();
        let configuration = NativeMarkdownPagedBodyConfiguration {
            presentation_mode,
            epoch: EPOCH,
            maximum_font_bytes: 512 * 1024 * 1024,
            padding: 8.0,
            colors: NativeMarkdownPagedBodyColors {
                background: [1.0; 4],
                active_line: [27.0 / 255.0, 32.0 / 255.0, 42.0 / 255.0, 1.0],
                text: [0.0, 0.0, 0.0, 1.0],
                selection: [0.7, 0.8, 1.0, 1.0],
                caret: [0.0, 0.0, 0.0, 1.0],
            },
        };
        let body = if let Some(cell) = &cell {
            NativeMarkdownPagedBody::attach_cell(
                cell.session.clone(),
                &scroll,
                font.as_ref(),
                &paragraph,
                configuration,
                mtm,
            )
        } else {
            NativeMarkdownPagedBody::attach(
                document.clone(),
                &scroll,
                font.as_ref(),
                &paragraph,
                configuration,
                mtm,
            )
        }
        .map_err(|cause| cause.to_string())?;
        drop(document); // Cell由显式原owner保活，Body由自身保活，不能让局部变量遮住释放缺口。
        let view = body.view().map_err(|cause| cause.to_string())?;
        ledger.include("body-find.panel", &panel)?;
        ledger.include("body-find.controller", &controller)?;
        ledger.include("body-find.parent", &parent)?;
        ledger.include("body-find.scroll", &scroll)?;
        ledger.include("body-find.view", &view)?;
        let first = observe(&body)?;
        panel.orderFrontRegardless();
        panel.makeFirstResponder(Some(&**view)); // 原输入字段正常辞任，不保留隐藏正文或第二输入owner。
        panel.displayIfNeeded();
        let mut rig = Self {
            body,
            view,
            panel,
            parent,
            scroll,
            bar,
            controller,
            lease,
            ledger,
            document: weak_document,
            backing,
            cell,
            _file: file,
            requests: Vec::new(),
            errors: Vec::new(),
            receipts: Vec::new(),
            retired: Vec::new(),
        };
        let first_result = if expected_complete {
            frame::wait_complete_frame("cell_first_frame", &rig.body, first)
        } else {
            wait_frame("first_frame", &rig.body, first)
        };
        if let Err(cause) = first_result.and_then(|_| rig.remember_receipt()) {
            let _ = release::close(rig, false); // 构造失败也先真正拆掉窗口/正文，不留合成控制图。
            return Err(cause);
        }
        Ok(rig)
    }

    fn action(
        &self,
        action: MarkdownFindAction,
    ) -> Result<NativeMarkdownFindExecution, NativeMarkdownFindExecutionError> {
        metrics::sample("before_execute").expect("真实同步动作前内核快照不可缺失");
        let pixels = self
            .view
            .pixel_observer()
            .expect("真实CPU像素观察者必须存在");
        println!(
            "native_body_find synchronous_action={action:?} pixel_stage=before_execute pixel_snapshot={:?} whole_product=false formal=false",
            pixels.snapshot()
        );
        let started = Instant::now(); // 只量本次真实同步动作，不读取正文或为续体重捕Source。
        let result = self.body.execute_paged_find_action(
            &self.bar,
            &self.controller,
            self.controller.find_input_intent(),
            action,
        );
        let elapsed = started.elapsed(); // 先固定原execute耗时，再查询公开内核和原像素原子账。
        let outcome = match &result {
            Ok(NativeMarkdownFindExecution::Pending(_)) => "pending",
            Ok(NativeMarkdownFindExecution::Complete) => "complete",
            Err(NativeMarkdownFindExecutionError::Rejected(_)) => "rejected",
            Err(NativeMarkdownFindExecutionError::CommittedEffect { .. }) => "committed-effect",
        };
        println!(
            "native_body_find synchronous_action={action:?} result={outcome} actual_execute_ms={:.3} whole_product=false formal=false",
            elapsed.as_secs_f64() * 1000.0
        );
        metrics::sample("after_execute").expect("真实同步动作后内核快照不可缺失");
        println!(
            "native_body_find synchronous_action={action:?} pixel_stage=after_execute pixel_snapshot={:?} whole_product=false formal=false",
            pixels.snapshot()
        );
        result
    }

    fn remember_receipt(&mut self) -> Result<(), String> {
        self.receipts.push(
            self.body
                .resource_receipts()
                .map_err(|cause| cause.to_string())?,
        );
        Ok(())
    }

    fn copy<const N: usize>(&self, start: usize) -> Result<[u16; N], String> {
        let document = self.document.upgrade().ok_or("原Find文档已释放")?;
        let mut units = [0; N];
        document
            .borrow()
            .copy_utf16_code_units(start, &mut units)
            .map_err(|cause| cause.to_string())?;
        Ok(units) // 原Doc借用到此结束，调用方随后才可进入Native字段/窗口。
    }
}

pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    // 每个控制在完整资源门后才开始下一个owner lease；没有同时存在的第二Body/Doc。
    let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut rig = Rig::new(mtm)?;
        let result = cases::run(&mut rig);
        let pending = release::hold_target_fixture_pending(&mut rig);
        let retirement = release::close(rig, false)?;
        Ok((retirement, pending.and(result))) // 准备失败仍先拆图和核全部资源，不能提前遗留原Rig。
    })?;
    retirement.verify(false)?;
    result?;
    navigation::run(mtm)?; // 原四MiB完整图已严格释放，才创建独立65,529B导航样本。
    interleave::run(mtm)?;
    table_edge::run(mtm)?; // 巨行边缘真实回退及严格释放完成后，才创建下一份RootLeaf文稿。
    root_leaf::run(mtm)?; // 独立Rig先完成Root语义帧及严格释放，不依赖后继Cell EOF结果。
    cell::run(mtm)?; // 原Body与RootLeaf已归还，才用同装配实测独立Cell Source及大草稿。
    Ok(())
}

/// 独立诊断g+1事务，不能绕过原fullSequence或据此称远端/整产品已经通过。
pub(super) fn run_transactions(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut rig = Rig::new(mtm)?;
        let result = cases::transactions(&mut rig);
        let pending = release::hold_target_fixture_pending(&mut rig);
        let retirement = release::close(rig, false)?;
        Ok((retirement, pending.and(result)))
    })?;
    retirement.verify(false)?;
    result?;
    interleave::run(mtm)?;
    Ok(())
}
