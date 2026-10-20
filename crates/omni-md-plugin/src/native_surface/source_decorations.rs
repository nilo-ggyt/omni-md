//! 原可见窗口的Find高亮、括号与控制字符提示，共用settled TextKit准备和原装饰Vec。
//!
//! 绘制只消费有限Copy矩形；不在paint中读取源码、构造另一份正文或强制离屏排版。

use super::*;
use objc2_app_kit::{
    NSAttributedStringNSStringDrawing, NSTextLayoutManagerSegmentOptions,
    NSTextLayoutManagerSegmentType,
};
use omni_markdown_core::markdown_brackets::{MarkdownBracketQuery, markdown_bracket_match};

// 独立参考图也沿正式可见 Find 的原配额，不在旧装饰模块重复定义数值。
use super::find_bar::{MAX_DECORATION_VIEWPORT_UNITS, MAX_SPECIAL_CHARACTER_MARKERS};

/// 原装饰Vec统一归还有限提示和Find矩形；批次仅保弱票，不持正文或Native布局对象。
#[derive(Clone)]
pub(super) enum NativeMarkdownDecorationGeometry {
    Source {
        frame: NSRect,
        matched: Option<bool>,
    },
    Find(Rc<NativeMarkdownFindHighlightBatch>),
    PreviewVisibility(Rc<NativeMarkdownPreviewVisibility>),
}

/// 原装饰批次只保实际可见范围的样式事实；不持解析快照、正文、Native 对象或 Find 查询。
pub(super) struct NativeMarkdownPreviewVisibility {
    document: WeakMarkdownDocument,
    selection: MarkdownTextSelection,
    semantic_epoch: u64,
    storage_input: u64,
    view_input: u64,
    folds: u64,
    font: u64,
    key: NativeMarkdownTableLayoutRequestKey,
    runs: Vec<omni_markdown_core::MarkdownPresentationQuery>,
}

impl NativeMarkdownPreviewVisibility {
    /// 纯语义不依赖 Find intent；正文、选择、模式、折叠或原输入变更仍立即撤销这份事实。
    fn current(
        &self,
        view: &PieceBackedMarkdownTextView,
        storage: &PieceBackedMarkdownTextStorage,
    ) -> bool {
        if view.ivars().textkit_graph_teardown.get()
            || view.ivars().textkit_input_depth.get() != 0
            || !storage.ivars().accepts_edits.get()
            || storage.ivars().composition_active.get()
            || storage.presentation_mode() != MarkdownPresentationMode::InlinePreview
            || storage.ivars().presentation_semantic_epoch.get() != Some(self.semantic_epoch)
            || storage.ivars().input_transaction_epoch.get() != self.storage_input
            || storage.ivars().font_run_scratch.epoch() != self.font
            || storage.ivars().table_geometry_epoch.get() != self.key.table_geometry_epoch
            || storage.ivars().table_projection_width_key.get() != self.key.text_view_width
            || view.ivars().table_input_intent_epoch.get() != self.view_input
            || view.ivars().folds.revision() != self.folds
            || !WeakMarkdownDocument::ptr_eq(&self.document, &storage.ivars().document.borrow())
        {
            return false; // 活着的旧范围不能借同数值代次授权后继正文或不同几何。
        }
        self.document.upgrade().is_some_and(|document| {
            let source_matches = view.ivars().folds.matches_document(&document);
            let document = document.borrow();
            source_matches
                && document.generation() == self.key.document_generation
                && document.utf16_len() == self.key.document_length
                && document.selection() == self.selection
        })
    }

    /// 发布前配对实际 clip、列宽和源窗口；这里只复核既有标量，不再进入 Native 几何。
    fn covers(
        &self,
        view: &PieceBackedMarkdownTextView,
        storage: &PieceBackedMarkdownTextStorage,
        visible: NSRect,
        source: &std::ops::Range<usize>,
    ) -> bool {
        self.current(view, storage)
            && self.key.visible_origin_x == native_markdown_layout_metric(visible.origin.x)
            && self.key.visible_origin_y == native_markdown_layout_metric(visible.origin.y)
            && self.key.visible_width == native_markdown_layout_metric(visible.size.width)
            && self.key.visible_height == native_markdown_layout_metric(visible.size.height)
            && self.key.native_viewport_start <= source.start
            && source.end <= self.key.native_viewport_end
    }

