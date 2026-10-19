//! 原 super 前签原票；后继捕获只补 Native 几何，不能重新领取较新的正文或模式权限。

use super::*;
use objc2_app_kit::{NSTextContentManager, NSWindow};
use objc2_core_graphics::CGImage;
use objc2_foundation::NSValue;
use objc2_quartz_core::CALayer;

fn geometry_epoch_current(owner: &NativeMarkdownRasterSurface, epoch: u64) -> bool {
    !owner.closed.get()
        && !owner
            .counters
            .viewport_epoch_exhausted
            .load(Ordering::Acquire)
        && owner.counters.viewport_epoch.load(Ordering::Acquire) == epoch
}

fn geometry_native_step<T>(
    capture: &NativeMarkdownSelectionCapture,
    owner: &NativeMarkdownRasterSurface,
    epoch: u64,
    work: impl FnOnce() -> T,
) -> Result<T, String> {
    if !geometry_epoch_current(owner, epoch) {
        return Err("光栅真实几何事件在 Native 前撤原票".into());
    }
    let result =
        selection_native_step(capture, work).map_err(|_| "光栅原选择票在 Native 调用中失效")?;
    if !geometry_epoch_current(owner, epoch) {
        return Err("光栅真实几何事件在 Native 后撤原票".into());
    }
    Ok(result)
}

#[derive(Clone, Copy, PartialEq)]
struct Key {
    generation: u64,
    selection: MarkdownTextSelection,
    mode: MarkdownPresentationMode,
    font_epoch: u64,
    semantic_epoch: u64,
    table_epoch: u64,
    table_width: i64,
    input_epoch: u64,
    input_intent: u64,
    fold_epoch: u64,
    preview_intent: u64,
    fold_revision: u64,
    composition: bool,
    marked: Option<NSRange>,
    input_depth: usize,
}

/// 空viewport只代表canonical空文档或真实EOF；内部暂态空范围不能取得绘制完成权限。
fn terminal_viewport_matches(
    length: usize,
    provider_length: usize,
    start: usize,
    end: usize,
) -> bool {
    length <= isize::MAX as usize && provider_length == length && start == length && end == length
}

fn capture_terminal_viewport(
    owner: &NativeMarkdownRasterSurface,
    view: &PieceBackedMarkdownTextView,
    permit: &NativeMarkdownRasterOriginPermit,
    content: &NSTextContentManager,
    range: &NSTextRange,
    length: usize,
) -> Result<bool, String> {
    let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(content);
    let document = permit.step(owner, view, || provider.documentRange())?;
    let origin = permit.step(owner, view, || document.location())?;
    let document_end = permit.step(owner, view, || document.endLocation())?;
    let start = permit.step(owner, view, || range.location())?;
    let end = permit.step(owner, view, || range.endLocation())?;
    let mut offsets = [0_usize; 3];
    for (slot, location) in offsets.iter_mut().zip([&document_end, &start, &end]) {
        let offset = permit.step(owner, view, || {
            content.offsetFromLocation_toLocation(&origin, location)
        })?;
        *slot = usize::try_from(offset).map_err(|_| "终端视口Native端点不属于原canonical文档")?;
        let reconstructed = permit
            .step(owner, view, || {
                content.locationFromLocation_withOffset(&origin, offset)
            })?
            .ok_or("终端视口原Native端点无法重建")?;
        if permit.step(owner, view, || {
            content.offsetFromLocation_toLocation(location, &reconstructed)
        })? != 0
        {
            return Err("终端视口原Native端点往返不一致".into());
        }
    }
    if offsets[0] != length {
        return Err("终端视口provider长度与原canonical文档不一致".into());
    }
    Ok(terminal_viewport_matches(
        length, offsets[0], offsets[1], offsets[2],
    ))
}

