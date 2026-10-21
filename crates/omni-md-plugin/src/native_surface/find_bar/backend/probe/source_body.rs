//! Exercise original FindBar navigation through the actual Body/Composer for remote
//! targets, without a substitute reveal port or limited Scene implementation.

use super::*;
use crate::native_surface::paged_body::{
    NativeMarkdownPagedBody, NativeMarkdownPagedBodyColors, NativeMarkdownPagedBodyConfiguration,
    NativeMarkdownPagedBodyPaintObserver, NativeMarkdownPagedBodyProgress,
    NativeMarkdownPagedBodyResourceReceipts,
};
use crate::native_surface::text_attributes::native_markdown_document_fixed_font;
use objc2::{rc::Weak as NativeWeak, runtime::ProtocolObject, sel};
use objc2_app_kit::{NSAutoresizingMaskOptions, NSParagraphStyle, NSTextInputClient};
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

/// Publish a recoverable identity before creating this control's private system
/// resource. A watchdog can release only this exact name after an aborted child;
/// neither this process nor its cleanup owner reads the user's general board.
fn private_pasteboard() -> Result<Retained<NSPasteboard>, String> {
    use std::io::Write;

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|cause| cause.to_string())?
        .as_nanos();
    let name = format!("omni-md-find-{}-{timestamp}", std::process::id());
    // Flush the name before the first Native allocation. An output failure must
    // leave no anonymous board whose ownership disappears with the test process.
    let mut output = std::io::stdout().lock();
    writeln!(output, "native_body_find private_pasteboard_name={name}")
        .and_then(|()| output.flush())
        .map_err(|cause| cause.to_string())?;
    drop(output); // Never retain the output lock across a reentrant Native call.
    Ok(NSPasteboard::pasteboardWithName(&NSString::from_str(&name)))
}

/// One synthetic fixture owns its actual window, Body, Input/view, and observations.
/// Weak document identity prevents this harness from concealing document retention;
/// explicit close verifies resources before the owned temporary file is removed.
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
    receipts: Vec<NativeMarkdownPagedBodyResourceReceipts>,
}

impl Rig {
    fn new(mtm: MainThreadMarker) -> Result<Self, String> {
        metrics::sample("before_rig")?; // Read this process's public kernel fields, never Source or the Native graph.
        let mut file = tempfile::NamedTempFile::new().map_err(|cause| cause.to_string())?;
        fixture::write(&mut file)?;
        Self::attach_fixture(mtm, file, fixture::UTF16, 1)
    }

    /// Fixed input variants share actual assembly and failure cleanup, without a
    /// second Input, substitute Scene, or simultaneously resident Rig.
    fn attach_fixture(
        mtm: MainThreadMarker,
        file: tempfile::NamedTempFile,
        expected_utf16: usize,
        expected_lines: usize,
    ) -> Result<Self, String> {
        Self::attach_target(mtm, file, expected_utf16, expected_lines, false)
    }

    /// Body and actual cell sessions share one window/Input/view/viewport assembly;
    /// the cell alone additionally retains its original canonical owner.
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

