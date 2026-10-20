//! 标准系统fragment负责正文；本模块只准备有限Preview矩形并在公开背景hook中绘制。
//!
//! 缓存不持正文、属性或Native布局对象；所有fragment强引用仅存在于prepare局部pool。
//! 模式/文档/语义/fold/视口任何变化均撤销旧矩形，绝不阻止super正文绘制。

use super::*;
use objc2_app_kit::{
    NSTextLayoutFragment, NSTextLayoutFragmentEnumerationOptions, NSTextLayoutFragmentState,
};
use objc2_core_graphics::CGContext;

#[cfg(feature = "native-block-fragment-diagnostic")]
pub(super) mod diagnostic;

/// 单次矩阵与请求窗口分别有硬上限；语义envelope另受core既有预算限制。
const MAX_PREVIEW_BLOCK_FRAGMENTS: usize = 256;
const MAX_PREVIEW_BLOCK_REQUEST_UTF16: usize = 16_384;

/// 布局与背景共用原分支优先级，table及已隐藏fence不会得到第二套padding。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NativeMarkdownPreviewBlockKind {
    Code,
    Quote,
    Rule,
}

impl NativeMarkdownPreviewBlockKind {
    /// 只从同代次有界语义提取种类，活动rule保留可编辑源语法。
    fn from_styles(styles: MarkdownPresentationStyles) -> Option<Self> {
        let hidden = styles.contains(MarkdownPresentationStyles::HIDDEN_MARKER);
        if hidden && styles.contains(MarkdownPresentationStyles::FENCED_CODE) {
            return None; // 隐藏fence不占代码padding或背景。
        }
        if styles.contains(MarkdownPresentationStyles::TABLE_ROW_PLACEHOLDER)
            || (hidden && styles.contains(MarkdownPresentationStyles::TABLE_DELIMITER))
        {
            return None; // 表格独占其行高与绘制，不能叠加块背景。
        }
        if hidden && styles.contains(MarkdownPresentationStyles::HORIZONTAL_RULE) {
            return Some(Self::Rule); // 只有隐藏语法的rule画细线。
        }
        if styles.contains(MarkdownPresentationStyles::FENCED_CODE) {
            return Some(Self::Code); // fenced code优先于内部类似quote的文字。
        }
        if styles.contains(MarkdownPresentationStyles::BLOCK_QUOTE) {
            return Some(Self::Quote); // 普通引用保留背景与1点边框。
        }
        None // 普通段落全部交给标准系统fragment。
    }
    /// 段距包含真实padding，rule的12点槽继续由原paragraph分支拥有。
    fn inset(self) -> f64 {
        match self {
            Self::Code => 6.0,  // 原代码块四边空隙。
            Self::Quote => 7.0, // 原6点padding与1点边框。
            Self::Rule => 0.0,  // 细线不增加缩进。
        }
    }
}

/// 支持的段落属性参与Native排版，不以绘图扩张伪造caret/软换行空间。
pub(super) fn apply_preview_block_insets(paragraph: &NSMutableParagraphStyle, inset: f64) {
    paragraph.setFirstLineHeadIndent(inset);
    paragraph.setHeadIndent(inset);
    paragraph.setTailIndent(-inset);
    // 保留标题/quote等已有外段距，只增加原fragment四边空隙。
    let before = paragraph.paragraphSpacingBefore();
    let after = paragraph.paragraphSpacing();
    paragraph.setParagraphSpacingBefore(before + inset);
    paragraph.setParagraphSpacing(after + inset);
}

/// 一次权限快照覆盖Source→Preview ABA及同代次selection/fold变化。
#[derive(Clone, Copy, PartialEq, Eq)]
struct BlockKey {
    generation: u64,
    semantic_epoch: u64,
    table_epoch: u64,
    fold_epoch: u64,
    mode: MarkdownPresentationMode,
}

/// 有限值缓存拥有的唯一Native类型只是Copy几何，不保留fragment/paragraph/字体。
#[derive(Clone, PartialEq)]
struct BlockGeometry {
    source: std::ops::Range<usize>,
    kind: NativeMarkdownPreviewBlockKind,
    frame: NSRect,
}

/// 一个真实可见行窗口只给相交fragment分配probe，完整段落长度不参与请求预算。
struct BlockSemanticRequest {
    window: std::ops::Range<usize>,
    visible_parts: Vec<Option<std::ops::Range<usize>>>,
    probes: Vec<usize>,
}

/// 缺真实源行等待原draw一次，超预算则沿既有稳定失败处理，二者不能混淆。
#[derive(Debug, PartialEq, Eq)]
enum BlockSemanticRequestFailure {
    MissingVisibleSource,
    Budget,
}

/// 只对已证明的实际可见源行和真实fragment求交；不截字数猜视口，也不扩大范围。
fn block_semantic_request(
    visible: &std::ops::Range<usize>,
    sources: &[std::ops::Range<usize>],
) -> Result<BlockSemanticRequest, BlockSemanticRequestFailure> {
    if visible.is_empty() || sources.is_empty() || sources.iter().any(std::ops::Range::is_empty) {
        return Err(BlockSemanticRequestFailure::MissingVisibleSource); // 无显示行或未知源不能伪装成空成功。
    }
    if visible.end - visible.start > MAX_PREVIEW_BLOCK_REQUEST_UTF16
        || sources.len() > MAX_PREVIEW_BLOCK_FRAGMENTS
    {
        return Err(BlockSemanticRequestFailure::Budget); // 仍使用原有窗口与矩阵硬上限，不增cap。
    }
    let mut parts = Vec::with_capacity(sources.len());
    let mut probes = Vec::with_capacity(sources.len());
    let mut window: Option<std::ops::Range<usize>> = None;
    for source in sources {
        let start = source.start.max(visible.start);
        let end = source.end.min(visible.end);
        if start >= end {
            parts.push(None);
            continue; // 几何边缘可能只露出段距，无源交集就不推断这一fragment的背景种类。
        }
        probes.push(start); // 每项只在自己的真实非空可见交集中查询，不能退回巨段落起点。
        parts.push(Some(start..end));
        window = Some(match window {
            Some(previous) => previous.start.min(start)..previous.end.max(end), // 所有端点仍在原可见源窗口中。
            None => start..end, // 第一项建立请求，不以0或全文边界作默认envelope。
        });
    }
    let window = window.ok_or(BlockSemanticRequestFailure::MissingVisibleSource)?;
    Ok(BlockSemanticRequest {
        window,
        visible_parts: parts,
        probes,
    })
}

/// 视口坐标属于准备时的同一布局；滚动/resize不能把旧屏幕矩形误用于新视口。
#[derive(Clone, Copy, PartialEq)]
struct BlockViewport {
    visible: NSRect,
    origin: NSPoint,
    view_width: f64,
}