    /// 只在已裁交、连续覆盖的真实 run 中查询；包络外没有默认 Plain 权限。
    fn query(&self, index: usize) -> Option<omni_markdown_core::MarkdownPresentationQuery> {
        let ordinal = self
            .runs
            .partition_point(|run| run.utf16_range.end <= index);
        self.runs
            .get(ordinal)
            .filter(|run| run.utf16_range.contains(&index))
            .cloned()
    }
}

/// 裁交原完整解析的 metadata；最多沿现成目录访问128个 run，不复制源码或保留整个 snapshot。
fn clipped_preview_visibility_runs(
    snapshot: &omni_markdown_core::MarkdownViewportPresentationSnapshot,
    window: std::ops::Range<usize>,
) -> Option<Vec<omni_markdown_core::MarkdownPresentationQuery>> {
    if window.is_empty()
        || window.len() > MAX_DECORATION_VIEWPORT_UNITS
        || snapshot.utf16_window().start > window.start
        || window.end > snapshot.utf16_window().end
    {
        return None; // 源窗口须来自原有限 viewport，不能把目录包络外或超预算正文当已知样式。
    }
    let mut cursor = window.start;
    let mut runs = Vec::new();
    while cursor < window.end {
        if runs.len() == MAX_SPECIAL_CHARACTER_MARKERS {
            return None; // 不完整的前缀不签全窗证明；原 RootLeaf 续体仍可在后续真实发布覆盖它。
        }
        let mut query = snapshot.query(cursor)?;
        query.utf16_range = cursor..query.utf16_range.end.min(window.end);
        cursor = query.utf16_range.end;
        runs.push(query); // 只移入两个坐标和 styles，URL、正文及原解析大目录均不进入原装饰 Vec。
    }
    Some(runs)
}

impl PartialEq for NativeMarkdownDecorationGeometry {
    /// 实际几何或原Find权限变化才要求重绘；不同提示角色不能互相复用像素。
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Source {
                    frame: left,
                    matched: left_match,
                },
                Self::Source {
                    frame: right,
                    matched: right_match,
                },
            ) => left == right && left_match == right_match,
            (Self::Find(left), Self::Find(right)) => left.same_geometry(right),
            (Self::PreviewVisibility(left), Self::PreviewVisibility(right)) => {
                Rc::ptr_eq(left, right) // 同一可见事实在普通查询变化时复用，不因为复读metadata要求重绘。
            }
            _ => false,
        }
    }
}

/// Match the preserved special-character predicate while excluding canonical CR/LF line separators.
fn is_visible_special_character(unit: u16) -> bool {
    matches!(unit, 0x0000..=0x0008 | 0x000b..=0x000c | 0x000e..=0x001f | 0x007f..=0x009f | 0x00ad | 0x061c | 0x200b | 0x200e..=0x200f | 0x2028..=0x2029 | 0x202d..=0x202e | 0x2066..=0x2067 | 0x2069 | 0xfeff | 0xfff9..=0xfffc)
}

impl PieceBackedMarkdownTextView {
    /// 只取原已准入源范围的真实TextKit segment；每个Native阶段前后都服从同一原票。
    fn source_decoration_frames(
        &self,
        source: std::ops::Range<usize>,
        maximum_frames: usize,
        current: &impl Fn() -> bool,
    ) -> Option<Vec<NSRect>> {
        let manager =
            native_markdown_table_intent_step(current, || self.textLayoutManager()).ok()??;
        let content =
            native_markdown_table_intent_step(current, || manager.textContentManager()).ok()??;
        if native_markdown_table_intent_step(current, || content.hasEditingTransaction()).ok()? {
            return None; // 活TextKit事务不签发稳定glyph位置，也不为提示主动ensure。
        }
        let provider = ProtocolObject::<dyn NSTextElementProvider>::from_ref(&*content);
        let document_range =
            native_markdown_table_intent_step(current, || provider.documentRange()).ok()?;
        let start =
            native_markdown_table_intent_step(current, || document_range.location()).ok()?;
        let (from, to) = (
            isize::try_from(source.start).ok()?,
            isize::try_from(source.end).ok()?,
        );
        let location = native_markdown_table_intent_step(current, || {
            content.locationFromLocation_withOffset(&start, from)
        })
        .ok()??;
        let end = native_markdown_table_intent_step(current, || {
            content.locationFromLocation_withOffset(&start, to)
        })
        .ok()??;
        let range = native_markdown_table_intent_step(current, || {
            NSTextRange::initWithLocation_endLocation(NSTextRange::alloc(), &location, Some(&end))
        })
        .ok()??;
        let origin =
            native_markdown_table_intent_step(current, || self.textContainerOrigin()).ok()?;
        let frames = RefCell::new(Vec::new());
        {
            let block = block2::StackBlock::new(
                |_range: *mut NSTextRange,
                 frame: NSRect,
                 _baseline: f64,
                 _container: NonNull<NSTextContainer>| {
                    if !current() || frames.borrow().len() >= maximum_frames {
                        // 取消或有限矩形预算到达后立即停止，callback不保留任何Native对象。
                        return objc2::runtime::Bool::NO;
                    }
                    if [
                        frame.origin.x,
                        frame.origin.y,
                        frame.size.width,
                        frame.size.height,
                    ]
                    .into_iter()
                    .all(f64::is_finite)
                        && frame.size.width >= 0.0
                        && frame.size.height >= 0.0
                    {
                        // Find保实际glyph矩形，不能用控制字符的最小提示宽度放大普通命中。
                        frames.borrow_mut().push(NSRect::new(
                            NSPoint::new(frame.origin.x + origin.x, frame.origin.y + origin.y),
                            frame.size,
                        ));
                    }
                    objc2::runtime::Bool::YES
                },
            );
            native_markdown_table_intent_step(current, || {
                manager.enumerateTextSegmentsInRange_type_options_usingBlock(
                    &range,
                    NSTextLayoutManagerSegmentType::Standard,
                    NSTextLayoutManagerSegmentOptions::RangeNotRequired,
                    &block,
                );
            })
            .ok()?;
        } // 同步StackBlock的词法作用域结束后才交付Copy矩形，不用drop延长非析构类型的借用。
        Some(frames.into_inner())
    }

