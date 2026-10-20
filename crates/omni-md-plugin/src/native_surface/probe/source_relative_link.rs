//! 隔离笔记库内的真实链接交接：未保存编辑、原鼠标激活、旧图释放与目标完整帧分别核验。

use super::*;
use objc2::rc::Weak as NativeWeak;
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

/// 两个文件各有自己的磁盘身份；保存只能替换源文件，目标不能因打开或关闭被重写。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DiskIdentity {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

fn disk_identity(path: &Path) -> Result<DiskIdentity, String> {
    let metadata = std::fs::metadata(path).map_err(|cause| cause.to_string())?;
    Ok(DiskIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
    })
}

/// 只保真实资源的观察值；不能把旧Doc消失当作Font、Reader和映射均已归还。
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
    /// 必须在原完整CPU帧已经通过后捕获；不创建新Reader或为了凑角色而重排正文。
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

    /// 交接与最终关闭均只读原收据；没有循环、阈值或等待Weak自行变空的第二释放协议。
    fn verify_released(&self) -> Result<(), String> {
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
            if weak.load().is_some() {
                return Err(format!("相对链接的原Native角色尚未释放：{role}"));
            }
        }
        self.ledger.verify_released()?;
        if !self.receipts.source.payload_released() || self.receipts.reader.is_none() {
            return Err("链接完整帧Source未释放或缺少真实Reader收据".into());
        }
        for observer in self
            .receipts
            .reader
            .iter()
            .chain(std::iter::once(&self.receipts.writer))
        {
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
            if !font.payload_released()
                || !bytes.unmap_succeeded()
                || bytes.mapped_bytes() != 0
                || bytes.failed()
            {
                return Err("链接原Font payload或字节映射未归还".into());
            }
        }
        for cell in &self.receipts.cells {
            cell.verify_released()?;
        }
        Ok(())
    }
}

/// 已安装目标只保弱身份和当时完整Input；路径相同的后来另一个Doc不能替本次交接通过。
struct Target {
    document: WeakMarkdownDocument,
    body: std::rc::Weak<NativeMarkdownPagedBody>,
    input: NativeMarkdownPagedInputState,
    /// 安装票与真正导航完成票分开；只在独立标题位置、原Doc和CPU都通过时绑定一次后态。
    navigated_input: Cell<Option<NativeMarkdownPagedInputState>>,
}

/// 这个场景只有一个原鼠标动作和一个目标；实际释放通知只绑定预声明文件，不发现任意后继文稿。
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
}