/// 仅活动view拥有这一批背景；弱文档身份不能延长映射或历史寿命。
pub(super) struct NativeMarkdownPreviewBlocks {
    identity: WeakMarkdownDocument,
    key: BlockKey,
    viewport: BlockViewport,
    geometry: Vec<BlockGeometry>,
}

/// 准备权限由实际Native绘制事件推进；同key只有初次加一次后继机会。
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
enum BlockPreparationGate {
    #[default]
    Initial,
    AwaitNativeDraw,
    RetryReady,
    Complete,
    Failed,
}

impl BlockPreparationGate {
    /// 已完成与稳定失败不再排队；等待状态只由真实draw完成事件唤醒。
    fn can_prepare(self) -> bool {
        matches!(self, Self::Initial | Self::RetryReady)
    }
    /// 第一次MissingLayout等待一次真实绘制；第二次或永久失败都稳定停止。
    fn finished(self, prepared: bool, missing_layout: bool) -> Self {
        if prepared {
            return Self::Complete;
        } // 空geometry也是完整成功，不能等同缺失。
        if missing_layout && self == Self::Initial {
            return Self::AwaitNativeDraw;
        }
        Self::Failed // 预算/语义失败不重试，重试后仍缺几何也不以caret blink轮询。
    }
    /// 真实super绘制完成一次后允许后继；后续draw不能重置已失败或已完成状态。
    fn completed_native_draw(self) -> Self {
        match self {
            Self::AwaitNativeDraw => Self::RetryReady, // 一次实际layout/draw边界已发生。
            current => current,                        // 未等待/已经消费的状态不获得新准备权限。
        }
    }
}

/// 与缓存相同的弱身份和key；只记录有限准备状态，不持正文或Native图。
pub(super) struct NativeMarkdownPreviewBlockAttempt {
    identity: WeakMarkdownDocument,
    key: BlockKey,
    viewport: BlockViewport,
    gate: BlockPreparationGate,
}

impl PieceBackedMarkdownTextView {
    /// 只读Rust标量；本函数返回前结束canonical借用，不跨Native调用。
    fn preview_block_key(
        &self,
        storage: &PieceBackedMarkdownTextStorage,
    ) -> Option<(WeakMarkdownDocument, BlockKey)> {
        if self.ivars().textkit_graph_teardown.get()
            || !storage.ivars().accepts_edits.get()
            || self.ivars().textkit_input_depth.get() != 0
            || storage.ivars().composition_active.get()
            || self
                .ivars()
                .marked_text_range
                .get()
                .is_some_and(|range| range.length != 0)
        {
            return None; // 关闭/临时输入期间不能消费派生背景，原生字形仍正常绘制。
        }
        let document = storage.document()?;
        let generation = document.borrow().generation();
        Some((
            Rc::downgrade(&document),
            BlockKey {
                generation,
                semantic_epoch: storage.ivars().font_run_scratch.epoch(),
                table_epoch: storage.ivars().table_geometry_epoch.get(),
                fold_epoch: self.ivars().preview_block_fold_epoch.get(),
                mode: storage.presentation_mode(),
            },
        ))
    }

    /// 精确文档和所有代次均一致才可发布；相同整数generation不能跨笔记复用。
    fn preview_block_current(
        &self,
        storage: &PieceBackedMarkdownTextStorage,
        identity: &WeakMarkdownDocument,
        key: BlockKey,
    ) -> bool {
        self.preview_block_key(storage)
            .is_some_and(|(actual, current)| {
                WeakMarkdownDocument::ptr_eq(identity, &actual) && current == key
            })
    }

    /// 只取得视口自身的有限几何getter，不查询fragment、正文或触发ensure。
    fn preview_block_viewport(&self) -> BlockViewport {
        BlockViewport {
            visible: self.visibleRect(),
            origin: self.textContainerOrigin(),
            view_width: self.bounds().size.width,
        }
    }

    /// 只承认可见窗口中已挂载scroll的真实clip；首次隐藏构造不发布永久失败状态。
    pub(super) fn bounded_paint_viewport(&self) -> Option<(NSRect, NSPoint)> {
        let _payload = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if self.ivars().textkit_graph_teardown.get() {
            return None;
        }
        let window = self.window()?;
        if !window.isVisible() {
            return None;
        }
        let scroll = self.enclosingScrollView()?;
        let content_size = scroll.contentSize();
        let visible = {
            #[cfg(feature = "native-markdown-runtime-probe")]
            let _phase = super::viewport_reentry_diagnostic::enter(
                super::viewport_reentry_diagnostic::Phase::BoundedVisible,
            );
            let value = self.visibleRect();
            #[cfg(feature = "native-markdown-runtime-probe")]
            super::viewport_reentry_diagnostic::returned_rect(value);
            value
        };
        let origin = {
            #[cfg(feature = "native-markdown-runtime-probe")]
            let _phase = super::viewport_reentry_diagnostic::enter(
                super::viewport_reentry_diagnostic::Phase::BoundedOrigin,
            );
            let value = self.textContainerOrigin();
            #[cfg(feature = "native-markdown-runtime-probe")]
            super::viewport_reentry_diagnostic::returned_point(value);
            value
        };
        {
            #[cfg(feature = "native-markdown-runtime-probe")]
            let _phase = super::viewport_reentry_diagnostic::enter(
                super::viewport_reentry_diagnostic::Phase::BoundedDropScroll,
            );
            drop(scroll);
        }
        {
            #[cfg(feature = "native-markdown-runtime-probe")]
            let _phase = super::viewport_reentry_diagnostic::enter(
                super::viewport_reentry_diagnostic::Phase::BoundedDropWindow,
            );
            drop(window); // 不持Native临时claim跨返回；析构后再核sealed。
        }
        (!self.ivars().textkit_graph_teardown.get()
            && native_markdown_paint_viewport_is_valid(visible, origin, content_size))
        .then_some((visible, origin))
    }

    /// 纯标量取消旧请求，不分配/取得Native资源，也不以地址替代活身份。
    fn advance_preview_block_intent(&self) -> u64 {
        let next = self
            .ivars()
            .preview_block_intent_epoch
            .get()
            .wrapping_add(1);
        self.ivars().preview_block_intent_epoch.set(next);
        next
    }

    /// 接受关闭/模式切换同步take全部容量，旧cache drop不跨状态借用。
    pub(super) fn release_preview_blocks(&self) {
        self.advance_preview_block_intent();
        let obsolete = self.ivars().preview_blocks.borrow_mut().take();
        let attempt = self.ivars().preview_block_attempt.borrow_mut().take();
        drop((obsolete, attempt));
    }

    /// 原fold入口仍更新同一权限代次，标准fragment不再需要项目delegate。
    pub(super) fn advance_block_fold_epoch(&self) {
        self.ivars()
            .preview_block_fold_epoch
            .set(self.ivars().preview_block_fold_epoch.get().wrapping_add(1));
        self.release_preview_blocks();
    }

