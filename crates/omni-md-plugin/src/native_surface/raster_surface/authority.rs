//! Fix source authority before the superclass callback; later capture adds only original Native geometry.
//! Geometry capture cannot adopt a newer document, presentation mode, selection, or input transaction.

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
    // A getter, setter, weak upgrade, or temporary release may synchronously deliver geometry events.
    // Check both the selection capture and the same pre-call epoch before allowing the next primitive.
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
    // These scalar identities cover every source/input/presentation fact that can change this paint.
    // The enclosing permit owns the original Native claims; this key owns no text or layout objects.
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

/// An empty viewport certifies completion only at canonical emptiness or true EOF, never an interior gap.
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
    // Round-trip the provider's end and both viewport endpoints through the original document origin.
    // Comparing empty ranges alone would let an unlaid-out interior viewport erase existing pixels.
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
    // A permit retains only these scalars; end the canonical document borrow before Native access.
    drop(document);
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

/// Lives only on the synchronous begin → superclass → consume stack.
/// Its Native claims cannot enter a persistent surface or callback.
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
        // Capture existing Rust authority before the first weak/Native operation. An unbound owner
        // or legitimate revocation cancels this attempt; neither can renew its permit or certify
        // that a first viewport with no layout has already painted.
        let Ok(capture) = view.ivars().folds.capture_selection() else {
            return Ok(None);
        };
        let result = (|| {
            // Upgrade the original graph under that capture; pointer identity prevents another
            // body using the same state values from acquiring this surface's publishing rights.
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
        // Revocation during construction is cancellation, even if a guarded getter reported an error.
        // Preserve structural errors only while the original source and geometry remain authoritative.
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

/// Adds Native geometry to this paint while every primitive consumes the permit fixed before super.
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
        // Diagnostics report the pre-super generation; they cannot authorize a successor document.
        self.permit.key.generation
    }

    /// Distinguishes blank source from hidden table rows using the original row-height projection.
    /// Missing ink alone cannot require a table overlay or authorize publication of an empty image.
    pub(super) fn table_overlay_requirement(&self) -> Result<Option<bool>, String> {
        if !self.current() {
            return Ok(None); // A successor projection cannot decide completeness for this revoked viewport.
        }
        if self.permit.key.mode == MarkdownPresentationMode::Source {
            return Ok(Some(false)); // Source mode has no placeholder heights; its complete source may be blank.
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
        } // Convert two Native endpoints only; do not read text or expand to paragraphs/the document.
        if offsets[0] > offsets[1] {
            return Err("占位检查的原视口范围反向".into());
        }
        let requirement = {
            let Ok(projection) = self.permit.storage.ivars().table_row_heights.try_borrow() else {
                return Ok(None); // Preparation already borrows this projection; cancel without another cache.
            };
            match projection.as_ref() {
                None => Some(false), // Without row heights the attribute producer retains literal source.
                Some(projection)
                    if projection.matches(
                        &self.owner.graph.document,
                        self.permit.key.generation,
                        self.permit.key.table_width,
                        self.permit.key.table_epoch,
                    ) =>
                {
                    Some(projection.rows.iter().any(|row| {
                        // Intersect bounded row-height metadata only. A zero-width terminal range
                        // needs its separate canonical EOF proof, rather than this overlap test.
                        row.source_utf16_range.start < offsets[1]
                            && offsets[0] < row.source_utf16_range.end
                    }))
                }
                Some(_) => None, // A stale projection proves neither placeholders nor their absence.
            }
        }; // End the projection borrow before revalidating authority or making any Native paint call.
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
                return Ok(None); // Read only the original permit's length; a successor cannot certify emptiness.
            }
            document.utf16_len()
        }; // Return the document borrow immediately; every later Native primitive uses the same permit.
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
        // Snapshot only the actual foreground window, viewport, and backing scale. Retained claims
        // keep this synchronous capture alive, but do not allow it to survive a changed Native relation.
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
            return Ok(None); // An unlaid-out interior range stays pending and cannot clear the old image.
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
        // A new fragment registration invalidates this exact batch even if the source key is unchanged.
        // A prior functional failure also prevents later Native work from publishing a partial image.
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
        // The caller keeps this synchronous CGImage alive; store identity only, never dereference pixels.
        self.published_image.set(Some(NonNull::from(image)));
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
        false // Preserve getter order; report only a fixed, content-free reason for the failed comparison.
    }

    fn check_current(&self) -> Result<bool, String> {
        // Reject scalar revocation before entering Native. Each subsequent getter still brackets
        // the same permit so synchronous reentry cannot turn this capture into newer geometry.
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
        // The original proxy and viewport range must still be installed. Matching dimensions alone
        // cannot authorize drawing after another delegate or layout has taken ownership.
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
        // Certify both ends of the scroll relation, including the exact original clip identity.
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
                // SAFETY: This main-thread image is retained by NativeOwners; compare its superview only.
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
        // Native claim destruction and the final geometry getter can revoke authority as well.
        // Repeat scalar checks before consulting the optional image that this operation published.
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
            // SAFETY: Our layer stores only supported CGImage/None contents. The caller retains
            // the same synchronous CGImage; compare identity without sending arbitrary messages to it.
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
        // Returning the temporary contents claim is another Native boundary; it cannot bypass
        // source/fragment/failure checks merely because the preceding identity comparison matched.
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
        // Empty source and exact EOF are valid; interior emptiness, provider disagreement, reversed
        // ranges, and offsets beyond Native's signed range cannot certify a completed viewport.
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