    /// 首个正文 Native getter 前裁出当前可见样式；后续属性窗口换页不会逐出这份有限事实。
    fn capture_preview_visibility(
        &self,
        storage: &PieceBackedMarkdownTextStorage,
    ) -> Option<Rc<NativeMarkdownPreviewVisibility>> {
        let (key, identity) = self
            .ivars()
            .visible_table_layout_cache
            .borrow()
            .as_ref()
            .map(|cache| (cache.request_key, cache.document_identity.clone()))?;
        if key.presentation_mode != MarkdownPresentationMode::InlinePreview
            || !WeakMarkdownDocument::ptr_eq(&identity, &storage.ivars().document.borrow())
        {
            return None; // 没有本原文稿已发布的实际 viewport 时，不用旧解析包络猜可见区。
        }
        let previous =
            self.ivars()
                .source_decorations
                .borrow()
                .iter()
                .find_map(|item| match item {
                    NativeMarkdownDecorationGeometry::PreviewVisibility(proof)
                        if proof.key == key && proof.current(self, storage) =>
                    {
                        Some(Rc::clone(proof))
                    }
                    _ => None, // 括号/Find几何不授予语义；过期可见事实也不能拼成新证明。
                });
        if previous.is_some() {
            return previous; // 同一真实 viewport 的查询变化只复用原有限 metadata，不反复分配。
        }
        let document = identity.upgrade()?;
        let selection = {
            let document = document.borrow();
            if document.generation() != key.document_generation
                || document.utf16_len() != key.document_length
            {
                return None; // 原 cache 已经落后于 canonical，不能从后来解析补签旧 key。
            }
            document.selection()
        };
        let semantic_epoch = storage.ivars().presentation_semantic_epoch.get()?;
        let runs = {
            let presentation = storage.ivars().presentation.borrow();
            let snapshot = presentation.semantic_snapshot()?;
            if snapshot.generation() != key.document_generation {
                return None; // SourceFallback 没有 snapshot；不同代次的完整目录同样没有本次权限。
            }
            clipped_preview_visibility_runs(
                &snapshot,
                key.native_viewport_start..key.native_viewport_end,
            )?
        }; // 临时 snapshot 在纯 Rust 裁交后即归还；不跨任何 Native getter 保留大目录。
        let proof = Rc::new(NativeMarkdownPreviewVisibility {
            document: identity,
            selection,
            semantic_epoch,
            storage_input: storage.ivars().input_transaction_epoch.get(),
            view_input: self.ivars().table_input_intent_epoch.get(),
            folds: self.ivars().folds.revision(),
            font: storage.ivars().font_run_scratch.epoch(),
            key,
            runs,
        });
        proof.current(self, storage).then_some(proof)
    }