    /// table调度在取得cache借用前查询这一权限，不能只以table自身key消除背景工作。
    pub(super) fn preview_blocks_need_preparation(&self) -> bool {
        let Some(storage) = self.ivars().text_storage.load() else {
            return false; // 关闭后没有原storage，不可重建任何派生资源。
        };
        let Some((identity, key)) = self.preview_block_key(&storage) else {
            return false; // 输入/关闭期间由现有真实完成事件再决定，不另排重试。
        };
        if key.mode != MarkdownPresentationMode::InlinePreview {
            return false; // Source无需Preview背景。
        }
        let viewport = self.preview_block_viewport();
        if self.bounded_paint_viewport() != Some((viewport.visible, viewport.origin)) {
            return false; // 未挂载/瞬时非法clip只等待既有draw/viewDidMove，不写Failed也不重排。
        }
        if !self.preview_block_current(&storage, &identity, key) {
            return false; // Native getter重入后不接受旧请求。
        }
        if storage.source_publication_needs_preparation(self) {
            return true; // 一个延后的真实来源发布只由原settled executor消费，不在此推进Core。
        }
        self.ivars()
            .preview_block_attempt
            .borrow()
            .as_ref()
            .is_none_or(|attempt| {
                attempt.key != key
                    || attempt.viewport != viewport
                    || !WeakMarkdownDocument::ptr_eq(&attempt.identity, &identity)
                    || attempt.gate.can_prepare()
            })
    }

    /// 不新增dispatch/timer，仅将一个等待态推进为可由原scheduler消费的权限。
    pub(super) fn preview_blocks_completed_native_draw(&self) {
        if self.ivars().textkit_graph_teardown.get() {
            return; // 迟到super返回不能复活accepted后已取走的准备槽。
        }
        if let Some(attempt) = self.ivars().preview_block_attempt.borrow_mut().as_mut() {
            attempt.gate = attempt.gate.completed_native_draw(); // 纯标量转移，无Native调用跨借用。
        }
    }

    /// 只在既有settled executor执行，所有Native临时claims在本地pool返回前结束。
    pub(super) fn refresh_preview_blocks(&self) -> bool {
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if self.ivars().textkit_graph_teardown.get() {
            return false; // 迟到queue不读取Native图，也不恢复accepted释放的容量。
        }
        let Some(storage) = self.ivars().text_storage.load() else {
            return false;
        }; // 无原storage不恢复owner。
        let Some((identity, key)) = self.preview_block_key(&storage) else {
            return false;
        }; // 输入期间不准备。
        let viewport = self.preview_block_viewport();
        if self.bounded_paint_viewport() != Some((viewport.visible, viewport.origin)) {
            return false; // 准入在attempt发布前；合法挂载事件仍可首次准备。
        }
        if !self.preview_block_current(&storage, &identity, key) {
            return false;
        } // getter重入即失效。
        let gate = self
            .ivars()
            .preview_block_attempt
            .borrow()
            .as_ref()
            .filter(|attempt| {
                attempt.key == key
                    && attempt.viewport == viewport
                    && WeakMarkdownDocument::ptr_eq(&attempt.identity, &identity)
            })
            .map_or(BlockPreparationGate::Initial, |attempt| attempt.gate);
        if !gate.can_prepare() {
            return false;
        } // 真实draw尚未完成/已有结果/稳定失败均不得重排。
        let intent_epoch = self.advance_preview_block_intent();
        let missing_layout = Cell::new(false);
        let prepared = autoreleasepool(|_| self.prepare_preview_blocks(&missing_layout));
        let current_viewport = self.preview_block_viewport();
        if self.ivars().preview_block_intent_epoch.get() != intent_epoch
            || current_viewport != viewport
            || self.bounded_paint_viewport() != Some((viewport.visible, viewport.origin))
            || prepared.as_ref().is_some_and(|prepared| {
                prepared.viewport != viewport
                    || prepared.key != key
                    || !WeakMarkdownDocument::ptr_eq(&prepared.identity, &identity)
            })
            || !self.preview_block_current(&storage, &identity, key)
        {
            return false; // 旧失败/成功栈都不能修改Native重入后新批cache与准备状态。
        }
        let next_gate = gate.finished(prepared.is_some(), missing_layout.get());
        let previous_attempt =
            self.ivars()
                .preview_block_attempt
                .replace(Some(NativeMarkdownPreviewBlockAttempt {
                    identity: identity.clone(),
                    key,
                    viewport,
                    gate: next_gate,
                }));
        drop(previous_attempt); // weak/标量在无slot借用处drop。
        let Some(prepared) = prepared else {
            let obsolete = self.ivars().preview_blocks.borrow_mut().take();
            drop(obsolete);
            return false; // 失败本身不setNeedsDisplay；只等待真实draw一次，永久失败稳定停止。
        };
        let changed = self
            .ivars()
            .preview_blocks
            .borrow()
            .as_ref()
            .is_none_or(|old| {
                old.key != prepared.key
                    || old.viewport != prepared.viewport
                    || old.geometry != prepared.geometry
                    || !WeakMarkdownDocument::ptr_eq(&old.identity, &prepared.identity)
            });
        let obsolete = self.ivars().preview_blocks.replace(Some(prepared));
        drop(obsolete); // 原值只含weak/Copy/有限Vec，不持Native回调对象。
        changed
    }

