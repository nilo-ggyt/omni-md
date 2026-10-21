//! Exercise real link handoff inside the Host's exclusive synthetic vault. Unsaved bytes, original
//! mouse delivery, old-graph release, and the target's complete CPU frame are independent facts.

use super::*;
use objc2::rc::{Retained, Weak as NativeWeak};
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
use omni_markdown_core::MarkdownDocumentRevision;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

const DIRECTORY: &str = "相对链接验收";
const TARGET_NOTE: &str = "相对链接验收/目标笔记.md";
const TARGET_HEADING_PREFIX: &str = "相对链接目标中文😀\n\n";
const TARGET_TEXT: &str =
    "相对链接目标中文😀\n\n## Linked Target\n\n这个目标文件必须保持原字节。\n";
const PREFIX: &str =
    "未保存的中文😀前缀。\n\n[打开目标](相对链接验收/目标笔记.md#linked-target)\n\n";
const LABEL: &str = "打开目标";
const HTTP_LABEL: &str = "打开外部验收页";

/// Compare the canonical stream with fixed target bytes and the optional finite HTTP suffix,
/// without assembling another copy of the target document.
struct TargetComparison<'a> {
    expected: io::Chain<io::Cursor<&'static [u8]>, io::Cursor<&'a [u8]>>,
    length: usize,
    written: usize,
}

impl<'a> TargetComparison<'a> {
    fn new(suffix: Option<&'a str>) -> Self {
        // These independent fixture bytes define the expected file before its production open.
        let suffix = suffix.unwrap_or("").as_bytes();
        Self {
            expected: io::Cursor::new(TARGET_TEXT.as_bytes()).chain(io::Cursor::new(suffix)),
            length: TARGET_TEXT.len() + suffix.len(),
            written: 0,
        }
    }

    fn finish(self) -> Result<(), String> {
        // Matching every received chunk is insufficient if the canonical writer stopped early.
        if self.written != self.length {
            return Err("相对链接目标缺少预声明字节".into());
        }
        Ok(())
    }
}

impl Write for TargetComparison<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut expected = [0_u8; 512];
        // Debit the same expected stream across arbitrary writer chunk boundaries; extra output
        // fails read_exact rather than becoming an accepted tail or a second text allocation.
        for part in bytes.chunks(expected.len()) {
            self.expected.read_exact(&mut expected[..part.len()])?;
            if part != &expected[..part.len()] {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "目标字节不同于预声明夹具",
                ));
            }
            self.written += part.len();
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        // This comparison sink owns no buffered output or descriptor; exact length is settled by finish.
        Ok(())
    }
}

/// Track each file's actual disk identity independently. Saving may replace the source inode;
/// opening or closing the target must leave its original identity unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DiskIdentity {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

fn disk_identity(path: &Path) -> Result<DiskIdentity, String> {
    // Snapshot the real file's identity and timestamp, without reading its contents or adopting
    // those observations as the canonical document's Source authority.
    let metadata = std::fs::metadata(path).map_err(|cause| cause.to_string())?;
    Ok(DiskIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
    })
}

/// Retain observation receipts only. An expired document Weak does not establish release of
/// its fonts, reader, backing mappings, worker, or native objects.
struct Graph {
    document: WeakMarkdownDocument,
    body: std::rc::Weak<NativeMarkdownPagedBody>,
    input: std::rc::Weak<NativeMarkdownPagedInput>,
    native: Vec<(&'static str, NativeWeak<AnyObject>)>,
    ledger: NativeMarkdownOwnedSnapshot,
    backing: omni_markdown_core::MarkdownOriginalBackingObserver,
    receipts: crate::native_surface::paged_body::NativeMarkdownPagedBodyResourceReceipts,
}

impl Graph {
    /// Capture after the original complete CPU frame passes, using its existing resources rather
    /// than creating a reader or reshaping text merely to populate an observation role.
    fn capture(surface: &NativeMarkdownWindowSurface) -> Result<Self, String> {
        let editor = surface.editor.as_ref().ok_or("链接释放观察缺少原正文")?;
        let paged = editor;
        let mut ledger = editor.owned_ledger().snapshot();
        ledger.seal();
        Ok(Self {
            document: Rc::downgrade(editor.document()),
            body: Rc::downgrade(&paged.body),
            input: paged.body.probe_find_input_weak(),
            native: paged
                .body
                .native_weaks()
                .map_err(|cause| cause.to_string())?,
            ledger,
            backing: editor
                .document()
                .borrow()
                .original_backing_observer()
                .ok_or("链接文稿没有原映射")?,
            receipts: paged
                .body
                .resource_receipts()
                .map_err(|cause| cause.to_string())?,
        })
    }