fn key(
    owner: &NativeMarkdownRasterSurface,
    view: &PieceBackedMarkdownTextView,
    storage: &PieceBackedMarkdownTextStorage,
) -> Option<Key> {
    if owner.closed.get()
        || view.ivars().textkit_graph_teardown.get()
        || !storage.ivars().accepts_edits.get()
    {
        return None;
    }
    let document = storage.document()?;
    if !std::rc::Weak::ptr_eq(&Rc::downgrade(&document), &owner.graph.document) {
        return None;
    }
    let (generation, selection) = {
        let document = document.try_borrow().ok()?;
        (document.generation(), document.selection())
    };
    drop(document); // 纯票只存标量；任何 Native 原语之前已经结束正文借用。
    Some(Key {
        generation,
        selection,
        mode: storage.presentation_mode(),
        font_epoch: storage.ivars().font_run_scratch.epoch(),
        semantic_epoch: storage.ivars().presentation_semantic_epoch.get()?,
        table_epoch: storage.ivars().table_geometry_epoch.get(),
        table_width: storage.ivars().table_projection_width_key.get(),
        input_epoch: storage.ivars().input_transaction_epoch.get(),
        input_intent: view.ivars().table_input_intent_epoch.get(),
        fold_epoch: view.ivars().preview_block_fold_epoch.get(),
        preview_intent: view.ivars().preview_block_intent_epoch.get(),
        fold_revision: view.ivars().folds.revision(),
        composition: storage.ivars().composition_active.get(),
        marked: view.ivars().marked_text_range.get(),
        input_depth: view.ivars().textkit_input_depth.get(),
    })
}

/// 仅存在于本次 begin→super→consume 同步栈；Native claim 不进入持久 Surface 或回调。
pub(in crate::native_surface) struct NativeMarkdownRasterOriginPermit {
    owner: std::rc::Weak<NativeMarkdownRasterSurface>,
    view: Retained<PieceBackedMarkdownTextView>,
    storage: Retained<PieceBackedMarkdownTextStorage>,
    capture: NativeMarkdownSelectionCapture,
    key: Key,
    viewport_epoch: u64,
}

impl NativeMarkdownRasterOriginPermit {
    pub(super) fn begin(
        owner: &Rc<NativeMarkdownRasterSurface>,
        view: &PieceBackedMarkdownTextView,
    ) -> Result<Option<Self>, String> {
        if owner.closed.get() || view.ivars().textkit_graph_teardown.get() {
            return Ok(None);
        }
        if owner
            .counters
            .viewport_epoch_exhausted
            .load(Ordering::Acquire)
        {
            return Err("光栅几何事件代次已耗尽".into());
        }
        let viewport_epoch = owner.counters.viewport_epoch.load(Ordering::Acquire);
        // 首个 Weak/Native 操作前就取得现有纯 Rust 票。构造未绑 owner 或合法撤权只取消本次，
        // 既不自行重签，也不把尚无 viewport 的首屏当作已经画好。
        let Ok(capture) = view.ivars().folds.capture_selection() else {
            return Ok(None);
        };
        let result = (|| {
            let original =
                geometry_native_step(&capture, owner, viewport_epoch, || owner.graph.view.load())?
                    .ok_or("光栅原 view 已释放")?;
            if !std::ptr::eq::<PieceBackedMarkdownTextView>(&*original, view) {
                return Err("光栅 begin 来自其他正文".into());
            }
            let storage = geometry_native_step(&capture, owner, viewport_epoch, || {
                owner.graph.storage.load()
            })?
            .ok_or("光栅原 storage 已释放")?;
            let key = key(owner, view, &storage).ok_or("光栅原代次或缓存权限无效")?;
            let permit = Self {
                owner: Rc::downgrade(owner),
                view: original,
                storage,
                capture: capture.clone(),
                key,
                viewport_epoch,
            };
            if !permit.current(owner, view) {
                return Ok(None);
            }
            Ok(Some(permit))
        })();
        if !capture.current() || !geometry_epoch_current(owner, viewport_epoch) {
            Ok(None)
        } else {
            result
        }
    }

    pub(super) fn current(
        &self,
        owner: &NativeMarkdownRasterSurface,
        view: &PieceBackedMarkdownTextView,
    ) -> bool {
        self.capture.current()
            && geometry_epoch_current(owner, self.viewport_epoch)
            && std::ptr::eq::<PieceBackedMarkdownTextView>(&*self.view, view)
            && self.owner.upgrade().is_some_and(|original| {
                std::ptr::eq::<NativeMarkdownRasterSurface>(&*original, owner)
            })
            && key(owner, view, &self.storage) == Some(self.key)
    }