    /// 取得真实可见系统fragment的有限临时claims；None options不要求新布局。
    fn prepare_preview_blocks(
        &self,
        missing_layout: &Cell<bool>,
    ) -> Option<NativeMarkdownPreviewBlocks> {
        let storage = self.ivars().text_storage.load()?;
        let (identity, key) = self.preview_block_key(&storage)?;
        if key.mode != MarkdownPresentationMode::InlinePreview {
            return None; // Source无Preview padding背景，旧缓存应立即移除。
        }
        let manager = self.textLayoutManager()?;
        let expected_manager = self.ivars().text_layout_manager.get()?.load()?;
        if !std::ptr::eq(&*manager, &*expected_manager) || manager.layoutQueue().is_some() {
            return None; // 只接受本图同步主线程布局，未知异步布局不能提供矩阵权限。
        }
        let viewport = self.preview_block_viewport();
        if self.bounded_paint_viewport() != Some((viewport.visible, viewport.origin)) {
            return None; // 在第一次fragment定位前拒绝伪视口，不用document bounds猜可见范围。
        }
        if [
            viewport.visible.origin.x,
            viewport.visible.origin.y,
            viewport.visible.size.width,
            viewport.visible.size.height,
            viewport.origin.x,
            viewport.origin.y,
            viewport.view_width,
        ]
        .iter()
        .any(|value| !value.is_finite())
            || viewport.visible.size.width <= 0.0
            || viewport.visible.size.height <= 0.0
            || viewport.view_width <= 0.0
        {
            return None; // 无效视口不发布NaN key，也不会以反复changed制造draw循环。
        }
        let container = manager.textContainer()?;
        let width = container.size().width;
        let line_padding = container.lineFragmentPadding();
        let body_width = (viewport.view_width - viewport.origin.x * 2.0).max(0.0);
        let top = (viewport.visible.origin.y - viewport.origin.y).max(0.0);
        let bottom = viewport.visible.origin.y + viewport.visible.size.height - viewport.origin.y;
        let first = manager
            .textLayoutFragmentForPosition(NSPoint::new(line_padding, top))
            .or_else(|| {
                missing_layout.set(true);
                None // 只允许下一次真实draw之后的一次后继。
            })?;
        let start = first.rangeInElement().location();
        let fragments: RefCell<Vec<Retained<NSTextLayoutFragment>>> = RefCell::new(Vec::new());
        let overflow = Cell::new(false);
        let visited = Cell::new(0_usize);
        let unavailable = Cell::new(false);
        let block = block2::RcBlock::new(|pointer: NonNull<NSTextLayoutFragment>| {
            // SAFETY: public同步枚举在回调期间借出活fragment；这里只增加一个局部+1 claim。
            let fragment = unsafe { pointer.as_ref() };
            visited.set(visited.get().saturating_add(1));
            if visited.get() > MAX_PREVIEW_BLOCK_FRAGMENTS {
                overflow.set(true);
                return objc2::runtime::Bool::NO; // 包括跳过的边缘段，禁止离屏callback无界推进。
            }
            if !self.preview_block_current(&storage, &identity, key) {
                unavailable.set(true);
                return objc2::runtime::Bool::NO; // 原生重入使本批整体失效。
            }
            if fragment.state() != NSTextLayoutFragmentState::LayoutAvailable {
                unavailable.set(true);
                missing_layout.set(true);
                return objc2::runtime::Bool::NO; // 不用estimated几何猜背景，也不ensure。
            }
            let frame = fragment.layoutFragmentFrame();
            if frame.origin.y >= bottom {
                return objc2::runtime::Bool::NO; // 已越过可见区，不枚举离屏paragraph。
            }
            if frame.origin.y + frame.size.height <= top {
                return objc2::runtime::Bool::YES; // 边缘查找可返回前一段，只跳过这一个真实已布局段。
            }
            if fragments.borrow().len() >= MAX_PREVIEW_BLOCK_FRAGMENTS {
                overflow.set(true);
                return objc2::runtime::Bool::NO; // 超预算丢整批，不把截断结果伪装完整视口。
            }
            // SAFETY: pointer是公开同步回调中的活对象，局部claim仅延续到本次矩阵准备结束。
            let Some(retained) = (unsafe { Retained::retain(pointer.as_ptr()) }) else {
                unavailable.set(true);
                return objc2::runtime::Bool::NO; // 不发布缺失claim的矩阵。
            };
            fragments.borrow_mut().push(retained); // 无Native调用跨越这个短可变借用。
            objc2::runtime::Bool::YES
        });
        manager.enumerateTextLayoutFragmentsFromLocation_options_usingBlock(
            Some(&start),
            NSTextLayoutFragmentEnumerationOptions::empty(),
            &block,
        );
        drop(block);
        if overflow.get()
            || unavailable.get()
            || !self.preview_block_current(&storage, &identity, key)
        {
            return None; // 未完成矩阵只在局部释放，不发布任何Native claim。
        }
        let fragments = fragments.into_inner();
        let mut matrix = Vec::with_capacity(fragments.len());
        for fragment in &fragments {
            let source = self.stable_fragment_source(fragment)?;
            let frame = fragment.layoutFragmentFrame();
            let top_margin = fragment.topMargin();
            let bottom_margin = fragment.bottomMargin();
            let actual = fragment.textLayoutManager()?;
            if source.is_empty() {
                missing_layout.set(true);
                return None; // 只有空源视觉行不能授予可见内容语义；保留原一次真实draw后继机会。
            }
            if !std::ptr::eq(&*actual, &*manager)
                || !self.preview_block_current(&storage, &identity, key)
            {
                return None; // 每项关联必须仍指向同一manager/文档，不能发布混合epoch矩阵。
            }
            matrix.push((source, frame, top_margin, bottom_margin));
        }
        let document = storage.document()?;
        let length = document.borrow().utf16_len();
        drop(document); // canonical借用和局部owner在Native真实行查询之前结束。
        let visible_source = self.read_visible_utf16_window(viewport.visible, length);
        if self.preview_block_viewport() != viewport
            || self.bounded_paint_viewport() != Some((viewport.visible, viewport.origin))
            || !self.preview_block_current(&storage, &identity, key)
        {
            return None; // 同key数字不能代替原owner和真实视口，getter重入后不发旧语义请求。
        }
        let semantic_request = match block_semantic_request(
            &visible_source,
            &matrix.iter().map(|item| item.0.clone()).collect::<Vec<_>>(),
        ) {
            Ok(request) => request,
            Err(BlockSemanticRequestFailure::MissingVisibleSource) => {
                missing_layout.set(true);
                return None; // 没有真实state3源行时保持MissingLayout，不把段落frame当作可见源。
            }
            Err(BlockSemanticRequestFailure::Budget) => {
                return None; // 原有真实视口预算仍约束请求；失败不能回退整paragraph读取。
            }
        };
        // 真实settled可见源在此接入唯一Core续体；下面的getter只消费已发布结果，不推进Global。
        storage.prepare_source_semantics(self, semantic_request.window.clone());
        let kinds = storage.preview_block_kinds(
            semantic_request.window.clone(),
            &semantic_request.probes,
            key.generation,
        )?;
        if kinds.len() != semantic_request.probes.len() {
            return None; // 一项一probe必须完整对应，不用zip截断制造缺失项的成功结果。
        }
        let mut kinds = kinds.into_iter();
        let mut geometry = Vec::with_capacity(matrix.len());
        let mut previous_end = 0;
        for ((fragment, first), visible_part) in fragments
            .iter()
            .zip(&matrix)
            .zip(&semantic_request.visible_parts)
        {
            // 本地强claim跨两次读取保持实际身份，不以drop后的地址相等替代活对象证明。
            let source = self.stable_fragment_source(fragment)?;
            let second = fragment.layoutFragmentFrame();
            let actual = fragment.textLayoutManager()?;
            if source != first.0
                || second != first.1
                || fragment.state() != NSTextLayoutFragmentState::LayoutAvailable
                || !std::ptr::eq(&*actual, &*manager)
                || source.start < previous_end
                || !self.preview_block_current(&storage, &identity, key)
            {
                missing_layout.set(true);
                return None; // range/frame/state/排序任何改变都丢整批，只许真实draw后一次后继。
            }
            previous_end = source.end;
            let Some(visible_part) = visible_part else {
                continue; // 二次完整source/frame认证仍执行，但无实际可见源的项绝不发布推断。
            };
            if visible_part.start < source.start
                || visible_part.end > source.end
                || visible_part.is_empty()
            {
                return None; // 准备后读取的真实源仍须完整包含原交集，不能把probe移到后继fragment。
            }
            let kind = kinds.next()?; // 只为非空可见交集消费其对应probe的已知语义结果。
            if let Some(kind) = kind {
                let frame = block_background_rect(
                    first.1,
                    viewport.origin,
                    width,
                    body_width,
                    line_padding,
                    (first.2 - kind.inset()).max(0.0),
                    (first.3 - kind.inset()).max(0.0),
                )?;
                geometry.push(BlockGeometry {
                    source,
                    kind,
                    frame,
                }); // 背景占正文列宽，不依赖glyph tight width。
            }
        }
        // 最后Native getter和所有局部fragment claims结束后，再做纯权限复核；cache只保值。
        let actual_viewport = self.preview_block_viewport();
        drop(fragments);
        drop(first);
        drop(container);
        drop(expected_manager);
        drop(manager);
        if actual_viewport != viewport || !self.preview_block_current(&storage, &identity, key) {
            return None; // drop/geometry getter重入后的旧矩形不能进入新会话。
        }
        Some(NativeMarkdownPreviewBlocks {
            identity,
            key,
            viewport,
            geometry,
        })
    }