    /// 仅在既有settled刷新消费同次匹配结果；查询、来源过滤与实际矩形均不进入paint栈。
    fn prepare_find_highlight_geometry(
        &self,
        storage: &PieceBackedMarkdownTextStorage,
        current: &impl Fn() -> bool,
        visible: NSRect,
        source_range: std::ops::Range<usize>,
        preview_visibility: Option<&NativeMarkdownPreviewVisibility>,
    ) -> Option<NativeMarkdownFindHighlightBatch> {
        let requested_source = source_range.clone(); // 两个坐标留给本批实际几何核验，不复制查询或正文。
        let controller =
            native_markdown_table_intent_step(current, || self.ivars().controller.get()?.load())
                .ok()??;
        let bar = controller.current_find_input_bar()?;
        let feedback = NativeMarkdownFindViewportFeedback::capture(
            &bar,
            &controller,
            controller.find_input_intent(),
            false,
        )?;
        let prepared =
            feedback.prepare_highlights(&bar, self, storage, &controller, visible, source_range)?;
        let mut batch = prepared.batch;
        let width = native_markdown_table_intent_step(current, || self.bounds().size.width).ok()?;
        let original = || current() && batch.current(self, storage, &controller, visible, width);
        let preview_visibility = preview_visibility.filter(|proof| {
            proof.covers(self, storage, visible, &requested_source)
                && proof.key.text_view_width == native_markdown_layout_metric(width)
        }); // 每轮真实 Native getter之后仍核原几何，旧 metadata 不能为不同列宽补签坐标。
        let mut frames = Vec::new();
        for source in prepared.ranges {
            if !original() {
                return None; // 上一个Native getter重入后旧范围全部丢弃，不发布半批高亮。
            }
            if frames.len() == MAX_SPECIAL_CHARACTER_MARKERS {
                break; // 多行命中也服从实际矩形上限，不为一次超长查询缓存整个段落。
            }
            if !self.find_source_is_visible(storage, &source, preview_visibility) {
                continue; // 自绘表格、隐藏标记或折叠源没有标准TextKit glyph权限，不能画占位矩形。
            }
            let Some(found) = self.source_decoration_frames(
                source,
                MAX_SPECIAL_CHARACTER_MARKERS - frames.len(),
                &original,
            ) else {
                return None; // 没有实际完整原生坐标时保持无高亮，不把来源坐标当屏幕位置。
            };
            for frame in found {
                if frame.size.width > 0.0
                    && frame.size.height > 0.0
                    && NSIntersectsRect(frame, visible)
                {
                    frames.push(frame); // 同一有限Vec只拥有Copy矩形，不持TextKit segment对象。
                }
            }
        }
        if !original() || frames.is_empty() {
            return None;
        }
        batch.frames = frames;
        Some(batch)
    }

    /// Preview须逐语义run证明源仍显示；未知语义不触发解析，Source保持全部原文字面可见。
    fn find_source_is_visible(
        &self,
        storage: &PieceBackedMarkdownTextStorage,
        source: &std::ops::Range<usize>,
        preview_visibility: Option<&NativeMarkdownPreviewVisibility>,
    ) -> bool {
        if self.ivars().folds.borrow().iter().any(|fold| {
            source.start < fold.hidden_utf16_range.end && source.end > fold.hidden_utf16_range.start
        }) {
            return false; // 跨折叠的命中不能当成一个连续可见glyph范围。
        }
        if storage.presentation_mode() == MarkdownPresentationMode::Source {
            return true;
        }
        let generation = self.ivars().folds.generation.get();
        let mut cursor = source.start;
        while cursor < source.end {
            let query = storage
                .source_leaf_query(cursor)
                .or_else(|| {
                    preview_visibility
                        .filter(|proof| proof.current(self, storage))
                        .and_then(|proof| proof.query(cursor))
                })
                .or_else(|| {
                    let presentation = storage.ivars().presentation.borrow();
                    let snapshot = presentation.semantic_snapshot()?;
                    (snapshot.generation() == generation)
                        .then(|| snapshot.query(cursor))
                        .flatten()
                }); // 只有完整解析目录或真实 RootLeaf 授权；fallback 的空 styles 不是 Plain 证明。
            let Some(query) = query else {
                return false; // 未发布的语义空洞不能被默认Plain冒认成可见源。
            };
            if [
                MarkdownPresentationStyles::MARKER,
                MarkdownPresentationStyles::HIDDEN_MARKER,
                MarkdownPresentationStyles::TABLE,
                MarkdownPresentationStyles::TABLE_ROW_PLACEHOLDER,
            ]
            .into_iter()
            .any(|style| query.styles.contains(style))
            {
                return false; // 表格由独立CTFrame绘制，marker被隐藏；后续需要其真实source map几何。
            }
            cursor = query.utf16_range.end.min(source.end); // Core query保证包含当前标量且非空，不重读源码。
        }
        true
    }