    /// RootLeaf explicitly requests InlinePreview. Fixture requirements determine
    /// expected completeness; a long document's first frame still permits only a
    /// genuine prefix. Body/cell construction uses the same Source entry rather
    /// than deriving expectations from current progress or forcing full preparation.
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
        // SAFETY: This control retains its own panel claim through close; AppKit
        // must not additionally release the same claim on closure.
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setContentSize(NSSize::new(720.0, 348.0));
        let controller = NativeMarkdownController::new(&ledger, &panel, mtm);
        let lease = crate::native_surface::probe::ProbeEditingOwnerLease::new(&controller)?;
        // The Rig keeps its controller alive and uses the production provider
        // route; query input must not fall back to a system-default field editor.
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
        // Use the production document-font role factory, never a UI fallback
        // that would invalidate font and geometry comparisons.
        let font = native_markdown_document_fixed_font(14.0).map_err(|cause| cause.to_string())?;
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
        // Attach the actual cell session when requested; otherwise the Body owns
        // this same canonical document. Both paths share the Native resource graph.
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
        // Leave lifetime responsibility with the cell/Body owner; a local strong
        // document reference must not hide a production release defect.
        drop(document);
        let view = body.view().map_err(|cause| cause.to_string())?;
        ledger.include("body-find.panel", &panel)?;
        ledger.include("body-find.controller", &controller)?;
        ledger.include("body-find.parent", &parent)?;
        ledger.include("body-find.scroll", &scroll)?;
        ledger.include("body-find.view", &view)?;
        let first = observe(&body)?;
        panel.orderFrontRegardless();
        panel.makeFirstResponder(Some(&**view)); // Resign the previous field normally; retain no hidden body or second Input owner.
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
            receipts: Vec::new(),
        };
        let first_result = if expected_complete {
            frame::wait_complete_frame("cell_first_frame", &rig.body, first)
        } else {
            wait_frame("first_frame", &rig.body, first)
        };
        if let Err(cause) = first_result.and_then(|_| rig.remember_receipt()) {
            // Failed first-frame preparation must still dismantle the actual window/Body.
            let _ = release::close(rig);
            return Err(cause);
        }
        Ok(rig)
    }

    /// Measure the actual synchronous action separately from later composer work.
    /// Diagnostics use existing kernel/pixel observers and acquire no new source.
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
        let started = Instant::now(); // Time this action only; no text read or Source recapture for its continuation.
        let result = self.body.execute_paged_find_action(
            &self.bar,
            &self.controller,
            self.controller.find_input_intent(),
            action,
        );
        let elapsed = started.elapsed(); // Fix action duration before sampling kernel fields or existing pixel atomics.
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

    /// Retain the actual Body's existing resource observations for close-time
    /// verification without creating another ownership graph.
    fn remember_receipt(&mut self) -> Result<(), String> {
        self.receipts.push(
            self.body
                .resource_receipts()
                .map_err(|cause| cause.to_string())?,
        );
        Ok(())
    }

    /// Read exactly the caller's fixed synthetic oracle window, never an entire
    /// document. End the document borrow before any later Native operation.
    fn copy<const N: usize>(&self, start: usize) -> Result<[u16; N], String> {
        let document = self.document.upgrade().ok_or("原Find文档已释放")?;
        let mut units = [0; N];
        document
            .borrow()
            .copy_utf16_code_units(start, &mut units)
            .map_err(|cause| cause.to_string())?;
        Ok(units) // The original document borrow ends before the caller accesses Native fields/windows.
    }
}

pub(super) fn run(mtm: MainThreadMarker) -> Result<(), String> {
    // Begin each owner lease only after the preceding control passes full resource
    // release; no second Body/document is resident concurrently.
    let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut rig = Rig::new(mtm)?;
        let result = cases::run(&mut rig);
        let pending = release::hold_target_fixture_pending(&mut rig);
        let retirement = release::close(rig)?;
        Ok((retirement, pending.and(result))) // Preparation failure still dismantles and verifies the original Rig first.
    })?;
    retirement.verify()?;
    result?;
    navigation::run(mtm)?; // Strictly release the 4MiB graph before creating the separate 65,523-byte navigation fixture.
    table_edge::run(mtm)?; // Complete actual huge-row fallback and strict release before the RootLeaf fixture.
    root_leaf::run(mtm)?; // Root semantic frames and release stand independently of later cell EOF results.
    cell::run(mtm)?; // After Body/RootLeaf release, reuse assembly for independent cell Source and large-draft controls.
    Ok(())
}

/// Diagnose generation-plus-one transactions independently. This cannot bypass the
/// full sequence or establish remote-navigation/whole-product acceptance.
pub(super) fn run_transactions(mtm: MainThreadMarker) -> Result<(), String> {
    let (retirement, result) = autoreleasepool(|_| -> Result<_, String> {
        let mut rig = Rig::new(mtm)?;
        let result = cases::transactions(&mut rig);
        let pending = release::hold_target_fixture_pending(&mut rig);
        let retirement = release::close(rig)?;
        Ok((retirement, pending.and(result)))
    })?;
    retirement.verify()?;
    result?;
    Ok(())
}