    /// 标准背景完成后只读有限Copy缓存，不枚举、layout、解析或取得任何正文字符串。
    pub(super) fn draw_preview_blocks(&self, dirty: NSRect) {
        if self.ivars().textkit_graph_teardown.get() {
            return; // accepted后迟到paint不取得任何旧图claim，只保留外层super行为。
        }
        let Some(storage) = self.ivars().text_storage.load() else {
            return; // accepted清理后的迟到背景只保留super行为。
        };
        let viewport = self.preview_block_viewport();
        if self.bounded_paint_viewport() != Some((viewport.visible, viewport.origin)) {
            return; // 只省略自有背景，不拦截或裁掉super glyph绘制。
        }
        let prepared = {
            let cache = self.ivars().preview_blocks.borrow();
            cache
                .as_ref()
                .filter(|cache| {
                    cache.viewport == viewport
                        && self.preview_block_current(&storage, &cache.identity, cache.key)
                })
                .map(|cache| (cache.identity.clone(), cache.key, cache.geometry.clone()))
        };
        let Some((identity, key, geometry)) = prepared else {
            return; // 缓存缺失/Source/ABA只省略派生背景，native glyph从不受此早退影响。
        };
        let Ok(paint_capture) = self.ivars().folds.capture_selection() else {
            return; // 实际绘图固定原选择票，后续Native回调不能改用后继selection继续旧批。
        };
        let Some(context) = NSGraphicsContext::currentContext() else {
            return; // 无graphics context时不构造替代渲染面。
        };
        let context = context.CGContext();
        let current_viewport = self.preview_block_viewport();
        if current_viewport != viewport
            || !paint_capture.current()
            || !self.preview_block_current(&storage, &identity, key)
        {
            return; // context/viewport getter重入后须同时重核几何与最终纯权限，纯resize也不能漏过。
        }
        CGContext::save_g_state(Some(&context));
        for item in geometry {
            if !paint_capture.current() || !self.preview_block_current(&storage, &identity, key) {
                break; // 原selection、owner或任何key变化只结束旧批，外层仍恢复graphics状态。
            }
            let Some(paint) = native_markdown_paint_rect(item.frame, dirty, viewport.visible)
            else {
                continue; // 未受损区域无绘图工作，全部矩形均来自先前已验证矩阵。
            };
            paint_preview_block(item.kind, item.frame, paint, &context);
            let after_paint_viewport = self.preview_block_viewport();
            if after_paint_viewport != viewport
                || !paint_capture.current()
                || !self.preview_block_current(&storage, &identity, key)
            {
                break; // Native绘制返回后实际几何/原票失效，不能登记成功或画下一块；不能早退漏restore。
            }
            #[cfg(feature = "native-block-fragment-diagnostic")]
            diagnostic::painted(
                item.kind as u8,
                item.frame.size.width,
                item.frame.size.height,
            );
        }
        CGContext::restore_g_state(Some(&context));
        #[cfg(feature = "native-block-fragment-diagnostic")]
        storage.ivars().mode_attribute_diagnostic.report(2);
    }

    #[cfg(feature = "native-block-fragment-diagnostic")]
    /// 诊断只读取现有视口range，不layout，不记录文本/路径/地址。
    pub(super) fn begin_mode_attribute_diagnostic(&self) {
        if let Some(storage) = self.ivars().text_storage.load() {
            let length = storage
                .document()
                .map_or(0, |document| document.borrow().utf16_len());
            let visible = self.visibleRect();
            let viewport = self.read_visible_utf16_window(visible, length);
            storage.ivars().mode_attribute_diagnostic.begin(viewport);
        }
    }
}

impl PieceBackedMarkdownTextStorage {
    /// 同批可见probes共用一次有界语义窗口，结束正文借用后才返回纯种类。
    fn preview_block_kinds(
        &self,
        window: std::ops::Range<usize>,
        probes: &[usize],
        generation: u64,
    ) -> Option<Vec<Option<NativeMarkdownPreviewBlockKind>>> {
        if window.is_empty()
            || window.end - window.start > MAX_PREVIEW_BLOCK_REQUEST_UTF16
            || probes.is_empty()
            || probes.len() > MAX_PREVIEW_BLOCK_FRAGMENTS
            || probes.iter().any(|probe| !window.contains(probe))
        {
            return None; // 只有实际可见交集内的有限probes可查询，不接受空窗口或屏外paragraph起点。
        }
        let document = self.document()?;
        if document.borrow().generation() != generation
            || self.presentation_mode() != MarkdownPresentationMode::InlinePreview
        {
            return None; // 在纯查询之前先守原背景请求，不能把后继Core结果补给旧矩阵。
        }
        let known: Vec<_> = probes
            .iter()
            .map(|index| {
                self.source_leaf_query(*index)
                    .map(|query| NativeMarkdownPreviewBlockKind::from_styles(query.styles))
            })
            .collect();
        if known.iter().all(Option::is_some) {
            return known.into_iter().collect();
        } // 每个probe均来自同一活来源Ready时，完全跳过legacy envelope；None背景仍是已知结果。
        let rebuild = {
            let presentation = self.ivars().presentation.borrow();
            presentation.needs_rebuild(generation, window.start)
                || presentation.needs_rebuild(generation, window.end.saturating_sub(1))
        };
        if rebuild {
            // 此处只约束传入的真实可见请求；legacy物化仍可能扩到完整block，不能据此宣称整条路径无大复制。
            let slice = document
                .borrow()
                .materialize_markdown_viewport(window, MARKDOWN_PRESENTATION_OVERSCAN_UTF16_UNITS)
                .ok()?;
            let mut presentation = self.ivars().presentation.borrow_mut();
            match slice.semantic_status {
                MarkdownViewportSemanticStatus::Complete => {
                    presentation
                        .rebuild(generation, slice.utf16_range, &slice.text)
                        .ok()?; // 仅完整有限上下文授予隐藏语法背景。
                }
                MarkdownViewportSemanticStatus::SourceFallback => {
                    presentation
                        .record_source_fallback_window(generation, slice.utf16_range)
                        .ok()?;
                    return None; // 不完整上下文保留可读source，不猜code/quote种类。
                }
            }
        }
        if document.borrow().generation() != generation
            || self.presentation_mode() != MarkdownPresentationMode::InlinePreview
        {
            return None; // 在途旧语义不能发布到新代次/Source。
        }
        let presentation = self.ivars().presentation.borrow();
        probes
            .iter()
            .zip(known)
            .map(|(index, known)| {
                if let Some(kind) = known {
                    return Some(kind); // 混合请求中已证Code/Rule优先复用原Ready，不以legacy重解释它。
                }
                // query缺失是未知语义，不等同已知普通段落的None背景；整批必须明确失败。
                presentation
                    .query(*index)
                    .map(|query| NativeMarkdownPreviewBlockKind::from_styles(query.styles))
            })
            .collect()
    }
}