impl RelativeLink {
    /// 原401已确认首屏；只有显式相对链接流程可在Host独占合成库建立自己的create_new文件。
    fn begin(workload: &NativeMarkdownMemoryWorkload) -> Result<Rc<Self>, String> {
        if workload.flow != NativeMarkdownHostedFlow::SourceRelativeLink {
            return Err("相对链接文件只允许建在本轮明确的Host合成场景".into());
        }
        workload.verify_open()?;
        let original = workload.facts()?;
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
        created.sync_all().map_err(|cause| cause.to_string())?;
        drop(created);
        let target_disk = disk_identity(&target_path)?;
        // 这个固定短文件的独立Core读取只求磁盘revision，不建Native图；在真实点击之前立即归还。
        // 生产目标随后仍从正式session打开，没有把这里的模型或源权限交给它。
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
                // 安装后的真实fragment选择会推进fold发布代次；这里只筛原g0/模式，
                // 终点由下方准确标题位置与同一完整Input/Scene/CPU联合认证，不能假定fold0。
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
        });
        case.insert(workload)?;
        Ok(case)
    }

    /// 只插入固定小前缀；显式0..0避免偷用后来选区，期望长度和g+1在Native前已知。
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
        // SAFETY: 这个客户属于已核的合成原Doc，只有有限NSString与一次显式插入，非IME验收。
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

    /// 已显示编辑必须与独立原FD逐片一致；这一实际正文revision才是后继保存的唯一允许结果。
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
            // 原独立FD借用在任何Native弱观察和资源捕获之前归还。
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

    /// 同真实Scene取得标签墨迹点；点击前保存实际原窗口身份，不能直接调用切笔记实现。
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
        // 先观察这一个原View的真实析构。关联marker只持原子观察元数据，不保活View；
        // 它只负责在异步链接确实拆图后找到原pending switch，绝不单独认证完整图已释放。
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
            // Command 单击沿原窗口号和真实命中坐标各入队一次；由NSApplication→原NSWindow→原View派发。
            // 不直接调用View selector或切笔记实现；这里不手动pump、重播或补点击。
            application.postEvent_atStart(&event, false);
        }
        if let Some(cause) = view.take_failure() {
            return Err(cause.to_string());
        }
        Ok(())
    }

    /// 原View真正析构后，只观察同一pending switch的完整释放端点；不以队列先后推测安装完成。
    fn original_view_released(&self) {
        if self.signal.borrow().is_none() {
            return; // Return/关闭已经撤销本次终点；迟到释放只结束观察，不能重新绑定后继文稿。
        }
        let result = (|| {
            if self.accept_installed()? {
                return Ok(()); // 若生产切换已经完成，消费它的实际安装与现成CPU帧，不要求另一次发布。
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
                    // 原relay先让生产finish消费同一屏障。此一次退栈仅移交已发生的事件，
                    // 不反复入队、不运行executor，也不把释放本身签成目标安装或完整CPU帧。
                    let Some(controller) = controller.get(mtm).load() else {
                        return;
                    };
                    // 同一生产handler可能因同步Native重入仍在等待原SurfaceLease恢复；
                    // 复用原accepted continuation接该实际恢复事件，不按main queue轮次猜安装先后。
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
        if self.failure.borrow().is_none() {
            *self.failure.borrow_mut() = Some(cause);
        }
        self.deliver();
    }

    fn deliver(&self) {
        let signal = self.signal.borrow_mut().take();
        if let Some(signal) = signal {
            signal();
        }
    }

    /// 新Doc只能在预声明路径/revision和原窗口的真实安装结果处绑定一次；未安装返回false。
    /// 调用方只在已有结果或真实释放端点继续，不能把false当成重新排队的许可。
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
                return Ok(None); // 原保存/拆图尚在进行；后继只接原View及原屏障的真实释放事件。
            }
            if surface.current_note_relative_path.as_deref() != Some(TARGET_NOTE)
                || surface.pending_note_switch.is_some()
                || self.original.document.upgrade().is_some()
            {
                return Err("相对链接目标安装未满足准确路径及旧Doc释放屏障".into());
            }
            let paged = editor;
            let document = editor.document().borrow();
            if document
                .path()
                .canonicalize()
                .map_err(|cause| cause.to_string())?
                != self
                    .target_path
                    .canonicalize()
                    .map_err(|cause| cause.to_string())?
                || document.original_revision() != Some(self.target_revision)
                || document.is_dirty()
                || document.generation() != 0
                || document.utf16_len() != TARGET_TEXT.encode_utf16().count()
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

    /// 仍使用原SourceFrame观察者；首屏可能在fragment接管前发布，只有明确标题后态才能交终点。
    /// 注册与后继帧共用实际mouseup时钟，没有重发点击、重开Doc或补做导航。
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
        }; // 这里只复制弱身份与标量，后续Native getter不持任何probe RefCell借用。
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
            // 只等原目标的下一次真实CPU发布；中间文首帧不是标题成功，也不替新输入续权。
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

    /// 场景终点只订阅一次；注册时已经到达的同一个真实安装/CPU结果可以立即消费。
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
            // 首次真实析构也可能从嵌套Native loop到达；原owner仍借出时只等其真正恢复，
            // 不让with_active的暂时Busy变成链接失败，不轮询或重新捕获后继controller。
            controller.enqueue_owned_after_release(None, move || {
                let mtm = MainThreadMarker::new().expect("原释放观察沿同一主线程恢复");
                if let Some(case) = next.get(mtm).upgrade() {
                    case.original_view_released();
                }
            });
        }); // 注册晚于真实释放也消费同一已完成事实；没有下一窗口更新或时间猜测。
        Ok(())
    }

    /// 原三秒期限已经失败才读取现成安装与绘制事实；不补绑定、不消费错误槽，也不推动后继工作。
    /// 这里区分“旧图已拆”与“目标已安装/已观察/已绘制”，不能用旧Body消失掩盖后面任一缺口。
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
        // Active租约与所有槽借用均已归还；Native几何查询即使重入也只读取当前诊断事实。
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
            let mut bytes = [0_u8; TARGET_TEXT.len()];
            let written = {
                let mut comparison = io::Cursor::new(bytes.as_mut_slice());
                document
                    .write_to(&mut comparison)
                    .map_err(|cause| cause.to_string())?;
                comparison.position() as usize
            };
            if written != TARGET_TEXT.len() || bytes != TARGET_TEXT.as_bytes() {
                return Err("相对链接目标canonical不同于独立固定字节".into());
            }
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

    /// 导航成功后的保存事实独立结算；原只读facts不改写，失败清理更不能借新基线冲销点击失败。
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

    /// 只接受本次已验证编辑的字节：固定前缀之后逐片对照仍打开的原inode，另核编辑前已封存的revision。
    /// 本函数既用于导航成功后的574，也用于失败后实际Return已保存的423；不会生成新的功能成功票。
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
        // open 的第二参数是文件编辑上限，而非驻留内存额度。原文件长度与唯一插入前缀
        // 在动作前已经确定；只允许这个精确的新长度，不借任意放大限额容纳未知尾部。
        let expected_bytes = self
            .original
            .disk
            .2
            .checked_add(u64::try_from(PREFIX.len()).map_err(|_| "链接固定前缀长度溢出")?)
            .ok_or("链接预声明保存长度溢出")?;
        if disk.length != expected_bytes {
            return Err("链接保存长度不同于原文件加唯一固定前缀".into());
        }
        // 已保存的文件再走Core公开读取；此独立mmap不进入产品图，核完立即归还实际映射。
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
        let mut file = self.target_file.borrow_mut();
        let file = file.as_mut().ok_or("目标独立FD已归还")?;
        file.seek(SeekFrom::Start(0))
            .map_err(|cause| cause.to_string())?;
        let mut bytes = [0_u8; TARGET_TEXT.len()];
        file.read_exact(&mut bytes)
            .map_err(|cause| cause.to_string())?;
        let mut tail = [0];
        if bytes != TARGET_TEXT.as_bytes()
            || file.read(&mut tail).map_err(|cause| cause.to_string())? != 0
            || disk_identity(&self.target_path)? != self.target_disk
        {
            return Err("相对链接打开或保存改写了目标文件".into());
        }
        Ok(())
    }

    /// 清理只关闭同一原窗口中的源Doc或已绑定目标；合法字符代次变化不改变这些弱身份。
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

    /// 错误/正常返回都先撤本地通知和目标FD；原独立inode留到Host收讫后核真实保存。
    pub(in super::super) fn prepare_return(&self) {
        drop(self.signal.borrow_mut().take());
        drop(self.frame.signal.borrow_mut().take());
        let release = self.original_view_release.borrow_mut().take();
        drop(release); // 先撤终点再归还关联marker/观察；即使析构重入也不能重新接纳目标。
        self.switch_release_waiting.set(false);
        drop(self.edited_frame.borrow_mut().take());
        drop(self.target_file.borrow_mut().take());
    }

    /// 产品严格关闭后核真实保存与已观察的资源；没有到达目标完整帧也不能把清理伪装成导航通过。
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
            // 目标安装后即使完整帧失败，也必须结束这两个真实weak；没有安装则不能虚构目标资源观察。
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
            let mut bytes = [0_u8; TARGET_TEXT.len()];
            target
                .read_exact(&mut bytes)
                .map_err(|cause| cause.to_string())?;
            let mut tail = [0];
            if bytes != TARGET_TEXT.as_bytes()
                || target.read(&mut tail).map_err(|cause| cause.to_string())? != 0
            {
                return Err("相对链接目标在最终关闭时字节变化".into());
            }
            Ok(target_graph.is_some())
        })();
        drop(original); // 成功与错误都先归还原独立FD，不能靠后继idle或进程退出替本轮清理。
        let target_frame_verified = result?;
        println!(
            "native_relative_note_link strict_close=true original_graph_released=true target_frame_verified={target_frame_verified} source_expected_edit_saved=true both_files_verified=true whole_product=false formal=false"
        );
        Ok(())
    }
}

impl Drop for RelativeLink {
    fn drop(&mut self) {
        // 注册失败和真实功能失败也移除本轮通知；没有删除用户文件或发送任何键盘/输入源动作。
        self.prepare_return();
        drop(self.original_file.get_mut().take()); // 423没有执行的错误出口仍由本唯一owner关闭测试FD。
    }
}

impl NativeMarkdownMemoryWorkload {
    pub(crate) fn exercise_relative_link(&self, stage: i16) -> Result<(), String> {
        if stage == 570 {
            let case = RelativeLink::begin(self)?;
            *self.relative_link.borrow_mut() = Some(case);
            return Ok(());
        }
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
            _ => Err("未声明的相对链接事件".into()),
        }
    }

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