    /// Inspect the same original receipts at handoff and final close. This creates no polling,
    /// threshold-based success, or second release protocol waiting for Weak values to expire.
    fn verify_released(&self) -> Result<(), String> {
        // Canonical ownership and both backing measures must end, with real cleanup failure
        // preserved even if all the Rust weak identities have already expired.
        if self.document.upgrade().is_some()
            || self.body.upgrade().is_some()
            || self.input.upgrade().is_some()
            || self.backing.mapped_bytes() != 0
            || self.backing.managed_resident_bytes() != 0
            || self.backing.failed()
        {
            return Err("相对链接的原Doc/Input/Body或mmap尚未真实释放".into());
        }
        for (role, weak) in &self.native {
            // Each observed native role belongs to this captured graph, not a later same-named view.
            if weak.load().is_some() {
                return Err(format!("相对链接的原Native角色尚未释放：{role}"));
            }
        }
        self.ledger.verify_released()?;
        // A complete frame must have produced an actual reader receipt; absence is not zero usage.
        if !self.receipts.source.payload_released() || self.receipts.reader.is_none() {
            return Err("链接完整帧Source未释放或缺少真实Reader收据".into());
        }
        for observer in self
            .receipts
            .reader
            .iter()
            .chain(std::iter::once(&self.receipts.writer))
        {
            // Reader and writer receipts independently settle descriptors, mappings, scratch,
            // Source payload, and the original error; one released role cannot excuse another.
            let actual = observer.snapshot();
            if actual.file_open
                || actual.mapped_bytes != 0
                || actual.buffer_bytes != 0
                || actual.cleanup_failed
                || actual.reader_failed
                || !actual.source_payload_released
                || actual.first_source_errno.is_some()
            {
                return Err(format!("链接原Reader/Writer未归还：{actual:?}"));
            }
        }
        let worker = self
            .receipts
            .worker
            .snapshot()
            .ok_or("链接worker观察已poison")?;
        if !worker.is_released() {
            return Err(format!("链接原worker未归还：{worker:?}"));
        }
        for (bytes, font) in &self.receipts.fonts {
            // Font payload release and the corresponding byte mapping's successful unmap are
            // separate obligations; neither proves system-wide font-cache destruction.
            if !font.payload_released()
                || !bytes.unmap_succeeded()
                || bytes.mapped_bytes() != 0
                || bytes.failed()
            {
                return Err("链接原Font payload或字节映射未归还".into());
            }
        }
        for cell in &self.receipts.cells {
            // Actual mounted cell graphs retain their own resource receipts after the parent ends.
            cell.verify_released()?;
        }
        Ok(())
    }
}

/// Keep only the installed target's weak identities and complete opening Input. A later document
/// at the same path cannot complete this original handoff.
struct Target {
    document: WeakMarkdownDocument,
    body: std::rc::Weak<NativeMarkdownPagedBody>,
    input: NativeMarkdownPagedInputState,
    /// Bind the navigated state once, separately from installation, after the independent heading
    /// position, original document, and complete CPU publication jointly pass.
    navigated_input: Cell<Option<NativeMarkdownPagedInputState>>,
}

/// Own the original relative-note action and declared target. The optional HTTP stage admits its
/// own single click only after that navigation passes. Release notifications bind only the declared
/// target, never discover or adopt an arbitrary successor document.
pub(in super::super) struct RelativeLink {
    weak: std::rc::Weak<Self>,
    original: MemoryWorkloadFacts,
    source_path: PathBuf,
    target_path: PathBuf,
    original_file: RefCell<Option<std::fs::File>>,
    target_file: RefCell<Option<std::fs::File>>,
    target_disk: DiskIdentity,
    target_revision: MarkdownDocumentRevision,
    edited_input: Cell<Option<NativeMarkdownPagedInputState>>,
    edited_revision: Cell<Option<MarkdownDocumentRevision>>,
    saved_disk: Cell<Option<DiskIdentity>>,
    edited_frame: RefCell<Option<Rc<MemoryFrameObservation>>>,
    old_graph: RefCell<Option<Graph>>,
    target_graph: RefCell<Option<Graph>>,
    window: NativeWeak<NativeMarkdownPanel>,
    controller: NativeWeak<NativeMarkdownController>,
    frame: Rc<SourceFrame>,
    target: RefCell<Option<Target>>,
    original_view_release: RefCell<Option<Rc<NativeMarkdownDeallocationCohort>>>,
    switch_release_waiting: Cell<bool>,
    started: Cell<Option<Instant>>,
    signal: RefCell<Option<Box<dyn FnOnce()>>>,
    failure: RefCell<Option<String>>,
    http_suffix: Option<String>,
    http_input: Cell<Option<NativeMarkdownPagedInputState>>,
}

impl RelativeLink {
    /// The original opening step already verified its first frame. Only the explicit relative-link
    /// flows may create an exclusive target file inside the Host-owned synthetic vault.
    fn begin(
        workload: &NativeMarkdownMemoryWorkload,
        http_url: Option<&str>,
    ) -> Result<Rc<Self>, String> {
        if !matches!(
            workload.flow,
            NativeMarkdownHostedFlow::SourceRelativeLink
                | NativeMarkdownHostedFlow::SourceRelativeLinkHttp
        ) {
            return Err("相对链接文件只允许建在本轮明确的Host合成场景".into());
        }
        if (workload.flow == NativeMarkdownHostedFlow::SourceRelativeLinkHttp) != http_url.is_some()
        {
            return Err("HTTP流程和本轮显式页面身份不一致".into());
        }
        workload.verify_open()?;
        let original = workload.facts()?;
        // create_dir/create_new reject pre-existing fixtures instead of overwriting another
        // owner's data. The Host retains ownership of this exact synthetic directory on failure.
        let vault = workload.path.parent().ok_or("合成原笔记没有库目录")?;
        std::fs::create_dir(vault.join(DIRECTORY)).map_err(|cause| cause.to_string())?;
        let target_path = vault.join(TARGET_NOTE);
        let mut created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target_path)
            .map_err(|cause| cause.to_string())?;
        created
            .write_all(TARGET_TEXT.as_bytes())
            .map_err(|cause| cause.to_string())?;
        let http_suffix = http_url.map(|url| format!("\n[{HTTP_LABEL}]({url})\n"));
        if let Some(suffix) = &http_suffix {
            created
                .write_all(suffix.as_bytes())
                .map_err(|cause| cause.to_string())?;
        } // Seal the complete target now; the later HTTP click must not edit this admitted file.
        created.sync_all().map_err(|cause| cause.to_string())?;
        drop(created);
        let target_disk = disk_identity(&target_path)?;
        // This independent Core open obtains only the fixed short file's disk revision. It creates
        // no native graph and returns its mapping before the actual click. Production opens the
        // target through its own session, without borrowing this model or its Source authority.
        let baseline = MarkdownDocumentBuffer::open(&target_path, 64 * 1024)
            .map_err(|cause| cause.to_string())?;
        let baseline_backing = baseline
            .original_backing_observer()
            .ok_or("目标基线没有mmap")?;
        let target_revision = baseline
            .current_revision()
            .map_err(|cause| cause.to_string())?;
        drop(baseline);
        if baseline_backing.mapped_bytes() != 0
            || baseline_backing.managed_resident_bytes() != 0
            || baseline_backing.failed()
        {
            return Err("目标独立短基线没有在正式打开前归还".into());
        }
        let source_path = workload.path.clone();
        // Independent read-only descriptors preserve both original inodes for later byte checks,
        // even when production saving atomically replaces the source path.
        let original_file = std::fs::File::open(&source_path).map_err(|cause| cause.to_string())?;
        let target_file = std::fs::File::open(&target_path).map_err(|cause| cause.to_string())?;
        let (window, controller) = with_active_native_markdown_surface(|surface| {
            if !workload.current(surface, &original) {
                return Err("相对链接开始时原笔记已变化".into());
            }
            Ok((
                NativeWeak::from_retained(&surface.panel),
                NativeWeak::from_retained(&surface.controller),
            ))
        })?;
        let case = Rc::new_cyclic(|weak| Self {
            weak: weak.clone(),
            original,
            source_path,
            target_path,
            original_file: RefCell::new(Some(original_file)),
            target_file: RefCell::new(Some(target_file)),
            target_disk,
            target_revision,
            edited_input: Cell::new(None),
            edited_revision: Cell::new(None),
            saved_disk: Cell::new(None),
            edited_frame: RefCell::new(None),
            old_graph: RefCell::new(None),
            target_graph: RefCell::new(None),
            window,
            controller,
            frame: Rc::new_cyclic(|weak| SourceFrame {
                weak: weak.clone(),
                body: RefCell::new(std::rc::Weak::new()),
                generation: Cell::new(0),
                mode: Cell::new(MarkdownPresentationMode::InlinePreview),
                // Real fragment selection advances fold publication after installation. Filter
                // only original generation zero and mode here; completion below certifies the
                // exact heading with the same Input/Scene/CPU instead of assuming fold revision zero.
                fold_revision: Cell::new(None),
                failure: RefCell::new(None),
                key: RefCell::new(None),
                signal: RefCell::new(None),
            }),
            target: RefCell::new(None),
            original_view_release: RefCell::new(None),
            switch_release_waiting: Cell::new(false),
            started: Cell::new(None),
            signal: RefCell::new(None),
            failure: RefCell::new(None),
            http_suffix,
            http_input: Cell::new(None),
        });
        case.insert(workload)?;
        Ok(case)
    }