/// 纯正文列矩形；glyph极窄、行内缩进、滚动坐标均不改变完整背景宽度。
fn block_background_rect(
    frame: NSRect,
    origin: NSPoint,
    container_width: f64,
    body_width: f64,
    line_padding: f64,
    outside_top: f64,
    outside_bottom: f64,
) -> Option<NSRect> {
    if [
        frame.origin.y,
        frame.size.height,
        origin.x,
        origin.y,
        container_width,
        body_width,
        line_padding,
        outside_top,
        outside_bottom,
    ]
    .iter()
    .any(|value| !value.is_finite())
        || container_width <= 0.0
        || body_width <= 0.0
        || line_padding < 0.0
        || outside_top < 0.0
        || outside_bottom < 0.0
    {
        return None; // 非有限/负尺寸不得扩大成异常绘图平面。
    }
    let width = container_width.min(body_width) - line_padding * 2.0;
    let height = frame.size.height - outside_top - outside_bottom;
    if width <= 0.0 || height <= 0.0 {
        return None; // 过窄列或无内容槽不猜背景，不影响标准文字。
    }
    Some(NSRect::new(
        NSPoint::new(
            origin.x + line_padding,
            origin.y + frame.origin.y + outside_top,
        ),
        NSSize::new(width, height),
    ))
}

/// 可见区必须受真实clip尺寸约束；浮点误差只允许绝对百万分之一pt，不按巨值比例放宽。
pub(super) fn native_markdown_paint_viewport_is_valid(
    visible: NSRect,
    origin: NSPoint,
    clip_size: NSSize,
) -> bool {
    const EPSILON: f64 = 0.000_001;
    finite_positive_rect(visible)
        && [
            origin.x,
            origin.y,
            clip_size.width,
            clip_size.height,
            visible.origin.x - origin.x,
            visible.origin.y - origin.y,
            visible.origin.x + visible.size.width - origin.x,
            visible.origin.y + visible.size.height - origin.y,
        ]
        .iter()
        .all(|value| value.is_finite())
        && clip_size.width > 0.0
        && clip_size.height > 0.0
        && visible.size.width <= clip_size.width + EPSILON
        && visible.size.height <= clip_size.height + EPSILON
}

/// 只算Copy矩形交集；不改原fragment/语义/命中范围，也不访问Native绘图上下文。
pub(super) fn native_markdown_paint_rect(
    frame: NSRect,
    dirty: NSRect,
    visible: NSRect,
) -> Option<NSRect> {
    if ![frame, dirty, visible]
        .into_iter()
        .all(finite_positive_rect)
    {
        return None;
    }
    let x = frame.origin.x.max(dirty.origin.x).max(visible.origin.x);
    let y = frame.origin.y.max(dirty.origin.y).max(visible.origin.y);
    let right = (frame.origin.x + frame.size.width)
        .min(dirty.origin.x + dirty.size.width)
        .min(visible.origin.x + visible.size.width);
    let bottom = (frame.origin.y + frame.size.height)
        .min(dirty.origin.y + dirty.size.height)
        .min(visible.origin.y + visible.size.height);
    (right > x && bottom > y)
        .then(|| NSRect::new(NSPoint::new(x, y), NSSize::new(right - x, bottom - y)))
}

fn finite_positive_rect(rect: NSRect) -> bool {
    let right = rect.origin.x + rect.size.width;
    let bottom = rect.origin.y + rect.size.height;
    [
        rect.origin.x,
        rect.origin.y,
        rect.size.width,
        rect.size.height,
        right,
        bottom,
    ]
    .iter()
    .all(|value| value.is_finite())
        && rect.size.width > 0.0
        && rect.size.height > 0.0
        && right > rect.origin.x
        && bottom > rect.origin.y
}

/// 原引用框四边各自保留位置；裁剪只缩短已有边段，不在viewport顶部/底部伪造横边。
fn clipped_quote_edges(frame: NSRect, paint: NSRect) -> [Option<(NSPoint, NSPoint)>; 4] {
    let mut result = [None; 4];
    if !finite_positive_rect(frame)
        || !finite_positive_rect(paint)
        || frame.size.width < 1.0
        || frame.size.height < 1.0
    {
        return result;
    }
    let left = frame.origin.x + 0.5;
    let right = frame.origin.x + frame.size.width - 0.5;
    let top = frame.origin.y + 0.5;
    let bottom = frame.origin.y + frame.size.height - 0.5;
    // 线宽1pt的中心线允许距clip半点，实际着色仍由后面的显式context clip限制。
    let x0 = paint.origin.x - 0.5;
    let x1 = paint.origin.x + paint.size.width + 0.5;
    let y0 = paint.origin.y - 0.5;
    let y1 = paint.origin.y + paint.size.height + 0.5;
    for (index, y) in [top, bottom].into_iter().enumerate() {
        if y >= y0 && y <= y1 && left.max(x0) < right.min(x1) {
            result[index] = Some((
                NSPoint::new(left.max(x0), y),
                NSPoint::new(right.min(x1), y),
            ));
        }
    }
    for (index, x) in [left, right].into_iter().enumerate() {
        if x >= x0 && x <= x1 && top.max(y0) < bottom.min(y1) {
            result[index + 2] = Some((
                NSPoint::new(x, top.max(y0)),
                NSPoint::new(x, bottom.min(y1)),
            ));
        }
    }
    result
}