    pub(super) fn step<T>(
        &self,
        owner: &NativeMarkdownRasterSurface,
        view: &PieceBackedMarkdownTextView,
        work: impl FnOnce() -> T,
    ) -> Result<T, String> {
        if !self.current(owner, view) {
            return Err("光栅原票在 Native 调用前失效".into());
        }
        let result = geometry_native_step(&self.capture, owner, self.viewport_epoch, work)?;
        if !self.current(owner, view) {
            return Err("光栅原票在 Native 调用后失效".into());
        }
        Ok(result)
    }
}

/// 仅补本次 Native 几何，所有原语均消费 super 之前固定的同一 OriginPermit。
pub(super) struct Authority<'a> {
    owner: &'a NativeMarkdownRasterSurface,
    permit: &'a NativeMarkdownRasterOriginPermit,
    pub(super) view: &'a PieceBackedMarkdownTextView,
    pub(super) manager: Retained<NSTextLayoutManager>,
    pub(super) content: Retained<NSTextContentManager>,
    window: Retained<NSWindow>,
    pub(super) range: Retained<NSTextRange>,
    pub(super) terminal_viewport: bool,
    pub(super) document_length: usize,
    selection: Retained<NSArray<NSValue>>,
    pub(super) visible: NSRect,
    pub(super) origin: NSPoint,
    bounds: NSRect,
    pub(super) scale: f64,
    pub(super) native: Rc<NativeOwners>,
    pub(super) layer: Retained<CALayer>,
    fragment_epoch: u64,
    published_image: Cell<Option<NonNull<CGImage>>>,
}

impl<'a> Authority<'a> {
    pub(super) fn owner(&self) -> &NativeMarkdownRasterSurface {
        self.owner
    }

    pub(super) fn generation(&self) -> u64 {
        self.permit.key.generation // 原super之前已经固定的代次；诊断不能补领后继文档权限。
    }