    /// 原合并executor结束实际编辑事务后准备提示与Find几何；本次原票贯穿全部Native阶段。
    pub(super) fn refresh_source_decorations(&self) -> bool {
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if self.ivars().textkit_graph_teardown.get() {
            // 直接或迟到调用不取得 canonical Rc，也不重建关闭后释放的几何容量。
            return false;
        }
        let Some(storage) = self.ivars().text_storage.load() else {
            // Teardown releases old cue geometry without replacing its expired document owner.
            let changed = !self.ivars().source_decorations.borrow().is_empty();
            self.ivars().source_decorations.borrow_mut().clear();
            return changed;
        };
        let Some(document) = storage.document() else {
            return false; // 已失去原Doc的Native空壳不能重新构造提示或Find几何。
        };
        let (generation, length) = {
            // End canonical borrows before viewport/segment calls can reenter native text callbacks.
            let document = document.borrow();
            (document.generation(), document.utf16_len())
        };
        let preview_visibility = self.capture_preview_visibility(&storage);
        let selection = self.selectedRange();
        let mut positions = Vec::with_capacity(MAX_SPECIAL_CHARACTER_MARKERS + 2);
        if selection.length == 0 && selection.location <= length {
            // Parsing/classification stays portable and bounded; release the canonical borrow before native geometry.
            let query = markdown_bracket_match(&document.borrow(), selection.location);
            match query {
                // Only a proven admitted token context supplies visible bracket handles.
                Ok(MarkdownBracketQuery::Match(found)) => {
                    positions.push((found.start_utf16, Some(found.matched)));
                    if let Some(end) = found.end_utf16 {
                        // Complete matching or mismatched pairs display both canonical source handles.
                        positions.push((end, Some(found.matched)));
                    }
                }
                // Ordinary carets have no nearby bracket cue and require no native segment queries.
                Ok(MarkdownBracketQuery::NoMatch) => {}
                // A bounded search cannot honestly label an unseen partner as unmatched.
                Ok(MarkdownBracketQuery::BudgetExhausted) => {}
                // Unestablished parser context is omitted rather than assigning a misleading cross-token pair.
                Ok(MarkdownBracketQuery::ContextUnavailable) => {}
                // Invalid/stale canonical coordinates produce no partial decoration or document mutation.
                Err(_) => {}
            }
        }
        let Some((visible, paragraph_window)) = self.visible_utf16_window(length) else {
            return false; // 没有layoutViewport后配对的实际clip/range，就不补签零范围或匹配反馈。
        };
        let viewport = match (
            self.insertion_offset_at_view_point(NSPoint::new(visible.origin.x, visible.origin.y)),
            self.insertion_offset_at_view_point(NSPoint::new(
                visible.origin.x + visible.size.width - 1.0,
                visible.origin.y + visible.size.height - 1.0,
            )),
        ) {
            // Point navigation narrows giant wrapped paragraphs to the actual visible character interval.
            (Some(start), Some(end)) => {
                start.min(end)..start.max(end).saturating_add(1).min(length)
            }
            // Unavailable transient point geometry keeps the existing bounded paragraph fallback.
            _ => paragraph_window,
        };
        if !self.projection_generation_is_current(generation) {
            // viewport 定位可能重入编辑；不把旧范围交给后续 canonical 扫描或 Native segment 查询。
            return false;
        }
        let mode = storage.presentation_mode();
        let fold_revision = self.ivars().folds.revision();
        let input_epoch = self.ivars().table_input_intent_epoch.get();
        let font_epoch = storage.ivars().font_run_scratch.epoch();
        let identity = Rc::downgrade(&document);
        let current = || {
            !self.ivars().textkit_graph_teardown.get()
                && document.borrow().generation() == generation
                && self.ivars().folds.matches_document(&document)
                && self.ivars().folds.revision() == fold_revision
                && self.ivars().table_input_intent_epoch.get() == input_epoch
                && storage.presentation_mode() == mode
                && storage.ivars().font_run_scratch.epoch() == font_epoch
                && WeakMarkdownDocument::ptr_eq(&identity, &storage.ivars().document.borrow())
        };
        let folds = self.ivars().folds.borrow().clone();
        let bracket_count = positions.len();
        let mut cursor = viewport.start;
        let mut scanned = 0_usize;
        let mut scratch = [0_u16; 512];
        while cursor < viewport.end
            && scanned < MAX_DECORATION_VIEWPORT_UNITS
            && positions.len() - bracket_count < MAX_SPECIAL_CHARACTER_MARKERS
        {
            // Skip collapsed source intervals by metadata; they consume no copy/scan budget or native query.
            if let Some(hidden) = folds
                .iter()
                .find(|fold| fold.hidden_utf16_range.contains(&cursor))
            {
                // The canonical hidden text remains saved/searchable but contributes no visible marker geometry.
                cursor = hidden.hidden_utf16_range.end;
                continue;
            }
            let next_hidden = folds
                .iter()
                .filter(|fold| fold.hidden_utf16_range.start > cursor)
                .map(|fold| fold.hidden_utf16_range.start)
                .min()
                .unwrap_or(viewport.end);
            let count = scratch
                .len()
                .min(viewport.end - cursor)
                .min(next_hidden - cursor)
                .min(MAX_DECORATION_VIEWPORT_UNITS - scanned);
            if document
                .borrow()
                .copy_utf16_code_units(cursor, &mut scratch[..count])
                .is_err()
            {
                // A changed/disconnected canonical interval contributes no partially initialized control cues.
                break;
            }
            for (index, unit) in scratch[..count].iter().enumerate() {
                // Every inspected control comes from the fixed visible scratch and finite retained marker cap.
                if positions.len() - bracket_count >= MAX_SPECIAL_CHARACTER_MARKERS {
                    // Further visible controls are discovered only after a future viewport/selection request.
                    break;
                }
                if is_visible_special_character(*unit) {
                    // A dot identifies a control without changing or retaining the underlying source code unit.
                    positions.push((cursor + index..cursor + index + 1, None));
                }
            }
            cursor += count;
            scanned += count;
        }
        let maximum_geometry = MAX_SPECIAL_CHARACTER_MARKERS + 2;
        let mut geometry = Vec::with_capacity(positions.len());
        let preview_visibility =
            preview_visibility.filter(|proof| proof.covers(self, &storage, visible, &viewport));
        for (source, matched) in positions {
            if !current() {
                // 前一个 segment 查询返回后代次可能改变，停止而不是查询下一个过期位置。
                return false;
            }
            if geometry.len() >= maximum_geometry {
                // A wrapped handle cannot expand persistent native geometry beyond the shared finite viewport budget.
                break;
            }
            if source.start >= viewport.end
                || source.end <= viewport.start
                || folds.iter().any(|fold| {
                    // A cue crossing omitted canonical text cannot claim one contiguous visible native handle.
                    source.start < fold.hidden_utf16_range.end
                        && source.end > fold.hidden_utf16_range.start
                })
            {
                // Offscreen partners and deliberately hidden source never force native layout or scrolling.
                continue;
            }
            let visible_source = source.start.max(viewport.start)..source.end.min(viewport.end);
            let Some(frames) = self.source_decoration_frames(
                visible_source,
                maximum_geometry - geometry.len(),
                &current,
            ) else {
                continue;
            };
            for frame in frames {
                // 控制/括号提示保留原最小尺寸；零宽控制字符也必须先扩成可见点再参与clip判断。
                let frame = NSRect::new(
                    frame.origin,
                    NSSize::new(frame.size.width.max(7.0), frame.size.height.max(14.0)),
                );
                if NSIntersectsRect(frame, visible) {
                    // Retain copied rectangles only for actual visible handle segments and control positions.
                    geometry.push(NativeMarkdownDecorationGeometry::Source { frame, matched });
                }
            }
        }
        if !current() {
            // TextKit 查询可同步重入；原生调用结束后的失效 geometry 仅在本地析构。
            return false;
        }
        if let Some(proof) = preview_visibility
            .as_ref()
            .filter(|proof| proof.covers(self, &storage, visible, &viewport))
        {
            // Vec至多保一个可见事实；Native glyph/属性 getter可逐出共享解析窗，但不夺其原纯语义。
            geometry.push(NativeMarkdownDecorationGeometry::PreviewVisibility(
                Rc::clone(proof),
            ));
        }
        if let Some(find) = self.prepare_find_highlight_geometry(
            &storage,
            &current,
            visible,
            viewport,
            preview_visibility.as_deref(),
        ) {
            geometry.push(NativeMarkdownDecorationGeometry::Find(Rc::new(find)));
        }
        if !current() {
            return false; // Find的字段/segment getters也可能重入，旧整批提示不能随后覆盖新图。
        }
        let changed = *self.ivars().source_decorations.borrow() != geometry;
        *self.ivars().source_decorations.borrow_mut() = geometry;
        self.ivars().source_decoration_generation.set(generation);
        self.ivars().source_decorations_dirty.set(false);
        changed
    }