/// 原块palette；有效绘图clip与每个填充请求都受viewport∩dirty约束，不创建离屏缓存。
fn paint_preview_block(
    kind: NativeMarkdownPreviewBlockKind,
    frame: NSRect,
    paint: NSRect,
    context: &CGContext,
) {
    CGContext::save_g_state(Some(context));
    CGContext::clip_to_rect(Some(context), paint);
    match kind {
        NativeMarkdownPreviewBlockKind::Code => {
            CGContext::set_rgb_fill_color(Some(context), 0.13, 0.15, 0.20, 1.0);
            CGContext::fill_rect(Some(context), paint); // 只填当前可见且损坏的正文列部分。
        }
        NativeMarkdownPreviewBlockKind::Quote => {
            CGContext::set_rgb_fill_color(Some(context), 0.10, 0.12, 0.16, 1.0);
            CGContext::fill_rect(Some(context), paint);
            CGContext::set_rgb_stroke_color(Some(context), 0.28, 0.34, 0.44, 1.0);
            CGContext::set_line_width(Some(context), 1.0);
            // 四边独立裁剪后用方端补齐原直角角点；新裁剪端仍被同一paint clip截住。
            CGContext::set_line_cap(Some(context), objc2_core_graphics::CGLineCap::Square);
            CGContext::begin_path(Some(context));
            for (start, end) in clipped_quote_edges(frame, paint).into_iter().flatten() {
                CGContext::move_to_point(Some(context), start.x, start.y);
                CGContext::add_line_to_point(Some(context), end.x, end.y);
            }
            CGContext::stroke_path(Some(context)); // 仍只有原框边，路径各段也局限在clip外半点内。
        }
        NativeMarkdownPreviewBlockKind::Rule => {
            CGContext::set_rgb_fill_color(Some(context), 0.28, 0.34, 0.44, 1.0);
            let rule = NSRect::new(frame.origin, NSSize::new(frame.size.width, 1.0));
            if let Some(line) = native_markdown_paint_rect(rule, paint, paint) {
                CGContext::fill_rect(Some(context), line); // 原12点槽内细线位置不随viewport改变。
            }
        }
    }
    CGContext::restore_g_state(Some(context));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_code_fragment_queries_only_its_actual_visible_lines() {
        let complete_fragment = 100..18_101;
        let visible_lines = 4_200..4_860;
        let request =
            block_semantic_request(&visible_lines, std::slice::from_ref(&complete_fragment))
                .unwrap();
        assert_eq!(request.window, visible_lines);
        assert_eq!(request.probes, [4_200]); // 不再用18,000字符物理段落的起点或全文envelope作请求。
        assert_eq!(request.visible_parts, [Some(4_200..4_860)]);
        assert!(complete_fragment.end - complete_fragment.start > MAX_PREVIEW_BLOCK_REQUEST_UTF16);
        assert!(request.window.end - request.window.start < MAX_PREVIEW_BLOCK_REQUEST_UTF16);
    }

    #[test]
    fn visible_intersections_keep_probe_slots_and_ignore_spacing_only_fragments() {
        let request =
            block_semantic_request(&(90..230), &[0..100, 100..200, 200..300, 400..500]).unwrap();
        assert_eq!(request.window, 90..230);
        assert_eq!(request.probes, [90, 100, 200]);
        assert_eq!(
            request.visible_parts,
            [Some(90..100), Some(100..200), Some(200..230), None]
        );
        // 第四项即使frame因段距擦过clip，也没有原生可见源行，不消费probe或推断背景。
        let middle =
            block_semantic_request(&(4_200..4_860), &[0..10, 100..18_101, 20_000..20_010]).unwrap();
        assert_eq!(middle.probes, [4_200]);
        assert_eq!(middle.visible_parts, [None, Some(4_200..4_860), None]);
    }

    #[test]
    fn missing_visible_source_cannot_be_empty_ready_and_real_window_cap_is_unchanged() {
        for (visible, source) in [(0..0, 0..18_001), (500..600, 0..100), (10..20, 10..10)] {
            assert_eq!(
                block_semantic_request(&visible, std::slice::from_ref(&source)).err(),
                Some(BlockSemanticRequestFailure::MissingVisibleSource)
            );
        }
        let long_source = 0..18_001;
        let actual_too_large = 0..MAX_PREVIEW_BLOCK_REQUEST_UTF16 + 1;
        assert_eq!(
            block_semantic_request(&actual_too_large, std::slice::from_ref(&long_source)).err(),
            Some(BlockSemanticRequestFailure::Budget)
        );
        let at_cap = 10..10 + MAX_PREVIEW_BLOCK_REQUEST_UTF16;
        assert_eq!(
            block_semantic_request(&at_cap, std::slice::from_ref(&long_source))
                .unwrap()
                .window,
            at_cap
        );
        let too_many = vec![0..1; MAX_PREVIEW_BLOCK_FRAGMENTS + 1];
        assert_eq!(
            block_semantic_request(&(0..1), &too_many).err(),
            Some(BlockSemanticRequestFailure::Budget)
        );
    }

    #[test]
    fn giant_paragraph_paints_only_the_real_visible_damage_intersection() {
        let frame = NSRect::new(
            NSPoint::new(0.0, -1_000_000.0),
            NSSize::new(1_000.0, 2_000_000.0),
        );
        let dirty = NSRect::new(NSPoint::new(100.0, 80.0), NSSize::new(600.0, 20.0));
        let visible = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(800.0, 500.0));
        assert_eq!(
            native_markdown_paint_rect(frame, dirty, visible),
            Some(dirty)
        );
        let offscreen = NSRect::new(NSPoint::new(0.0, 600.0), NSSize::new(800.0, 50.0));
        assert!(native_markdown_paint_rect(frame, offscreen, visible).is_none());
        // 同一纯交集也用于ruler坐标；正文滚动offset不能改变ruler自身的横向范围。
        let ruler = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(74.0, 500.0));
        let line = NSRect::new(NSPoint::new(0.0, -100_000.0), NSSize::new(74.0, 200_000.0));
        assert_eq!(native_markdown_paint_rect(line, ruler, ruler), Some(ruler));
    }

    #[test]
    fn viewport_admission_uses_actual_clip_and_rejects_nonfinite_or_fictional_area() {
        let visible = NSRect::new(NSPoint::new(0.0, 1_000_000.0), NSSize::new(871.0, 638.0));
        let origin = NSPoint::new(36.0, 32.0);
        let clip = visible.size;
        assert!(native_markdown_paint_viewport_is_valid(
            visible, origin, clip
        ));
        let mut almost = visible;
        almost.size.width += 0.000_000_5;
        assert!(native_markdown_paint_viewport_is_valid(
            almost, origin, clip
        ));
        for size in [
            NSSize::new(871.1, 638.0),
            NSSize::new(871.0, 638.1),
            NSSize::new(f64::MAX, f64::MAX),
            NSSize::new(f64::INFINITY, 638.0),
            NSSize::new(871.0, f64::NAN),
            NSSize::new(0.0, 638.0),
        ] {
            assert!(!native_markdown_paint_viewport_is_valid(
                NSRect::new(visible.origin, size),
                origin,
                clip
            ));
        }
        assert!(!native_markdown_paint_viewport_is_valid(
            visible,
            NSPoint::new(f64::NAN, 32.0),
            clip
        ));
        let overflow = NSRect::new(NSPoint::new(f64::MAX, 0.0), NSSize::new(f64::MAX, 638.0));
        assert!(!native_markdown_paint_viewport_is_valid(
            overflow, origin, clip
        ));
        let tall = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(871.0, f64::MAX * 0.75));
        assert!(!native_markdown_paint_viewport_is_valid(
            tall,
            NSPoint::new(0.0, -f64::MAX * 0.75),
            tall.size,
        )); // 两端各自有限也不代表转到container坐标后的末端有限。
        let invalid = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(-1.0, 100.0));
        assert!(native_markdown_paint_rect(invalid, visible, visible).is_none());
    }

    #[test]
    fn clipped_quote_keeps_only_real_edges_without_viewport_crossbars() {
        let frame = NSRect::new(NSPoint::new(10.0, -10_000.0), NSSize::new(200.0, 20_000.0));
        let paint = NSRect::new(NSPoint::new(10.0, 100.0), NSSize::new(200.0, 50.0));
        let edges = clipped_quote_edges(frame, paint);
        assert!(edges[0].is_none() && edges[1].is_none()); // 中段没有原顶部/底部，不能生成假横边。
        assert_eq!(
            edges[2],
            Some((NSPoint::new(10.5, 99.5), NSPoint::new(10.5, 150.5)))
        );
        assert_eq!(
            edges[3],
            Some((NSPoint::new(209.5, 99.5), NSPoint::new(209.5, 150.5)))
        );
        let inner = NSRect::new(NSPoint::new(50.0, 100.0), NSSize::new(100.0, 50.0));
        assert!(
            clipped_quote_edges(frame, inner)
                .into_iter()
                .all(|edge| edge.is_none())
        );
        let corner_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(300.0, 1_000.0));
        let corner = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 100.0));
        let edges = clipped_quote_edges(corner_frame, corner);
        assert_eq!(
            edges[0],
            Some((NSPoint::new(0.5, 0.5), NSPoint::new(100.5, 0.5)))
        );
        assert!(edges[1].is_none() && edges[3].is_none());
        assert!(edges[2].is_some()); // 左上真实角仍由两原边共同覆盖。
    }
    #[test]
    fn tiny_rule_and_shifted_quote_use_full_body_column() {
        let a = NSRect::new(NSPoint::new(5.0, 40.0), NSSize::new(0.15, 24.0));
        let b = NSRect::new(NSPoint::new(125.0, 40.0), a.size);
        let origin = NSPoint::new(18.0, 18.0);
        let expected = NSRect::new(NSPoint::new(23.0, 64.0), NSSize::new(500.0, 12.0));
        assert_eq!(
            block_background_rect(a, origin, 510.0, 510.0, 5.0, 6.0, 6.0),
            Some(expected)
        );
        assert_eq!(
            block_background_rect(b, origin, 1000000.0, 510.0, 5.0, 6.0, 6.0),
            Some(expected)
        );
    }
    #[test]
    fn rejects_invalid_or_exhausted_background_geometry() {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 24.0));
        for width in [f64::NAN, f64::INFINITY, -1.0, 0.0, 5.0] {
            assert!(
                block_background_rect(frame, NSPoint::new(0.0, 0.0), width, 510.0, 5.0, 0.0, 0.0)
                    .is_none()
            );
        }
        assert!(
            block_background_rect(frame, NSPoint::new(0.0, 0.0), 510.0, 510.0, 5.0, 12.0, 12.0)
                .is_none()
        );
    }
    #[test]
    fn hidden_fence_and_active_rule_keep_native_source_priority() {
        let text = "中文😀\n\n> quote\n\n```rust\nlet x = 1;\n```\n\n---\n";
        let offset = |needle: &str| text[..text.find(needle).unwrap()].encode_utf16().count();
        let mut presentation = MarkdownViewportPresentation::default();
        presentation
            .rebuild(0, 0..text.encode_utf16().count(), text)
            .unwrap();
        // 用真实parser组合位而非手工构造，实现隐藏fence/代码/引用的分支合同。
        let kind = |presentation: &MarkdownViewportPresentation, needle| {
            NativeMarkdownPreviewBlockKind::from_styles(
                presentation.query(offset(needle)).unwrap().styles,
            )
        };
        assert_eq!(kind(&presentation, "```rust"), None);
        assert_eq!(
            kind(&presentation, "quote"),
            Some(NativeMarkdownPreviewBlockKind::Quote)
        );
        assert_eq!(
            kind(&presentation, "let x"),
            Some(NativeMarkdownPreviewBlockKind::Code)
        );
        assert_eq!(
            kind(&presentation, "---"),
            Some(NativeMarkdownPreviewBlockKind::Rule)
        );
        presentation.set_active_selection(offset("---")..offset("---"));
        presentation
            .rebuild(0, 0..text.encode_utf16().count(), text)
            .unwrap();
        assert_eq!(kind(&presentation, "---"), None); // caret所在rule必须可见可编辑，不能被细线代替。
    }
    #[test]
    fn missing_layout_waits_for_real_draw_and_has_only_one_successor() {
        let first = BlockPreparationGate::Initial.finished(false, true);
        assert_eq!(first, BlockPreparationGate::AwaitNativeDraw);
        assert!(!first.can_prepare());
        let ready = first.completed_native_draw();
        assert!(ready.can_prepare());
        let failed = ready.finished(false, true);
        assert_eq!(failed, BlockPreparationGate::Failed);
        for _ in 0..8 {
            assert!(!failed.completed_native_draw().can_prepare()); // caret/native额外绘制不能变成轮询。
        }
        assert_eq!(ready.finished(true, false), BlockPreparationGate::Complete); // 第二批成功可正常发布。
    }
    #[test]
    fn permanent_budget_failure_and_valid_empty_result_are_stable() {
        let failed = BlockPreparationGate::Initial.finished(false, false);
        assert!(!failed.completed_native_draw().can_prepare());
        let empty_ready = BlockPreparationGate::Initial.finished(true, false);
        assert_eq!(empty_ready, BlockPreparationGate::Complete);
        assert!(!empty_ready.completed_native_draw().can_prepare()); // 空装饰也是有效完成，不需额外Native布局。
    }
}