    /// Insert only the fixed small prefix at explicit 0..0, without adopting a later selection.
    /// The expected length and generation increment are fixed before any native input call.
    fn insert(&self, workload: &NativeMarkdownMemoryWorkload) -> Result<(), String> {
        let (view, before) = with_active_native_markdown_surface(|surface| {
            if !workload.current(surface, &self.original)
                || !self.original.opening_epoch.permits(surface)
            {
                return Err("相对链接编辑的原输入或窗口已撤销".into());
            }
            let paged = surface.editor.as_ref().ok_or("原分页正文已关闭")?;
            Ok((
                Rc::clone(&paged.text_view),
                paged
                    .text_view
                    .input_state()
                    .map_err(|cause| cause.to_string())?,
            ))
        })?;
        let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&**view);
        // SAFETY: This client belongs to the verified original synthetic document. One bounded
        // NSString and explicit replacement exercise real insertion, without claiming IME delivery.
        unsafe {
            client.insertText_replacementRange(&NSString::from_str(PREFIX), NSRange::new(0, 0));
        }
        let frame = with_active_native_markdown_surface(|surface| {
            if !memory_workload_identity_current(surface, &self.original)
                || !self.original.opening_epoch.permits(surface)
            {
                return Err("相对链接插字期间原客户已变化".into());
            }
            let paged = surface.editor.as_ref().ok_or("原分页正文已关闭")?;
            let actual = paged
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?;
            // Compare the exact declared edit result before capturing its actual CPU endpoint;
            // a successor generation, marked composition, or different caret cannot substitute.
            if actual.generation != before.generation.checked_add(1).ok_or("原generation耗尽")?
                || actual.utf16_len != before.utf16_len + PREFIX.encode_utf16().count()
                || actual.primary_range != NSRange::new(PREFIX.encode_utf16().count(), 0)
                || actual.marked.is_some()
            {
                return Err("相对链接插字没有形成准确一次预声明后态".into());
            }
            self.edited_input.set(Some(actual));
            MemoryFrameObservation::capture(
                surface,
                &self.original,
                self.original.opening_epoch,
                actual,
            )
        })?;
        *self.edited_frame.borrow_mut() = Some(frame);
        Ok(())
    }

    /// Compare the displayed edit against the independent original descriptor in bounded pieces.
    /// This actual canonical revision is the sole allowed result of the subsequent save.
    fn verify_edit(&self) -> Result<(), String> {
        self.edited_frame
            .borrow()
            .as_ref()
            .ok_or("缺少链接编辑完整帧")?
            .completed()?;
        with_active_native_markdown_surface(|surface| {
            if !memory_workload_identity_current(surface, &self.original)
                || !self.original.opening_epoch.permits(surface)
            {
                return Err("链接编辑帧不属于原笔记".into());
            }
            let editor = surface.editor.as_ref().ok_or("链接原正文已关闭")?;
            let paged = editor;
            if Some(
                paged
                    .text_view
                    .input_state()
                    .map_err(|cause| cause.to_string())?,
            ) != self.edited_input.get()
            {
                return Err("链接编辑帧的完整Input已被替换".into());
            }
            let document = editor.document().borrow();
            // The fixture must still have a real unsaved edit and undo history before navigation;
            // a clean reopened document cannot stand in for saving this operation's dirty source.
            if !document.is_dirty() || !document.can_undo() || document.can_redo() {
                return Err("链接点击之前没有真实未保存编辑及原历史".into());
            }
            {
                let mut original = self.original_file.borrow_mut();
                let mut comparison = SourceComparison {
                    original: original.as_mut().ok_or("源FD已归还")?,
                    replaced: 0..0,
                    inserted: PREFIX.as_bytes(),
                    written: 0,
                    scratch: [0; 64 * 1024],
                };
                document
                    .write_to(&mut comparison)
                    .map_err(|cause| cause.to_string())?;
                if comparison.written != self.original.disk.2 as usize + PREFIX.len() {
                    return Err("链接编辑改变了预声明范围之外的字节长度".into());
                }
            }
            // The independent descriptor borrow ended before any native weak observation or
            // resource capture; retain the measured canonical revision, not another text copy.
            self.edited_revision.set(Some(
                document
                    .current_revision()
                    .map_err(|cause| cause.to_string())?,
            ));
            drop(document);
            *self.old_graph.borrow_mut() = Some(Graph::capture(surface)?);
            Ok(())
        })
    }

    /// Derive an ink point from the original ready Scene and keep its actual window identity.
    /// Deliver mouse events through AppKit rather than invoking note-switch implementation directly.
    fn click(&self) -> Result<(), String> {
        let window = self.window.load().ok_or("链接原窗口已释放")?;
        window.makeKeyWindow();
        let view = with_active_native_markdown_surface(|surface| {
            if !std::ptr::eq(&*surface.panel, &*window)
                || !memory_workload_identity_current(surface, &self.original)
            {
                return Err("链接原窗口或文稿已被替换".into());
            }
            let paged = surface.editor.as_ref().ok_or("链接原正文已关闭")?;
            Ok(Rc::clone(&paged.text_view))
        })?;
        if !window.makeFirstResponder(Some(&**view)) || !window.isKeyWindow() {
            return Err("链接原正文没有取得实际鼠标焦点".into());
        }
        let key = view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        if key.request.input != self.edited_input.get().ok_or("链接尚无编辑后态")? {
            return Err("原链接点击前完整Input已变化".into());
        }
        let start = PREFIX[..PREFIX.find(LABEL).expect("固定标签在前缀内")]
            .encode_utf16()
            .count();
        let point = view
            .screen_rect(NSRange::new(start, 1))
            .map_err(|cause| cause.to_string())?;
        if point.actual_range != NSRange::new(start, 1) || point.rect.size.width <= 0.0 {
            return Err("原链接标签没有精确完整GCB墨迹".into());
        }
        let point = window.convertPointFromScreen(NSPoint::new(
            point.rect.origin.x + point.rect.size.width * 0.25,
            point.rect.origin.y + point.rect.size.height * 0.5,
        ));
        if !view
            .capture_ready_scene_key()
            .is_ok_and(|actual| actual.same(&key))
        {
            return Err("原链接鼠标点转换期间Scene已失效".into());
        }
        // Observe this original view's actual deallocation before clicking. Its associated marker
        // holds atomic observation metadata only, not the view. That event locates the original
        // pending switch after teardown; it cannot independently certify release of the whole graph.
        if self.original_view_release.borrow().is_some() {
            return Err("相对链接原View释放观察重复登记".into());
        }
        let mut released = NativeMarkdownDeallocationCohort::new(window.mtm());
        released.track(&view, window.mtm());
        released.seal();
        *self.original_view_release.borrow_mut() = Some(Rc::new(released));
        if !view
            .capture_ready_scene_key()
            .is_ok_and(|actual| actual.same(&key))
        {
            return Err("原View释放观察登记期间链接Scene已失效".into());
        }
        self.started.set(Some(Instant::now()));
        let application = NSApplication::sharedApplication(window.mtm());
        for kind in [NSEventType::LeftMouseDown, NSEventType::LeftMouseUp] {
            let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                kind, point, NSEventModifierFlags::Command, 1.0, window.windowNumber(), None, 1, 1, 1.0,
            ).ok_or("无法建立本次有限真实鼠标语义")?;
            // Queue each half of this Command-click once with the actual window number and ink
            // point. NSApplication dispatches through the original NSWindow to its view; no direct
            // view selector, note switch, manual event pump, replay, or replacement click is used.
            application.postEvent_atStart(&event, false);
        }
        if let Some(cause) = view.take_failure() {
            return Err(cause.to_string());
        }
        Ok(())
    }

    /// After actual original-view deallocation, observe the same pending switch's complete release
    /// endpoint. Main-queue order does not establish that the target has been installed.
    fn original_view_released(&self) {
        if self.signal.borrow().is_none() {
            return; // Return/close revoked this endpoint; late release cannot bind a successor document.
        }
        let result = (|| {
            if self.accept_installed()? {
                return Ok(()); // Consume an already completed installation and CPU frame without requesting another publication.
            }
            let window = self.window.load().ok_or("相对链接原窗口已释放")?;
            with_active_native_markdown_surface(|surface| {
                if !std::ptr::eq(&*surface.panel, &*window)
                    || std::ptr::from_ref(&*surface.controller).addr() != self.original.owner
                    || !native_markdown_controller_has_foreground_authority(&surface.controller)
                    || self.original.document.upgrade().is_some()
                {
                    return Err("相对链接释放事件不属于原窗口及已拆除文稿".into());
                }
                let pending = surface
                    .pending_note_switch
                    .as_ref()
                    .filter(|pending| pending.relative_path == TARGET_NOTE)
                    .ok_or("相对链接原View释放后没有预声明目标的原切换")?;
                let barrier = pending
                    .release_barrier
                    .as_ref()
                    .ok_or("相对链接原切换没有真实旧图释放屏障")?;
                self.switch_release_waiting.set(true);
                let weak = self.weak.clone();
                let controller =
                    dispatch2::MainThreadBound::new(self.controller.clone(), surface.panel.mtm());
                barrier.probe_when_released(move || {
                    let mtm = MainThreadMarker::new().expect("原释放端点位于主线程");
                    let next = dispatch2::MainThreadBound::new(weak, mtm);
                    // The original relay lets production finish consume this same barrier first.
                    // This one continuation transfers an event that already occurred; it neither
                    // repeatedly enqueues work nor treats release as installation or a complete frame.
                    let Some(controller) = controller.get(mtm).load() else {
                        return;
                    };
                    // Synchronous native reentry may still leave production awaiting restoration of
                    // its original SurfaceLease. Use that owner's accepted continuation for the
                    // actual restoration event, without guessing from main-queue turns.
                    controller.enqueue_owned_after_release(None, move || {
                        let mtm = MainThreadMarker::new().expect("原切换完成后仍在主线程");
                        let Some(case) = next.get(mtm).upgrade() else {
                            return;
                        };
                        case.switch_release_waiting.set(false);
                        if case.signal.borrow().is_none() {
                            return;
                        }
                        match case.accept_installed() {
                            Ok(true) => {}
                            Ok(false) => case.fail("原切换释放完成后没有安装预声明目标".into()),
                            Err(cause) => case.fail(cause),
                        }
                    });
                });
                Ok(())
            })
        })();
        if let Err(cause) = result {
            self.fail(cause);
        }
    }

    fn fail(&self, cause: String) {
        // Preserve the first functional failure; later observations may only deliver its endpoint.
        if self.failure.borrow().is_none() {
            *self.failure.borrow_mut() = Some(cause);
        }
        self.deliver();
    }

    fn deliver(&self) {
        // Remove the one-shot signal before invocation so reentry cannot deliver it twice.
        let signal = self.signal.borrow_mut().take();
        if let Some(signal) = signal {
            signal();
        }
    }

    /// Bind the new document once at actual installation in the original window, with its declared
    /// path and revision. False means not installed; callers may continue only from an existing
    /// result or real release endpoint, never enqueue another speculative observation.
    fn accept_installed(&self) -> Result<bool, String> {
        if self.signal.borrow().is_none() || self.target.borrow().is_some() {
            return Ok(true);
        }
        let window = self.window.load().ok_or("相对链接原窗口已释放")?;
        let installed = with_active_native_markdown_surface(|surface| {
            if !std::ptr::eq(&*surface.panel, &*window)
                || std::ptr::from_ref(&*surface.controller).addr() != self.original.owner
                || !native_markdown_controller_has_foreground_authority(&surface.controller)
            {
                return Err("相对链接安装不属于原窗口".into());
            }
            let Some(editor) = surface.editor.as_ref() else {
                return Ok(None);
            };
            if WeakMarkdownDocument::ptr_eq(
                &Rc::downgrade(editor.document()),
                &self.original.document,
            ) {
                return Ok(None); // Original save/teardown continues; only the original view/barrier release events may resume observation.
            }
            if surface.current_note_relative_path.as_deref() != Some(TARGET_NOTE)
                || surface.pending_note_switch.is_some()
                || self.original.document.upgrade().is_some()
            {
                return Err("相对链接目标安装未满足准确路径及旧Doc释放屏障".into());
            }
            let paged = editor;
            let document = editor.document().borrow();
            // Actual confinement/opening must yield the declared clean target revision with no
            // fabricated edit history. Path equality alone cannot certify this canonical allocation.
            if document
                .path()
                .canonicalize()
                .map_err(|cause| cause.to_string())?
                != self
                    .target_path
                    .canonicalize()
                    .map_err(|cause| cause.to_string())?
                || document.original_revision() != self.target_revision
                || document.is_dirty()
                || document.generation() != 0
                || document.utf16_len()
                    != TARGET_TEXT.encode_utf16().count()
                        + self
                            .http_suffix
                            .as_deref()
                            .unwrap_or("")
                            .encode_utf16()
                            .count()
                || document.can_undo()
                || document.can_redo()
            {
                return Err("相对链接装入了非预声明文件/revision或额外编辑".into());
            }
            drop(document);
            let input = paged
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?;
            if input.marked.is_some()
                || ![
                    NSRange::new(0, 0),
                    NSRange::new(TARGET_HEADING_PREFIX.encode_utf16().count(), 0),
                ]
                .contains(&input.primary_range)
                || paged.body.presentation_mode() != MarkdownPresentationMode::InlinePreview
            {
                return Err("相对链接目标初始输入或模式不同".into());
            }
            Ok(Some((
                Rc::clone(&paged.body),
                Target {
                    document: Rc::downgrade(editor.document()),
                    body: Rc::downgrade(&paged.body),
                    input,
                    navigated_input: Cell::new(None),
                },
            )))
        })?;
        let Some((body, target)) = installed else {
            return Ok(false);
        };
        *self.frame.body.borrow_mut() = target.body.clone();
        *self.target.borrow_mut() = Some(target);
        self.arm_target_frame();
        let view = body.view().map_err(|cause| cause.to_string())?;
        // Registration may follow an already completed CPU publication. Consume that exact ready
        // Scene/frame when present; otherwise subscribe to the existing body's next actual frame.
        if let Some((key, paints)) = view.probe_cpu_published_frame()
            && paints != 0
            && view.cpu_paint_pending() == Some(false)
            && view
                .capture_ready_scene_key()
                .is_ok_and(|ready| ready.same(&key))
        {
            self.frame.frame_published(&key, paints);
        } else {
            let erased: Rc<dyn NativeMarkdownPagedBodyPaintObserver> = self.frame.clone();
            body.observe_next_cpu_frame(Rc::downgrade(&erased))
                .map_err(|cause| cause.to_string())?;
        }
        Ok(true)
    }

    /// Reuse the original SourceFrame observer. An initial BOF frame may precede fragment handoff;
    /// only the exact navigated heading state completes this operation. All observations share the
    /// actual mouse-up clock, without replaying clicks, reopening documents, or adding navigation.
    fn arm_target_frame(&self) {
        let weak = self.weak.clone();
        *self.frame.signal.borrow_mut() = Some(Box::new(move || {
            if let Some(case) = weak.upgrade()
                && let Err(cause) = case.target_frame_ready()
            {
                case.fail(cause);
            }
        }));
    }

    fn target_frame_ready(&self) -> Result<(), String> {
        use crate::native_surface::paged_text_view::NativeMarkdownPagedTextViewError as ViewError;
        if let Some(cause) = self.frame.failure.borrow().as_ref() {
            return Err(cause.clone());
        }
        let key = self
            .frame
            .key
            .borrow_mut()
            .take()
            .ok_or("相对锚点没有实际CPU帧")?;
        let expected = NSRange::new(TARGET_HEADING_PREFIX.encode_utf16().count(), 0);
        let (document_identity, body_identity, opening_input) = {
            let target = self.target.borrow();
            let target = target.as_ref().ok_or("相对锚点没有原安装票")?;
            (target.document.clone(), target.body.clone(), target.input)
        }; // Copy only weak identities and scalars; no probe RefCell borrow crosses later native getters.
        let ready = with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("相对锚点原目标已关闭")?;
            if !WeakMarkdownDocument::ptr_eq(&document_identity, &Rc::downgrade(editor.document()))
                || !std::rc::Weak::ptr_eq(&body_identity, &Rc::downgrade(&editor.body))
                || surface.current_note_relative_path.as_deref() != Some(TARGET_NOTE)
            {
                return Err("相对锚点后继帧不是原目标文稿".into());
            }
            editor
                .document()
                .borrow()
                .byte_offset_for_utf16(0)
                .map_err(|cause| cause.to_string())?;
            if let Some(cause) = editor.body.take_failure() {
                return Err(cause.to_string());
            }
            if let Some(cause) = editor.text_view.take_failure()
                && !matches!(cause, ViewError::Pending)
            {
                return Err(cause.to_string());
            }
            let actual = editor
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?;
            if actual.generation != opening_input.generation
                || actual.utf16_len != opening_input.utf16_len
                || actual.marked.is_some()
                || (actual != opening_input && actual.primary_range != expected)
            {
                return Err("相对锚点准备期间发生非预声明输入".into());
            }
            let ready_key = match editor.text_view.capture_ready_scene_key() {
                Ok(ready) => Some(ready),
                Err(ViewError::Pending) => None,
                Err(cause) => return Err(cause.to_string()),
            };
            // The independent heading position, original complete Input, ready Scene, and finished
            // CPU publication must agree. A rendered intermediate BOF frame grants no completion.
            let complete = key.request.input == actual
                && actual.primary_range == expected
                && ready_key.is_some_and(|ready| ready.same(&key))
                && editor
                    .text_view
                    .probe_cpu_published_frame()
                    .is_some_and(|(cpu, paints)| cpu.same(&key) && paints != 0)
                && editor.text_view.cpu_paint_pending() == Some(false);
            if complete {
                let target = self.target.borrow();
                let target = target.as_ref().ok_or("相对锚点安装票已退出")?;
                if target.navigated_input.get().is_some() {
                    return Err("相对锚点完成票重复绑定".into());
                }
                target.navigated_input.set(Some(actual));
            }
            Ok(complete)
        })?;
        if ready {
            *self.frame.key.borrow_mut() = Some(key);
            self.deliver();
        } else {
            // Wait only for the original target's next actual CPU publication; an intermediate BOF
            // frame is neither heading success nor permission to continue under another Input.
            self.arm_target_frame();
            let body = self
                .frame
                .body
                .borrow()
                .upgrade()
                .ok_or("相对锚点Body已释放")?;
            let erased: Rc<dyn NativeMarkdownPagedBodyPaintObserver> = self.frame.clone();
            body.observe_next_cpu_frame(Rc::downgrade(&erased))
                .map_err(|cause| cause.to_string())?;
        }
        Ok(())
    }

    /// Register one endpoint; an already completed installation/CPU result for this same operation
    /// may be consumed immediately when the observer arrives.
    fn observe_target(&self, observer: impl FnOnce() + 'static) -> Result<(), String> {
        if self.signal.borrow().is_some() || self.target.borrow().is_some() {
            return Err("相对链接目标终点重复注册".into());
        }
        *self.signal.borrow_mut() = Some(Box::new(observer));
        if self.accept_installed()? {
            return Ok(());
        }
        let released = self
            .original_view_release
            .borrow()
            .clone()
            .ok_or("相对链接缺少鼠标前登记的原View释放观察")?;
        let weak = self.weak.clone();
        let mtm = MainThreadMarker::new().expect("相对链接终点登记位于主线程");
        let controller = dispatch2::MainThreadBound::new(self.controller.clone(), mtm);
        released.when_released(move || {
            let mtm = MainThreadMarker::new().expect("原View析构端点位于主线程");
            let next = dispatch2::MainThreadBound::new(weak, mtm);
            let Some(controller) = controller.get(mtm).load() else {
                return;
            };
            // Initial deallocation may arrive inside a nested native loop. Await actual restoration
            // if the original surface is still leased, rather than treating temporary Busy as a link
            // failure, polling, or recapturing another controller.
            controller.enqueue_owned_after_release(None, move || {
                let mtm = MainThreadMarker::new().expect("原释放观察沿同一主线程恢复");
                if let Some(case) = next.get(mtm).upgrade() {
                    case.original_view_released();
                }
            });
        }); // Late registration consumes the same completed release, without waiting for another window update or guessing by time.
        Ok(())
    }

    /// Read existing installation/paint facts only after the original three-second deadline fails,
    /// without binding missing targets, consuming error slots, or advancing later work. Old-graph
    /// release and target installation/observation/painting are distinct facts, even if Body expired.
    pub(in super::super) fn diagnose_deadline(&self) {
        let target = self
            .target
            .borrow()
            .as_ref()
            .map(|target| (target.document.clone(), target.body.clone(), target.input));
        eprintln!(
            "native_relative_link_deadline target_bound={} original_view_release_registered={} switch_release_waiting={} terminal_waiting={} frame_waiting={} frame_recorded={} local_failure={} frame_failure={} original_document_released={}",
            target.is_some(),
            self.original_view_release.borrow().is_some(),
            self.switch_release_waiting.get(),
            self.signal.borrow().is_some(),
            self.frame.signal.borrow().is_some(),
            self.frame.key.borrow().is_some(),
            self.failure.borrow().is_some(),
            self.frame.failure.borrow().is_some(),
            self.original.document.upgrade().is_none(),
        );
        let window = self.window.load();
        let current = with_active_native_markdown_surface(|surface| {
            let same_window = window
                .as_ref()
                .is_some_and(|window| std::ptr::eq(&**window, &*surface.panel));
            let same_controller =
                std::ptr::from_ref(&*surface.controller).addr() == self.original.owner;
            let Some(editor) = surface.editor.as_ref() else {
                eprintln!(
                    "native_relative_link_deadline same_window={same_window} same_controller={same_controller} pending_switch={} target_path={} editor_installed=false",
                    surface.pending_note_switch.is_some(),
                    surface.current_note_relative_path.as_deref() == Some(TARGET_NOTE),
                );
                return Ok(None);
            };
            let paged = editor;
            let bound_document = target.as_ref().is_some_and(|(document, _, _)| {
                WeakMarkdownDocument::ptr_eq(document, &Rc::downgrade(editor.document()))
            });
            let bound_body = target.as_ref().is_some_and(|(_, body, _)| {
                std::rc::Weak::ptr_eq(body, &Rc::downgrade(&paged.body))
            });
            eprintln!(
                "native_relative_link_deadline same_window={same_window} same_controller={same_controller} pending_switch={} target_path={} editor_installed=true paged=true bound_document={bound_document} bound_body={bound_body}",
                surface.pending_note_switch.is_some(),
                surface.current_note_relative_path.as_deref() == Some(TARGET_NOTE),
            );
            Ok(Some((Rc::clone(&paged.body), Rc::clone(&paged.text_view))))
        });
        // The active-surface lease and all slot borrows ended. Native geometry getters may reenter,
        // but this path only reads existing diagnostics and grants no production authority.
        let Ok(Some((body, view))) = current else {
            if current.is_err() {
                eprintln!("native_relative_link_deadline current_surface=false");
            }
            return;
        };
        let input = view
            .input_state()
            .map_err(|cause| std::mem::discriminant(&cause));
        let ready = view.capture_ready_scene_key();
        let cpu = view.probe_cpu_published_frame();
        eprintln!(
            "native_relative_link_deadline input={input:?} bound_input={} mode={:?} progress={:?} ready={} ready_failure={:?} cpu_published={} cpu_paints={:?} cpu_pending={:?} cpu_steps={:?} ready_cpu_same={}",
            input.as_ref().is_ok_and(|input| target
                .as_ref()
                .is_some_and(|(_, _, original)| input == original)),
            body.presentation_mode(),
            body.progress()
                .map_err(|cause| std::mem::discriminant(&cause)),
            ready.is_ok(),
            ready.as_ref().err().map(std::mem::discriminant),
            cpu.is_some(),
            view.cpu_paints(),
            view.cpu_paint_pending(),
            view.cpu_paint_steps(),
            ready
                .as_ref()
                .is_ok_and(|ready| cpu.as_ref().is_some_and(|(cpu, _)| ready.same(cpu))),
        );
    }

    fn verify_target(&self) -> Result<(), String> {
        // A later frame or successful cleanup cannot erase the operation's original failure or
        // exceed the one mouse action's deadline and still count as successful navigation.
        if let Some(cause) = self.failure.borrow().as_ref() {
            return Err(cause.clone());
        }
        if let Some(cause) = self.frame.failure.borrow().as_ref() {
            return Err(cause.clone());
        }
        if self.started.get().ok_or("相对链接没有实际点击")?.elapsed() > Duration::from_secs(3)
        {
            return Err("相对链接从原mouseup到完整CPU帧超过三秒".into());
        }
        let (document_identity, body_identity, navigated) = {
            let target = self.target.borrow();
            let target = target.as_ref().ok_or("相对链接没有准确目标安装")?;
            (
                target.document.clone(),
                target.body.clone(),
                target
                    .navigated_input
                    .get()
                    .ok_or("相对链接标题导航没有完整后态")?,
            )
        };
        let key = self
            .frame
            .key
            .borrow()
            .clone()
            .ok_or("相对链接目标没有真实CPU终点")?;
        with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("相对链接目标已关闭")?;
            let paged = editor;
            // Require the exact installed document/body and the independently bound navigated
            // Input; another document or newer geometry cannot donate its CPU completion.
            if !WeakMarkdownDocument::ptr_eq(&document_identity, &Rc::downgrade(editor.document()))
                || !std::rc::Weak::ptr_eq(&body_identity, &Rc::downgrade(&paged.body))
                || key.request.input != navigated
                || paged
                    .text_view
                    .input_state()
                    .map_err(|cause| cause.to_string())?
                    != navigated
                || key.presentation_mode != MarkdownPresentationMode::InlinePreview
                || !paged
                    .text_view
                    .capture_ready_scene_key()
                    .is_ok_and(|ready| ready.same(&key))
                || !paged
                    .text_view
                    .probe_cpu_published_frame()
                    .is_some_and(|(actual, paints)| actual.same(&key) && paints != 0)
                || paged.text_view.cpu_paint_pending() != Some(false)
            {
                return Err("相对链接目标CPU帧已被后继Doc/Input/几何替换".into());
            }
            paged
                .text_view
                .verify_find_target_visible(
                    &key,
                    NSRange::new(TARGET_HEADING_PREFIX.encode_utf16().count(), 0),
                )
                .map_err(|cause| cause.to_string())?;
            let document = editor.document().borrow();
            // Verify canonical target bytes independently of pixel/frame success, then drop the
            // document borrow before capturing native weak observations and original resources.
            let mut comparison = TargetComparison::new(self.http_suffix.as_deref());
            document
                .write_to(&mut comparison)
                .map_err(|cause| cause.to_string())?;
            comparison.finish()?;
            drop(document);
            *self.target_graph.borrow_mut() = Some(Graph::capture(surface)?);
            Ok(())
        })?;
        println!(
            "native_relative_note_link actual_mouseup=true original_window=true exact_target_revision=true original_dirty_edit=true target_heading_anchor_visible=true target_cpu_frame=true action_to_frame_ms={:.3} whole_product=false formal=false",
            self.started.get().unwrap().elapsed().as_secs_f64() * 1000.0
        );
        Ok(())
    }

    /// Settle saved-file facts independently after navigation. Original read-only facts stay fixed;
    /// failure cleanup cannot adopt a new baseline and thereby excuse a failed click.
    fn verify_saved(&self) -> Result<(), String> {
        self.old_graph
            .borrow()
            .as_ref()
            .ok_or("链接缺少旧图释放收据")?
            .verify_released()?;
        let mut original = self.original_file.borrow_mut();
        let original = original.as_mut().ok_or("链接原FD已归还")?;
        let disk = self.verify_source_file(original)?;
        self.verify_target_file()?;
        self.saved_disk.set(Some(disk));
        println!(
            "native_relative_note_link original_saved_exact_prefix=true original_remainder_stream_equal=true original_graph_released=true target_file_unchanged=true whole_product=false formal=false"
        );
        Ok(())
    }

    /// Accept only the verified edit: compare the fixed prefix and stream the remainder against
    /// the still-open original inode, then check the canonical revision captured before navigation.
    /// Both successful navigation and failure cleanup use this check; it creates no new click-success receipt.
    fn verify_source_file(&self, original: &mut std::fs::File) -> Result<DiskIdentity, String> {
        original
            .seek(SeekFrom::Start(0))
            .map_err(|cause| cause.to_string())?;
        let mut saved =
            std::fs::File::open(&self.source_path).map_err(|cause| cause.to_string())?;
        let mut prefix = [0_u8; PREFIX.len()];
        saved
            .read_exact(&mut prefix)
            .map_err(|cause| cause.to_string())?;
        if prefix != PREFIX.as_bytes() {
            return Err("相对链接没有保存原未保存前缀".into());
        }
        let (mut expected, mut actual) = ([0_u8; 64 * 1024], [0_u8; 64 * 1024]);
        // Compare bounded chunks of the original remainder without loading the long document.
        // The independent old descriptor still names the pre-save inode after atomic replacement.
        loop {
            let count = original
                .read(&mut expected)
                .map_err(|cause| cause.to_string())?;
            if count == 0 {
                break;
            }
            saved
                .read_exact(&mut actual[..count])
                .map_err(|cause| cause.to_string())?;
            if actual[..count] != expected[..count] {
                return Err("链接保存改变了原长文的其他字节".into());
            }
        }
        if saved
            .read(&mut actual[..1])
            .map_err(|cause| cause.to_string())?
            != 0
        {
            return Err("链接保存出现预声明内容外的尾部".into());
        }
        let disk = disk_identity(&self.source_path)?;
        let revision = self
            .edited_revision
            .get()
            .ok_or("链接缺少已验证编辑revision")?;
        // Core open's second argument is the editable file-size limit, not a resident-memory
        // budget. Original file length plus the sole declared prefix determines this exact limit;
        // do not enlarge it to accommodate an unknown tail.
        let expected_bytes = self
            .original
            .disk
            .2
            .checked_add(u64::try_from(PREFIX.len()).map_err(|_| "链接固定前缀长度溢出")?)
            .ok_or("链接预声明保存长度溢出")?;
        if disk.length != expected_bytes {
            return Err("链接保存长度不同于原文件加唯一固定前缀".into());
        }
        // Reopen the saved file through Core's public reader. This independent mmap never joins
        // the product graph and must actually unmap immediately after its revision is checked.
        let document = MarkdownDocumentBuffer::open(&self.source_path, expected_bytes)
            .map_err(|cause| cause.to_string())?;
        let backing = document
            .original_backing_observer()
            .ok_or("保存核验没有原mmap")?;
        let actual_revision = document
            .current_revision()
            .map_err(|cause| cause.to_string())?;
        drop(document);
        if actual_revision != revision
            || backing.mapped_bytes() != 0
            || backing.managed_resident_bytes() != 0
            || backing.failed()
        {
            return Err("链接实际保存不等于原编辑revision或核验映射没有归还".into());
        }
        Ok(disk)
    }

    fn verify_target_file(&self) -> Result<(), String> {
        // Use the original target descriptor for exact bytes and the current path for immutable
        // disk identity; replacing the path with equal bytes still fails the identity check.
        let mut file = self.target_file.borrow_mut();
        let file = file.as_mut().ok_or("目标独立FD已归还")?;
        file.seek(SeekFrom::Start(0))
            .map_err(|cause| cause.to_string())?;
        let mut comparison = TargetComparison::new(self.http_suffix.as_deref());
        io::copy(file, &mut comparison).map_err(|cause| cause.to_string())?;
        comparison.finish()?;
        if disk_identity(&self.target_path)? != self.target_disk {
            return Err("相对链接打开或保存改写了目标文件".into());
        }
        Ok(())
    }

    /// Cleanup may close only the original source or the bound target inside the same original
    /// window. Legitimate character-generation changes do not replace those weak identities.
    pub(in super::super) fn require_return_current(
        &self,
        surface: &NativeMarkdownWindowSurface,
    ) -> Result<(), String> {
        let window = self.window.load().ok_or("相对链接原窗口已释放")?;
        if !native_markdown_controller_has_foreground_authority(&surface.controller)
            || std::ptr::from_ref(&*surface.controller).addr() != self.original.owner
            || !std::ptr::eq(&*surface.panel, &*window)
        {
            return Err("相对链接原Host窗口已经失效".into());
        }
        let editor = surface
            .editor
            .as_ref()
            .ok_or("相对链接原客户尚未完成安装")?;
        let original = WeakMarkdownDocument::ptr_eq(
            &self.original.document,
            &Rc::downgrade(editor.document()),
        );
        let target = self.target.borrow();
        let bound_target = target.as_ref().is_some_and(|target| {
            WeakMarkdownDocument::ptr_eq(&target.document, &Rc::downgrade(editor.document()))
                && std::rc::Weak::ptr_eq(&target.body, &Rc::downgrade(&editor.body))
        });
        if !original && !bound_target {
            return Err("相对链接清理不能接管后来打开的同名Doc".into());
        }
        Ok(())
    }

    /// HTTP borrows only the target graph that passed relative navigation. An external page cannot
    /// authorize document replacement, history changes, or recapture of Source authority.
    fn http_target_view(
        &self,
    ) -> Result<
        Rc<Retained<crate::native_surface::paged_text_view::NativeMarkdownPagedTextView>>,
        String,
    > {
        if self.http_suffix.is_none() || self.saved_disk.get().is_none() {
            return Err("HTTP点击缺少显式页面或原保存完成事实".into());
        }
        let view = with_active_native_markdown_surface(|surface| {
            let (target_document, target_body, navigated) = {
                let target = self.target.borrow();
                let target = target.as_ref().ok_or("HTTP目标没有原安装票")?;
                (
                    target.document.clone(),
                    target.body.clone(),
                    target.navigated_input.get(),
                )
            }; // End the local observation-slot borrow before any native View/Scene entry.
            let editor = surface.editor.as_ref().ok_or("HTTP原目标已关闭")?;
            let input = editor
                .text_view
                .input_state()
                .map_err(|cause| cause.to_string())?;
            if !WeakMarkdownDocument::ptr_eq(&target_document, &Rc::downgrade(editor.document()))
                || !std::rc::Weak::ptr_eq(&target_body, &Rc::downgrade(&editor.body))
                || navigated != Some(input)
                || self.http_input.get().is_some_and(|before| before != input)
            {
                return Err("HTTP改变了原目标Doc/Body或完整Input".into());
            }
            let document = editor.document().borrow();
            // Browser focus changes are allowed; canonical content, clean history, and the exact
            // original target revision must remain unchanged throughout the external handoff.
            if document
                .current_revision()
                .map_err(|cause| cause.to_string())?
                != self.target_revision
                || document.is_dirty()
                || document.can_undo()
                || document.can_redo()
            {
                return Err("HTTP外链改变了原目标内容或历史".into());
            }
            drop(document); // Finish Source/revision checks before any native getter; no document borrow crosses that boundary.
            self.require_return_current(surface)?;
            let key = editor
                .text_view
                .capture_ready_scene_key()
                .map_err(|cause| cause.to_string())?;
            if key.request.input != input
                || self
                    .frame
                    .key
                    .borrow()
                    .as_ref()
                    .is_none_or(|original| original.source != key.source)
            {
                return Err("HTTP目标不再使用原Source及完整Input".into());
            } // Browser activation may change focus presentation, never the admitted document or Source.
            Ok(Rc::clone(&editor.text_view))
        })?;
        if disk_identity(&self.source_path)? != self.saved_disk.get().ok_or("HTTP原保存身份缺失")?
        {
            return Err("HTTP改变了已验证源文件的磁盘身份".into());
        }
        self.verify_target_file()?;
        Ok(view)
    }

    /// Admit one declared Command-click after Host arms its server observation. Posting the native
    /// events is an action only, not evidence that HTTP or browser work completed.
    fn click_http(&self) -> Result<(), String> {
        if self.http_input.get().is_some() {
            return Err("本轮HTTP链接不得重复点击".into());
        }
        let view = self.http_target_view()?;
        let window = self.window.load().ok_or("HTTP原窗口已释放")?;
        window.makeKeyWindow();
        if !window.makeFirstResponder(Some(&**view)) || !window.isKeyWindow() {
            return Err("HTTP原正文没有真实鼠标焦点".into());
        }
        let key = view
            .capture_ready_scene_key()
            .map_err(|cause| cause.to_string())?;
        let before = view.input_state().map_err(|cause| cause.to_string())?;
        // Only the suffix's newline and opening bracket precede its label. Derive the offset from
        // independently declared fixture bytes, without inferring a hit position from the result.
        let start = TARGET_TEXT.encode_utf16().count() + 2;
        let rect = view
            .screen_rect(NSRange::new(start, 1))
            .map_err(|cause| cause.to_string())?;
        if rect.actual_range != NSRange::new(start, 1) || rect.rect.size.width <= 0.0 {
            return Err("HTTP标签没有完整GCB墨迹点".into());
        }
        let point = window.convertPointFromScreen(NSPoint::new(
            rect.rect.origin.x + rect.rect.size.width * 0.25,
            rect.rect.origin.y + rect.rect.size.height * 0.5,
        ));
        let events = [NSEventType::LeftMouseDown, NSEventType::LeftMouseUp].map(|kind| {
            NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                kind, point, NSEventModifierFlags::Command, 1.0, window.windowNumber(), None, 1, 1, 1.0,
            )
        });
        let [Some(down), Some(up)] = events else {
            return Err("无法建立HTTP真实鼠标事件".into());
        };
        if !view
            .capture_ready_scene_key()
            .is_ok_and(|actual| actual.same(&key))
            || view.input_state().map_err(|cause| cause.to_string())? != before
        {
            return Err("HTTP鼠标构造期间原Scene/Input失效".into());
        }
        self.http_input.set(Some(before)); // Consume the sole action before posting; no failure authorizes external navigation replay.
        let application = NSApplication::sharedApplication(window.mtm());
        application.postEvent_atStart(&down, false);
        application.postEvent_atStart(&up, false);
        Ok(())
    }

    /// Check the original document after the Host's actual GET/204 receipt; browser cleanup is separate.
    fn verify_http_unchanged(&self) -> Result<(), String> {
        if self.http_input.get().is_none() {
            // A network receipt cannot substitute for the original Command-click intent.
            return Err("HTTP没有原鼠标动作票".into());
        }
        // This accessor revalidates the original target, content and history authority.
        // A published view failure remains a failure even after a server receives its GET.
        let view = self.http_target_view()?;
        if let Some(cause) = view.take_failure() {
            return Err(cause.to_string());
        }
        // Report only the document facts checked here, without claiming ownership of a browser tab.
        println!(
            "native_external_http_link actual_command_mouse=true original_input_unchanged=true original_source_unchanged=true original_history_unchanged=true both_files_unchanged=true https_verified=false browser_internal_cleanup_verified=false"
        );
        Ok(())
    }

    /// Revoke local notifications and close the target descriptor on success and failure alike.
    /// Keep the independent original inode until Host close completes and the actual save is verified.
    pub(in super::super) fn prepare_return(&self) {
        drop(self.signal.borrow_mut().take());
        drop(self.frame.signal.borrow_mut().take());
        let release = self.original_view_release.borrow_mut().take();
        drop(release); // Revoke endpoints before releasing markers/observation; destructor reentry cannot readmit a target.
        self.switch_release_waiting.set(false);
        drop(self.edited_frame.borrow_mut().take());
        drop(self.target_file.borrow_mut().take());
    }

    /// After strict product close, check the actual save and captured resources. Successful cleanup
    /// cannot count as successful navigation if the target never reached a complete frame.
    pub(in super::super) fn verify_closed(&self) -> Result<(), String> {
        let mut original = self
            .original_file
            .borrow_mut()
            .take()
            .ok_or("相对链接原独立FD已归还")?;
        let result: Result<bool, String> = (|| {
            self.old_graph
                .borrow()
                .as_ref()
                .ok_or("相对链接缺少原图释放收据")?
                .verify_released()?;
            let target_graph = self.target_graph.borrow();
            if let Some(graph) = target_graph.as_ref() {
                graph.verify_released()?;
            }
            // Once installed, both actual target identities must end even if frame verification
            // failed. Without installation, do not fabricate observations of nonexistent resources.
            if self.target.borrow().as_ref().is_some_and(|target| {
                target.document.upgrade().is_some() || target.body.upgrade().is_some()
            }) || self.target_file.borrow().is_some()
                || self.original_view_release.borrow().is_some()
            {
                return Err("相对链接关闭后目标或本地观察资源未归还".into());
            }
            let disk = self.verify_source_file(&mut original)?;
            if self.saved_disk.get().is_some_and(|saved| saved != disk)
                || disk_identity(&self.target_path)? != self.target_disk
            {
                return Err("相对链接关闭改变了已验证保存或目标磁盘身份".into());
            }
            let mut target =
                std::fs::File::open(&self.target_path).map_err(|cause| cause.to_string())?;
            let mut comparison = TargetComparison::new(self.http_suffix.as_deref());
            io::copy(&mut target, &mut comparison).map_err(|cause| cause.to_string())?;
            comparison.finish()?;
            Ok(target_graph.is_some())
        })();
        drop(original); // Close the independent descriptor before success/error propagation; later idle or process exit is not this cleanup.
        let target_frame_verified = result?;
        println!(
            "native_relative_note_link strict_close=true original_graph_released=true target_frame_verified={target_frame_verified} source_expected_edit_saved=true both_files_verified=true whole_product=false formal=false"
        );
        Ok(())
    }
}