    /// 消费已准备的括号、控制点与Find矩形；绘制不枚举元素，也不改用户选择和历史。
    pub(super) fn draw_source_decorations(&self, dirty: NSRect) {
        let _payload_use = PayloadUseGuard::enter(&self.ivars().payload_use_depth);
        if self.ivars().textkit_graph_teardown.get() {
            // 独立 draw 调用也不能使用旧 cue snapshot 或 canonical owner。
            return;
        }
        if self.ivars().textkit_input_depth.get() != 0
            || self
                .ivars()
                .marked_text_range
                .get()
                .is_some_and(|range| range.length != 0)
        {
            // Composition keeps native provisional glyphs unobscured and never paints stale cue positions.
            return;
        }
        let storage = self.ivars().text_storage.load();
        let generation = storage
            .as_ref()
            .and_then(|storage| storage.document())
            .map(|document| document.borrow().generation());
        if generation != Some(self.ivars().source_decoration_generation.get()) {
            // Canonical edits invalidate all old rectangles before the next queued layout transaction.
            return;
        }
        let geometry = self.ivars().source_decorations.borrow().clone();
        NSGraphicsContext::saveGraphicsState_class();
        for cue in geometry {
            let NativeMarkdownDecorationGeometry::Source { frame, matched } = cue else {
                if let NativeMarkdownDecorationGeometry::Find(batch) = cue
                    && let Some(storage) = storage.as_ref()
                {
                    self.draw_find_highlight_batch(&batch, storage, dirty);
                }
                continue; // Find与既有提示共用当前上下文，不创建新层或进入源码/布局调用。
            };
            // Copying the bounded cache ends state borrows before native drawing can reenter AppKit.
            if !NSIntersectsRect(frame, dirty) {
                // Undamaged viewport cues need no native draw work.
                continue;
            }
            let color = match matched {
                // Balanced pairs use an unobtrusive green outline around their existing source glyphs.
                Some(true) => NSColor::colorWithSRGBRed_green_blue_alpha(0.4, 0.8, 0.6, 0.7),
                // Invalid/unclosed pairs retain a visible red outline without changing editable text.
                Some(false) => NSColor::systemRedColor(),
                // Special control characters use the same visible warning tint as the preserved editor.
                None => NSColor::systemRedColor(),
            };
            color.setStroke();
            if matched.is_some() {
                // Outlines preserve the native glyph, selection and accessibility source value.
                NSBezierPath::strokeRect(NSInsetRect(frame, -1.0, -1.0));
            } else {
                // A red dot reveals invisible control presence while its canonical code unit remains unchanged.
                let font = NSFont::systemFontOfSize(12.0);
                // SAFETY: these are immutable AppKit exported font/color keys with process lifetime.
                let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
                let values: [&AnyObject; 2] = [&font, &color];
                let attributes = MarkdownTextAttributes::from_slices(&keys, &values);
                // SAFETY: the complete immutable dictionary uses valid font/color objects retained during this draw.
                let label = unsafe {
                    NSAttributedString::initWithString_attributes(
                        NSAttributedString::alloc(),
                        ns_string!("•"),
                        Some(&attributes),
                    )
                };
                label.drawAtPoint(frame.origin);
            }
        }
        NSGraphicsContext::restoreGraphicsState_class();
    }