    /// 无ink不等于表格占位：只有原行高投影能授权隐藏table源行，空白源码不应等待表格。
    pub(super) fn table_overlay_requirement(&self) -> Result<Option<bool>, String> {
        if !self.current() {
            return Ok(None); // 原视口已撤权，不能依据后继投影决定本次空图是否完整。
        }
        if self.permit.key.mode == MarkdownPresentationMode::Source {
            return Ok(Some(false)); // Source属性不使用占位高度；完整实际源流可以只有空白。
        }
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*self.content);
        let document = self.step(|| provider.documentRange())?;
        let origin = self.step(|| document.location())?;
        let start = self.step(|| self.range.location())?;
        let end = self.step(|| self.range.endLocation())?;
        let mut offsets = [0_usize; 2];
        for (slot, location) in offsets.iter_mut().zip([&start, &end]) {
            let offset = self.step(|| {
                self.content
                    .offsetFromLocation_toLocation(&origin, location)
            })?;
            *slot = usize::try_from(offset).map_err(|_| "占位检查的原视口端点越界")?;
            let reconstructed = self
                .step(|| {
                    self.content
                        .locationFromLocation_withOffset(&origin, offset)
                })?
                .ok_or("占位检查的原视口端点无法重建")?;
            if *slot > self.document_length
                || self.step(|| {
                    self.content
                        .offsetFromLocation_toLocation(location, &reconstructed)
                })? != 0
            {
                return Err("占位检查的原视口端点往返不一致".into());
            }
        } // 只转换两个Native端点；不读取字符，也不扩展到实际paragraph或整篇。
        if offsets[0] > offsets[1] {
            return Err("占位检查的原视口范围反向".into());
        }
        let requirement = {
            let Ok(projection) = self.permit.storage.ivars().table_row_heights.try_borrow() else {
                return Ok(None); // 同步准备正在借用投影，只取消本次，不制造第二份缓存。
            };
            match projection.as_ref() {
                None => Some(false), // 无行高时属性producer明确保留literal源文，不能隐藏表格行。
                Some(projection)
                    if projection.matches(
                        &self.owner.graph.document,
                        self.permit.key.generation,
                        self.permit.key.table_width,
                        self.permit.key.table_epoch,
                    ) =>
                {
                    Some(projection.rows.iter().any(|row| {
                        // 只判断有界行高元数据与原视口相交；零宽终端另走独立终端证明。
                        row.source_utf16_range.start < offsets[1]
                            && offsets[0] < row.source_utf16_range.end
                    }))
                }
                Some(_) => None, // 旧投影既不能授权占位，也不能证明当前无占位；等待原准备完成。
            }
        }; // 借用结束后才复核原票，Native绘制不跨投影借用。
        Ok(self.current().then_some(requirement).flatten())
    }

    pub(super) fn capture(
        owner: &'a NativeMarkdownRasterSurface,
        view: &'a PieceBackedMarkdownTextView,
        permit: &'a NativeMarkdownRasterOriginPermit,
    ) -> Result<Option<Self>, String> {
        if !permit.current(owner, view) {
            return Ok(None);
        }
        let document_length = {
            let document = permit
                .storage
                .document()
                .ok_or("光栅原canonical文档已释放")?;
            let document = document
                .try_borrow()
                .map_err(|_| "光栅原canonical文档仍被独占")?;
            if document.generation() != permit.key.generation
                || document.selection() != permit.key.selection
            {
                return Ok(None); // 只读取原票的长度，不从后继代次或选择补领空文档权限。
            }
            document.utf16_len()
        }; // 长度固定后立刻归还Doc借用，后续所有Native原语仍消费原permit。
        let Some(window) = permit.step(owner, view, || view.window())? else {
            return Ok(None);
        };
        if !permit.step(owner, view, || window.isVisible())? {
            return Ok(None);
        }
        let manager = permit
            .step(owner, view, || owner.graph.manager.load())?
            .ok_or("光栅原 manager 已释放")?;
        let content = permit
            .step(owner, view, || manager.textContentManager())?
            .ok_or("光栅原 content manager 已释放")?;
        let controller = permit
            .step(owner, view, || {
                view.ivars().controller.get().and_then(Weak::load)
            })?
            .ok_or("光栅原 controller 缺失")?;
        if !native_markdown_controller_has_foreground_authority(&controller) {
            return Ok(None);
        }
        let visible = permit.step(owner, view, || view.visibleRect())?;
        let origin = permit.step(owner, view, || view.textContainerOrigin())?;
        let scroll = permit
            .step(owner, view, || view.enclosingScrollView())?
            .ok_or("光栅原 scroll 缺失")?;
        let clip_size = permit.step(owner, view, || scroll.contentSize())?;
        if !native_markdown_paint_viewport_is_valid(visible, origin, clip_size) {
            return Err("光栅真实可见视口无效".into());
        }
        let scale = permit.step(owner, view, || window.backingScaleFactor())?;
        geometry::dimensions(visible, scale)?;
        let viewport = permit.step(owner, view, || manager.textViewportLayoutController())?;
        let Some(range) = permit.step(owner, view, || viewport.viewportRange())? else {
            return Ok(None);
        };
        let terminal_viewport = permit.step(owner, view, || range.isEmpty())?;
        if terminal_viewport
            && !capture_terminal_viewport(owner, view, permit, &content, &range, document_length)?
        {
            return Ok(None); // 未布局的内部空viewport保持Pending，不能假发布空正文或清旧图。
        }
        let bounds = permit.step(owner, view, || view.bounds())?;
        let selection = permit.step(owner, view, || view.selectedRanges())?;
        let native = owner.native_owner()?;
        let layer = permit
            .step(owner, view, || native.image().layer())?
            .ok_or("光栅原 CALayer 缺失")?;
        let result = Self {
            owner,
            permit,
            view,
            manager,
            content,
            window,
            range,
            terminal_viewport,
            document_length,
            selection,
            visible,
            origin,
            bounds,
            scale,
            native,
            layer,
            fragment_epoch: owner.fragment_epoch.get(),
            published_image: Cell::new(None),
        };
        if !result.current() {
            return Err("光栅几何捕获期间原状态改变".into());
        }
        Ok(Some(result))
    }

    pub(super) fn step<T>(&self, work: impl FnOnce() -> T) -> Result<T, String> {
        if self.owner.fragment_epoch.get() != self.fragment_epoch
            || self.owner.counters.sticky_failure.load(Ordering::Acquire)
        {
            return Err("光栅已创建 fragment 集合在 Native 调用前改变".into());
        }
        let result = self.permit.step(self.owner, self.view, work)?;
        if self.owner.fragment_epoch.get() != self.fragment_epoch
            || self.owner.counters.sticky_failure.load(Ordering::Acquire)
        {
            return Err("光栅已创建 fragment 集合在 Native 调用后改变".into());
        }
        Ok(result)
    }

    pub(super) fn current(&self) -> bool {
        self.check_current().unwrap_or(false)
    }

    pub(super) fn bind_published_image(&self, image: &CGImage) {
        self.published_image.set(Some(NonNull::from(image))); // 只比身份，不拥有或解引用像素。
    }

    fn reject(&self, reason: &'static str) -> bool {
        let count = self
            .owner
            .counters
            .geometry_mismatch_logs
            .fetch_add(1, Ordering::AcqRel);
        if count < 8 {
            eprintln!("native_raster_surface event=current_rejected reason={reason} formal=false");
        }
        false // 原生读取次数与顺序不变，只给已有失败比较固定的无正文原因。
    }

    fn check_current(&self) -> Result<bool, String> {
        if !self.permit.current(self.owner, self.view) {
            return Ok(self.reject("original_ticket"));
        }
        if self.owner.fragment_epoch.get() != self.fragment_epoch {
            return Ok(self.reject("fragment_set"));
        }
        if self.owner.counters.sticky_failure.load(Ordering::Acquire) {
            return Ok(self.reject("prior_failure"));
        }
        if !self
            .step(|| self.view.window())?
            .is_some_and(|w| std::ptr::eq::<NSWindow>(&*w, &*self.window))
        {
            return Ok(self.reject("source_window"));
        }
        if !self
            .step(|| self.view.textLayoutManager())?
            .is_some_and(|m| std::ptr::eq::<NSTextLayoutManager>(&*m, &*self.manager))
        {
            return Ok(self.reject("source_manager"));
        }
        if !self
            .step(|| self.manager.textContentManager())?
            .is_some_and(|c| std::ptr::eq::<NSTextContentManager>(&*c, &*self.content))
        {
            return Ok(self.reject("source_content_manager"));
        }
        if self.step(|| self.view.visibleRect())? != self.visible {
            return Ok(self.reject("source_visible_rect"));
        }
        if self.step(|| self.view.bounds())? != self.bounds {
            return Ok(self.reject("source_bounds"));
        }
        if self.step(|| self.view.textContainerOrigin())? != self.origin {
            return Ok(self.reject("source_container_origin"));
        }
        if self.step(|| self.window.backingScaleFactor())? != self.scale {
            return Ok(self.reject("source_backing_scale"));
        }
        let selections = self.step(|| self.view.selectedRanges())?;
        if !self.step(|| selections.isEqualToArray(&self.selection))? {
            return Ok(self.reject("source_selection"));
        }
        let viewport = self.step(|| self.manager.textViewportLayoutController())?;
        let proxy = self
            .native
            .proxy
            .as_deref()
            .ok_or("光栅原viewport proxy缺失")?;
        let delegate = self.step(|| viewport.delegate())?;
        if !delegate.as_deref().is_some_and(|actual| {
            std::ptr::eq(
                std::ptr::from_ref::<
                    ProtocolObject<dyn objc2_app_kit::NSTextViewportLayoutControllerDelegate>,
                >(actual)
                .cast::<AnyObject>(),
                std::ptr::from_ref(proxy).cast::<AnyObject>(),
            )
        }) {
            return Ok(self.reject("source_viewport_proxy"));
        }
        let Some(range) = self.step(|| viewport.viewportRange())? else {
            return Ok(self.reject("source_viewport_missing"));
        };
        if !self.step(|| range.isEqualToTextRange(&self.range))? {
            return Ok(self.reject("source_viewport_range"));
        }
        let Some(scroll) = self.step(|| self.view.enclosingScrollView())? else {
            return Ok(self.reject("source_scroll_missing"));
        };
        let Some(clip) = self.step(|| self.owner.graph.clip.load())? else {
            return Ok(self.reject("source_clip_missing"));
        };
        let body: &NSView = self.view;
        if !self
            .step(|| scroll.documentView())?
            .is_some_and(|v| std::ptr::eq::<NSView>(&*v, body))
        {
            return Ok(self.reject("source_scroll_document"));
        }
        if !std::ptr::eq::<NSClipView>(&*self.step(|| scroll.contentView())?, &*clip) {
            return Ok(self.reject("source_scroll_clip"));
        }
        let image = self.native.image();
        if !self
            .step(|| {
                // SAFETY: 主线程自有 image 仍由本次 NativeOwners 租约保活，只比较原 superview 身份。
                unsafe { image.superview() }
            })?
            .is_some_and(|v| std::ptr::eq::<NSView>(&*v, body))
        {
            return Ok(self.reject("image_superview"));
        }
        if self.step(|| image.frame())? != self.visible {
            return Ok(self.reject("image_frame"));
        }
        if self.step(|| image.isHidden())? {
            return Ok(self.reject("image_hidden"));
        }
        if !self.step(|| image.wantsLayer())? {
            return Ok(self.reject("image_not_layer_backed"));
        }
        if !self
            .step(|| image.layer())?
            .is_some_and(|layer| std::ptr::eq::<CALayer>(&*layer, &*self.layer))
        {
            return Ok(self.reject("image_layer_identity"));
        }
        if self.step(|| self.layer.contentsScale())? != self.scale {
            return Ok(self.reject("image_backing_scale"));
        }
        if !self.permit.current(self.owner, self.view) {
            return Ok(self.reject("ticket_after_geometry"));
        }
        if self.owner.fragment_epoch.get() != self.fragment_epoch {
            return Ok(self.reject("fragment_set_after_geometry"));
        }
        if self.owner.counters.sticky_failure.load(Ordering::Acquire) {
            return Ok(self.reject("failure_after_geometry"));
        }
        if let Some(expected) = self.published_image.get() {
            // SAFETY: 自有层仅保存公开允许的 CGImage/None；只比身份，caller 保活同一同步 CGImage。
            let contents = self.step(|| unsafe { self.layer.contents() })?;
            let same = contents.as_deref().is_some_and(|actual| {
                std::ptr::eq(
                    std::ptr::from_ref(actual),
                    expected.as_ptr().cast::<AnyObject>(),
                )
            });
            drop(contents);
            if !same {
                return Ok(self.reject("published_image_identity"));
            }
        }
        if !self.permit.current(self.owner, self.view) {
            return Ok(self.reject("ticket_after_contents"));
        }
        if self.owner.fragment_epoch.get() != self.fragment_epoch {
            return Ok(self.reject("fragment_set_after_contents"));
        }
        if self.owner.counters.sticky_failure.load(Ordering::Acquire) {
            return Ok(self.reject("failure_after_contents"));
        }
        Ok(true)
    }
}

#[cfg(test)]
mod terminal_tests {
    use super::*;

    #[test]
    fn only_exact_canonical_empty_or_true_eof_is_a_terminal_viewport() {
        assert!(terminal_viewport_matches(0, 0, 0, 0));
        assert!(terminal_viewport_matches(71, 71, 71, 71));
        for (length, provider, start, end) in [
            (71, 71, 0, 0),
            (71, 71, 70, 70),
            (71, 70, 71, 71),
            (0, 1, 0, 0),
            (71, 71, 70, 71),
            (71, 71, 72, 71),
            (usize::MAX, usize::MAX, usize::MAX, usize::MAX),
        ] {
            assert!(!terminal_viewport_matches(length, provider, start, end));
        }
    }
}