impl Drop for RelativeLink {
    fn drop(&mut self) {
        // Failed registration and failed functionality revoke this operation's notifications too.
        // This destructor neither deletes user files nor sends keyboard/input-source actions.
        self.prepare_return();
        drop(self.original_file.get_mut().take()); // This owner closes the fixture descriptor even when the verified-close stage never ran.
    }
}

impl NativeMarkdownMemoryWorkload {
    /// Dispatch only the owning workload's declared stages. The original case retains action,
    /// Source, target, and cleanup identities across steps instead of recreating them per call.
    pub(crate) fn exercise_relative_link(
        &self,
        stage: i16,
        http_url: Option<&str>,
    ) -> Result<(), String> {
        if stage == 570 {
            let case = RelativeLink::begin(self, http_url)?;
            *self.relative_link.borrow_mut() = Some(case);
            return Ok(());
        }
        // Later stages must borrow the already admitted case; missing ownership is an error,
        // never permission to create a new target or replay an earlier mouse action.
        let case = self
            .relative_link
            .borrow()
            .clone()
            .ok_or("相对链接缺少原场景owner")?;
        match stage {
            571 => case.verify_edit(),
            572 => case.click(),
            573 => case.verify_target(),
            574 => case.verify_saved(),
            575 => self.request_return(),
            576 => case.click_http(),
            577 => case.verify_http_unchanged(),
            _ => Err("未声明的相对链接事件".into()),
        }
    }

    /// Subscribe only to the actual edited-frame or installed-target endpoint owned by this case;
    /// synchronous step dispatch and cleanup are not replacement terminal events.
    pub(crate) fn observe_relative_link_terminal(
        &self,
        stage: i16,
        observer: impl FnOnce() + 'static,
    ) -> Result<(), String> {
        let case = self
            .relative_link
            .borrow()
            .clone()
            .ok_or("相对链接终点缺少原owner")?;
        match stage {
            571 => case
                .edited_frame
                .borrow()
                .clone()
                .ok_or("链接编辑缺少原完整帧")?
                .observe(observer),
            573 => case.observe_target(observer),
            _ => Err("未声明的相对链接终点".into()),
        }
    }
}