    /// 只画同票下已准备的有限矩形；clip、模式或原输入变化后旧batch不再留下新像素。
    fn draw_find_highlight_batch(
        &self,
        batch: &NativeMarkdownFindHighlightBatch,
        storage: &PieceBackedMarkdownTextStorage,
        dirty: NSRect,
    ) {
        let Some(controller) = self.ivars().controller.get().and_then(Weak::load) else {
            return; // 原controller结束后，没有其他窗口可以替这批几何授予绘制权。
        };
        let authority = || batch.authority_current(self, storage, &controller);
        let Ok(visible) = native_markdown_table_intent_step(&authority, || self.visibleRect())
        else {
            return;
        };
        let Ok(width) = native_markdown_table_intent_step(&authority, || self.bounds().size.width)
        else {
            return;
        };
        let current = || batch.current(self, storage, &controller, visible, width);
        let Ok(color) = native_markdown_table_intent_step(&current, || {
            NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.75, 0.12, 0.25)
        }) else {
            return;
        };
        if native_markdown_table_intent_step(&current, || color.setFill()).is_err() {
            return;
        }
        for frame in &batch.frames {
            if !current() {
                return; // 上一个fill重入的后继输入只能撤旧绘制，不能改用户选区或重做查找。
            }
            if NSIntersectsRect(*frame, dirty)
                && native_markdown_table_intent_step(&current, || NSBezierPath::fillRect(*frame))
                    .is_err()
            {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipped_preview_visibility_keeps_real_hidden_markers_and_utf16_intersections() {
        let text = "前 **中文🙂** 后\n";
        let mut presentation = MarkdownViewportPresentation::default();
        presentation
            .rebuild(7, 100..100 + text.encode_utf16().count(), text)
            .unwrap();
        let snapshot = presentation.semantic_snapshot().unwrap();
        let clipped = clipped_preview_visibility_runs(&snapshot, 103..111).unwrap();
        assert_eq!(clipped.first().unwrap().utf16_range.start, 103);
        assert_eq!(clipped.last().unwrap().utf16_range.end, 111);
        assert!(clipped.iter().any(|run| {
            run.utf16_range.contains(&103)
                && run.styles.contains(MarkdownPresentationStyles::MARKER)
                && run
                    .styles
                    .contains(MarkdownPresentationStyles::HIDDEN_MARKER)
        })); // 裁到半个原marker也保留真实隐藏事实，不能把边缘空白变成 Plain。
        assert!(clipped.iter().any(|run| {
            run.utf16_range.start <= 104
                && 108 <= run.utf16_range.end
                && run.styles.contains(MarkdownPresentationStyles::STRONG)
        })); // 中文和emoji两单元仍沿原UTF16坐标，不改成字符数或重新推断 token。
        assert!(
            clipped
                .iter()
                .any(|run| { run.utf16_range.contains(&110) && run.styles.is_empty() })
        );
        drop(snapshot);
        presentation.rebuild(7, 900..905, "other").unwrap();
        assert!(presentation.query(104).is_none());
        assert!(clipped.iter().any(|run| {
            run.utf16_range.contains(&104)
                && run.styles.contains(MarkdownPresentationStyles::STRONG)
        })); // 共享属性窗口真正逐出原目录后，保留的可见metadata仍只解释原103..111。
    }

    #[test]
    fn clipped_preview_visibility_rejects_fallback_outside_and_incomplete_budgets() {
        let mut presentation = MarkdownViewportPresentation::default();
        presentation
            .record_source_fallback_window(7, 100..120)
            .unwrap();
        assert!(presentation.query(105).is_some());
        assert!(presentation.semantic_snapshot().is_none()); // 空styles的fallback不能进入完整snapshot接面。
        let text = "x".repeat(MAX_DECORATION_VIEWPORT_UNITS + 1);
        presentation
            .rebuild(7, 100..100 + text.len(), &text)
            .unwrap();
        let snapshot = presentation.semantic_snapshot().unwrap();
        assert!(clipped_preview_visibility_runs(&snapshot, 99..101).is_none());
        assert!(clipped_preview_visibility_runs(&snapshot, 100..100).is_none());
        assert!(clipped_preview_visibility_runs(&snapshot, 100..100 + text.len()).is_none());
        assert!(
            clipped_preview_visibility_runs(&snapshot, 100..100 + MAX_DECORATION_VIEWPORT_UNITS,)
                .is_some()
        ); // 原可见单位上限允许完整范围，超出一单元不能签部分覆盖。
        let marked = "**x** ".repeat(MAX_SPECIAL_CHARACTER_MARKERS);
        presentation.rebuild(8, 0..marked.len(), &marked).unwrap();
        let snapshot = presentation.semantic_snapshot().unwrap();
        assert!(clipped_preview_visibility_runs(&snapshot, 0..marked.len()).is_none());
        assert!(clipped_preview_visibility_runs(&snapshot, 0..6).is_some()); // 多run准入失败仍不损害真实有限子窗。
    }

    #[test]
    fn invisible_controls_match_the_preserved_editor_predicate_without_marking_emoji_joiners() {
        for unit in [0, 7, 0x061c, 0x200b, 0x202e, 0x2066, 0xfeff, 0xfffc] {
            // Explicit controls/bidi markers are visible cues; ordinary line breaks and emoji joiners are excluded.
            assert!(is_visible_special_character(unit));
        }
        for unit in [9, 10, 13, 32, 0x200c, 0x200d, 0x2068, 0xd83d] {
            // The exact baseline predicate does not treat every format character or surrogate as a special marker.
            assert!(!is_visible_special_character(unit));
        }
    }
}
