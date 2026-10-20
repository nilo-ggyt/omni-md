//! 输入变化撤旧反馈与像素；原settled刷新同次生成有限计数、源范围与真实字面高亮。

use super::*;
/// 即时可见反馈共享同一有限窗口与结果上限，不随文稿长度增加工作。
pub(in crate::native_surface) const MAX_DECORATION_VIEWPORT_UNITS: usize = 16_384;
pub(in crate::native_surface) const MAX_SPECIAL_CHARACTER_MARKERS: usize = 128;
use omni_markdown_core::MarkdownDocumentBufferError;
use omni_markdown_core::markdown_find::{
    MarkdownLiteralFindOptions, visit_markdown_literal_matches_cancellable,
};

/// 一次原Scene准备中的两种真实来源；格只借窗口，canonical包络用于排序而不授匹配权限。
#[cfg(feature = "native-paged-production")]
pub(in crate::native_surface) enum NativeMarkdownFindVisibleSource<'a> {
    Body(std::ops::Range<usize>),
    Cell {
        canonical: std::ops::Range<usize>,
        window: &'a crate::native_surface::paged_line_viewport::NativeMarkdownComposedTableCell,
    },
}

/// 显示窗口与canonical源是两个真实失败边界；交原Body映射时保留Source首错和原几何错误。
#[cfg(feature = "native-paged-production")]
pub(in crate::native_surface) enum NativeMarkdownPagedFindHighlightError {
    Source(MarkdownDocumentBufferError),
    Window(crate::native_surface::paged_line_viewport::NativeMarkdownComposedViewportError),
}

#[cfg(feature = "native-paged-production")]
impl From<MarkdownDocumentBufferError> for NativeMarkdownPagedFindHighlightError {
    fn from(cause: MarkdownDocumentBufferError) -> Self {
        Self::Source(cause)
    }
}

/// 排队只保原bar/Doc弱身份和输入时的纯代次；不保存查询值、正文或Native强引用。
#[derive(Clone)]
pub(in crate::native_surface) struct NativeMarkdownFindViewportFeedback {
    bar: std::rc::Weak<NativeMarkdownFindBar>,
    document: WeakMarkdownDocument,
    generation: u64,
    owner: usize,
    intent: u64,
    bar_revision: u64,
    visible: bool,
    refresh: bool,
}

impl NativeMarkdownFindViewportFeedback {
    /// 原Doc由栏创建时固定；首个Native getter前捕获代次，不能等队列执行再领取当前Doc。
    pub(in crate::native_surface) fn capture(
        bar: &Rc<NativeMarkdownFindBar>,
        controller: &NativeMarkdownController,
        intent: u64,
        refresh: bool,
    ) -> Option<Self> {
        let document = bar.document.upgrade()?;
        let generation = document.borrow().generation();
        let request = Self {
            bar: Rc::downgrade(bar),
            document: bar.document.clone(),
            generation,
            owner: std::ptr::from_ref(controller).addr(),
            intent,
            bar_revision: bar.layout_revision.get(),
            visible: bar.visible.get(),
            refresh,
        };
        request.current(controller).then_some(request)
    }

    /// 检查只读取原Rust权限；不会为比较身份升级Native weak或借用Active Surface。
    pub(in crate::native_surface) fn current(&self, controller: &NativeMarkdownController) -> bool {
        std::ptr::from_ref(controller).addr() == self.owner
            && native_markdown_controller_has_foreground_authority(controller)
            && controller.find_input_intent() == self.intent
            && controller.owns_find_input_bar(&self.bar)
            && self.bar.upgrade().is_some_and(|bar| {
                !bar.closed.get()
                    && bar.visible.get() == self.visible
                    && bar.layout_revision.get() == self.bar_revision
                    && WeakMarkdownDocument::ptr_eq(&bar.document, &self.document)
            })
            && self
                .document
                .upgrade()
                .is_some_and(|document| document.borrow().generation() == self.generation)
    }

    /// 新输入立即撤旧“找到/无结果/已替换”文案；原bar的借用在setter前已经结束。
    pub(in crate::native_surface) fn clear(&self, controller: &NativeMarkdownController) {
        if self.current(controller)
            && let Some(bar) = self.bar.upgrade()
        {
            bar.status.setStringValue(ns_string!(""));
        }
    }

    /// 输入结束便交给原合并 executor；反馈回调不再先等一轮主队列才有机会申请几何刷新。
    pub(in crate::native_surface) fn schedule_highlights(
        &self,
        controller: &NativeMarkdownController,
    ) {
        if !self.current(controller) {
            return;
        }
        let Some(bar) = self.bar.upgrade() else {
            return;
        };
        let binding = bar.presentation_body.borrow().clone();
        if let Some(body) = binding.and_then(|body| body.upgrade()) {
            body.refresh_find_highlights(&bar, controller, self);
        }
    }

    /// 可见匹配由原 Scene 发布完成；输入反馈只校验正则语法，不创建临时正文图。
    pub(in crate::native_surface) fn run(&self, controller: &NativeMarkdownController) {
        if !self.current(controller) {
            return;
        }
        let Some(bar) = self.bar.upgrade() else {
            return;
        };
        if bar.presentation_body.borrow().is_none() {
            return;
        }
        let current = || self.current(controller);
        if self.visible && self.refresh {
            let result = (|| -> Result<(), String> {
                if let Some(query) = read_visible_query(&bar, &current)?
                    && query.regex
                {
                    native_markdown_table_intent_step(&current, || query.compiled_regex())
                        .map_err(|_| "原正则输入已变化")??;
                    self.consume_status(&bar, &current, "");
                }
                Ok(())
            })();
            if let Err(cause) = result {
                self.consume_status(&bar, &current, &cause);
            }
        }
    }

    /// 独立 TextKit 机制夹具借其已存在的实际图观察字面高亮，正式入口不调用此路径。
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    pub(in crate::native_surface) fn prepare_highlights(
        &self,
        bar: &NativeMarkdownFindBar,
        view: &PieceBackedMarkdownTextView,
        storage: &PieceBackedMarkdownTextStorage,
        controller: &NativeMarkdownController,
        visible: NSRect,
        source_range: std::ops::Range<usize>,
    ) -> Option<NativeMarkdownFindHighlightPreparation> {
        if !self.visible || !self.current(controller) {
            return None;
        }
        match self.read_highlight_matches(bar, view, storage, controller, visible, source_range) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.consume_status(bar, &|| self.current(controller), &error);
                None // 原读取或几何失败不发布部分batch，保留原文字与显式查找能力。
            }
        }
    }

    /// 查询只活本次刷新；Source直接使用原文，Preview只允许后续证明可见的语义区间。
    #[cfg(any(test, feature = "native-markdown-runtime-probe"))]
    fn read_highlight_matches(
        &self,
        bar: &NativeMarkdownFindBar,
        view: &PieceBackedMarkdownTextView,
        storage: &PieceBackedMarkdownTextStorage,
        controller: &NativeMarkdownController,
        visible: NSRect,
        range: std::ops::Range<usize>,
    ) -> Result<Option<NativeMarkdownFindHighlightPreparation>, String> {
        let document = self.document.upgrade().ok_or("原查找文稿已关闭")?;
        if !view.ivars().folds.matches_document(&document)
            || !WeakMarkdownDocument::ptr_eq(&self.document, &storage.ivars().document.borrow())
        {
            return Ok(None); // 原Doc不能通过同generation数值借到另一图的可见范围。
        }
        let selection = view
            .ivars()
            .folds
            .capture_selection()
            .map_err(|_| "当前编辑状态暂不能读取可见范围")?;
        let table_epoch = view.ivars().table_input_intent_epoch.get();
        let mode = storage.presentation_mode();
        let fold_revision = view.ivars().folds.revision();
        let font_epoch = storage.ivars().font_run_scratch.epoch();
        let current = || {
            self.current(controller)
                && selection.current()
                && !view.ivars().textkit_graph_teardown.get()
                && view.ivars().table_input_intent_epoch.get() == table_epoch
                && storage.presentation_mode() == mode
                && view.ivars().folds.revision() == fold_revision
                && storage.ivars().font_run_scratch.epoch() == font_epoch
        };
        let Some(query) = read_visible_query(bar, &current)? else {
            return Ok(None); // marked查询不能产生临时高亮，也不能读取候选值。
        };
        if query.search.is_empty() {
            self.consume_status(bar, &current, "");
            return Ok(None); // 空查询只保持空状态，没有零长枚举或自动选择。
        }
        if query.regex {
            // 仅即时验证同一ICU语法；任意lookaround可能依赖视口外源，不能在截断串上伪造匹配。
            return Ok(None); // 语法反馈在原输入队列完成；这里不反复编译或冒称已有自动正则高亮。
        }
        let length = document.borrow().utf16_len();
        if length == 0 {
            self.consume_status(bar, &current, "当前文档为空");
            return Ok(None);
        }
        let width = native_markdown_table_intent_step(&current, || view.bounds().size.width)
            .map_err(|_| "原查找宽度已变化")?;
        if range.is_empty() {
            return Ok(None); // 没有已证明的可见源行时不能把未知范围说成“没有匹配”。
        }
        let outcome = visible_literal_matches(
            &document.borrow(),
            &unquote(&query.search),
            range,
            MarkdownLiteralFindOptions {
                case_sensitive: query.case_sensitive,
                whole_word: query.word,
                overlapping: true,
            },
            MAX_SPECIAL_CHARACTER_MARKERS,
            current,
        )
        .map_err(|cause| {
            // 保留真实源错误作诊断；产品状态使用固定中文，不把系统英文或笔记内容拼入界面。
            eprintln!("native_find_viewport event=source_read_failed cause={cause}");
            "当前可见区域查找失败，请重试".to_owned()
        })?;
        if current() {
            self.consume_status(bar, &current, &outcome.status());
        }
        if !current() {
            return Ok(None); // 反馈setter重入后，旧源范围也失去生成实际几何的权限。
        }
        Ok(Some(NativeMarkdownFindHighlightPreparation {
            ranges: outcome.ranges,
            batch: NativeMarkdownFindHighlightBatch {
                feedback: self.clone(),
                mode,
                fold_revision,
                font_epoch,
                table_epoch,
                width: native_markdown_layout_metric(width),
                visible,
                frames: Vec::new(),
            },
        }))
    }

    /// 分页发布只借有序的真实可见源域；所有行和格共用16K单元/128匹配，不给每格重领预算。
    /// 查询只活本次调用。原Source读取错误保类型返回，不能洗成零匹配或成功反馈。
    #[cfg(feature = "native-paged-production")]
    pub(in crate::native_surface) fn prepare_paged_highlights(
        &self,
        bar: &NativeMarkdownFindBar,
        document: &Rc<RefCell<MarkdownDocumentBuffer>>,
        sources: &[NativeMarkdownFindVisibleSource<'_>],
        current: &impl Fn() -> bool,
    ) -> Result<Box<[std::ops::Range<usize>]>, NativeMarkdownPagedFindHighlightError> {
        if !self.visible || !current() {
            return Ok(Box::new([]));
        }
        let query = match read_visible_query(bar, current) {
            Ok(Some(query)) => query,
            Ok(None) => return Ok(Box::new([])), // 真实marked输入既不读候选，也不沿旧查询留像素。
            Err(message) => {
                self.consume_status(bar, current, &message);
                return Ok(Box::new([]));
            }
        };
        if query.search.is_empty() {
            self.consume_status(bar, current, "");
            return Ok(Box::new([]));
        }
        if query.regex || sources.is_empty() {
            return Ok(Box::new([])); // 正则只在原输入队列核语法；不能对截断视口执行lookaround冒充全文语义。
        }
        let search = unquote(&query.search);
        let options = MarkdownLiteralFindOptions {
            case_sensitive: query.case_sensitive,
            whole_word: query.word,
            overlapping: true,
        };
        let mut outcome = VisibleLiteralMatches {
            count: 0,
            ranges: Vec::new(),
            source_limited: false,
            count_limited: false,
        };
        let mut remaining = MAX_DECORATION_VIEWPORT_UNITS;
        for (ordinal, source) in sources.iter().enumerate() {
            let maximum_units = remaining;
            // 每个原raw连续域只匹配一次。格迭代器按同一remaining夹display末端，
            // 只逐原sealed记录前进；不会先收齐巨格的全部raw片段或给每格重领16K额度。
            let limited = {
                let mut consume =
                    |range: std::ops::Range<usize>| -> Result<bool, MarkdownDocumentBufferError> {
                        let end = {
                            let document = document.borrow();
                            markdown_utf16_boundary_down(
                                &document,
                                range.start.saturating_add(remaining).min(range.end),
                            )?
                        };
                        let found = visible_literal_matches(
                            &document.borrow(),
                            &search,
                            range.start..end,
                            options,
                            MAX_SPECIAL_CHARACTER_MARKERS - outcome.count,
                            current,
                        )?;
                        remaining -= end - range.start;
                        outcome.count += found.count;
                        outcome.ranges.extend(found.ranges);
                        outcome.count_limited = found.count_limited;
                        outcome.source_limited |= end < range.end;
                        Ok(!outcome.count_limited && !outcome.source_limited && remaining != 0)
                    };
                match source {
                    NativeMarkdownFindVisibleSource::Body(range) => {
                        consume(range.clone())?;
                        false
                    }
                    NativeMarkdownFindVisibleSource::Cell { window, .. } => {
                        let ranges = window
                            .visible_find_ranges(maximum_units, current)
                            .map_err(NativeMarkdownPagedFindHighlightError::Window)?;
                        if let Some(ranges) = ranges {
                            let limited = ranges.source_limited();
                            for range in ranges {
                                if !consume(range?)? {
                                    break;
                                }
                            }
                            limited
                        } else {
                            false // 同票下确实没有与clip相交的文字，不制造空显示源。
                        }
                    }
                }
            };
            outcome.source_limited |= limited;
            if outcome.count_limited || outcome.source_limited || remaining == 0 {
                outcome.source_limited |= ordinal + 1 < sources.len();
                break;
            }
        }
        if current() {
            self.consume_status(bar, current, &outcome.status());
        }
        // 重叠搜索仍按实际命中数反馈；绘图只覆盖匹配域的并集，避免同一像素叠两次半透明黄色。
        let mut merged: Vec<std::ops::Range<usize>> = Vec::with_capacity(outcome.ranges.len());
        for range in outcome.ranges {
            if let Some(last) = merged.last_mut()
                && range.start <= last.end
            {
                last.end = last.end.max(range.end);
            } else {
                merged.push(range);
            }
        }
        Ok(merged.into_boxed_slice())
    }

    /// 只有该次真实输入仍待反馈才发布文案；scroll/显式动作不重新领取状态写入权。
    fn consume_status(
        &self,
        bar: &NativeMarkdownFindBar,
        current: &impl Fn() -> bool,
        message: &str,
    ) {
        let pending = || current() && bar.viewport_feedback_intent.get() == Some(self.intent);
        write_viewport_status(bar, &pending, message);
        if pending() {
            bar.viewport_feedback_intent.set(None);
        } // setter重入已经登记后继intent时，不能清掉后继pending。
    }
}

/// 原查询的组合状态只读；每个Native getter之后继续核同一票，不提交或取消IME。
fn read_visible_query(
    bar: &NativeMarkdownFindBar,
    current: &impl Fn() -> bool,
) -> Result<Option<Query>, String> {
    let editor = native_markdown_table_intent_step(current, || bar.query.currentEditor())
        .map_err(|_| "原查询输入已变化")?;
    if let Some(editor) = editor {
        let editor = editor
            .downcast_ref::<NSTextView>()
            .ok_or("查找字段的原输入上下文不可用")?;
        if native_markdown_table_intent_step(current, || editor.hasMarkedText())
            .map_err(|_| "原查询输入已变化")?
        {
            return Ok(None); // marked期间只撤旧像素，不读取候选查询。
        }
    }
    Query::read_viewport(bar, current).map(Some)
}

/// 源范围只活本次settled准备；几何发布后不继续保存匹配表或查询串。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(in crate::native_surface) struct NativeMarkdownFindHighlightPreparation {
    pub(in crate::native_surface) ranges: Vec<std::ops::Range<usize>>,
    pub(in crate::native_surface) batch: NativeMarkdownFindHighlightBatch,
}

/// 同一原装饰Vec拥有有限矩形和弱票；不持Doc、TextKit、查询文字或第二份布局。
#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
pub(in crate::native_surface) struct NativeMarkdownFindHighlightBatch {
    feedback: NativeMarkdownFindViewportFeedback,
    mode: MarkdownPresentationMode,
    fold_revision: u64,
    font_epoch: u64,
    table_epoch: u64,
    width: i64,
    pub(in crate::native_surface) visible: NSRect,
    pub(in crate::native_surface) frames: Vec<NSRect>,
}

#[cfg(any(test, feature = "native-markdown-runtime-probe"))]
impl NativeMarkdownFindHighlightBatch {
    /// 已准备矩形必须仍属于原栏、原Doc与原模式/字体/折叠/可见几何；绘制不枚举正文。
    pub(in crate::native_surface) fn current(
        &self,
        view: &PieceBackedMarkdownTextView,
        storage: &PieceBackedMarkdownTextStorage,
        controller: &NativeMarkdownController,
        visible: NSRect,
        width: f64,
    ) -> bool {
        self.authority_current(view, storage, controller)
            && self.width == native_markdown_layout_metric(width)
            && self.visible == visible
    }

    /// 读取实际clip/宽度之前先核原业务票，避免Native getter给关闭或模式后的旧batch续权。
    pub(in crate::native_surface) fn authority_current(
        &self,
        view: &PieceBackedMarkdownTextView,
        storage: &PieceBackedMarkdownTextStorage,
        controller: &NativeMarkdownController,
    ) -> bool {
        self.feedback.current(controller)
            && self.feedback.visible
            && !view.ivars().textkit_graph_teardown.get()
            && view.ivars().textkit_input_depth.get() == 0
            && !view
                .ivars()
                .marked_text_range
                .get()
                .is_some_and(|range| range.length != 0)
            && self.mode == storage.presentation_mode()
            && self.fold_revision == view.ivars().folds.revision()
            && self.font_epoch == storage.ivars().font_run_scratch.epoch()
            && self.table_epoch == view.ivars().table_input_intent_epoch.get()
            && WeakMarkdownDocument::ptr_eq(
                &self.feedback.document,
                &storage.ivars().document.borrow(),
            )
    }

    /// 缓存比较包含真实原票及矩形；相同像素也不能保留另一query/Doc的过期授权。
    pub(in crate::native_surface) fn same_geometry(&self, other: &Self) -> bool {
        std::rc::Weak::ptr_eq(&self.feedback.bar, &other.feedback.bar)
            && WeakMarkdownDocument::ptr_eq(&self.feedback.document, &other.feedback.document)
            && self.feedback.intent == other.feedback.intent
            && self.feedback.generation == other.feedback.generation
            && self.feedback.bar_revision == other.feedback.bar_revision
            && self.mode == other.mode
            && self.fold_revision == other.fold_revision
            && self.font_epoch == other.font_epoch
            && self.table_epoch == other.table_epoch
            && self.width == other.width
            && self.visible == other.visible
            && self.frames == other.frames
    }
}

/// 字符串构造和setter分别核同一原viewport票，Native重入后不能覆盖后继输入或位置。
fn write_viewport_status(bar: &NativeMarkdownFindBar, current: &impl Fn() -> bool, message: &str) {
    if let Ok(message) = native_markdown_table_intent_step(current, || NSString::from_str(message))
    {
        let _ = native_markdown_table_intent_step(current, || bar.status.setStringValue(&message));
    }
}

/// 同次visitor交付计数与有限源范围，几何消费后归还；达到预算必须说明“部分/至少”。
#[derive(Debug, Eq, PartialEq)]
struct VisibleLiteralMatches {
    count: usize,
    ranges: Vec<std::ops::Range<usize>>,
    source_limited: bool,
    count_limited: bool,
}

impl VisibleLiteralMatches {
    /// 文案只描述当前真实范围，不能把预算耗尽或局部零结果提升成全局结论。
    fn status(&self) -> String {
        if self.count_limited {
            format!("当前可见区域至少 {} 处匹配（已达反馈上限）", self.count)
        } else if self.source_limited {
            format!("当前可见区域已检查部分，找到 {} 处完整匹配", self.count)
        } else {
            format!("当前可见区域找到 {} 处匹配", self.count)
        }
    }
}

/// 传原Doc范围给既有visitor；whole-word读取原真实邻接，不把截断窗口当作词边界。
fn visible_literal_matches(
    document: &MarkdownDocumentBuffer,
    search: &str,
    viewport: std::ops::Range<usize>,
    options: MarkdownLiteralFindOptions,
    maximum_matches: usize,
    keep_searching: impl FnMut() -> bool,
) -> Result<VisibleLiteralMatches, MarkdownDocumentBufferError> {
    let end = markdown_utf16_boundary_down(
        document,
        viewport
            .start
            .saturating_add(MAX_DECORATION_VIEWPORT_UNITS)
            .min(viewport.end),
    )?;
    let mut result = VisibleLiteralMatches {
        count: 0,
        ranges: Vec::new(),
        source_limited: end < viewport.end,
        count_limited: false,
    };
    visit_markdown_literal_matches_cancellable(
        document,
        search,
        viewport.start..end,
        options,
        keep_searching,
        |found| {
            result.count += 1;
            result.ranges.push(found.range_utf16); // 同一次visitor交付计数和坐标，几何不能重新扫描。
            result.count_limited = result.count == maximum_matches;
            Ok(!result.count_limited) // 精确总数未知时停止原visitor，不继续为了文案扫描更多源。
        },
    )?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 仅创建合成原文件；测试只借同一canonical，不初始化AppKit或建立全文Native镜像。
    fn document(source: &str) -> (tempfile::TempDir, MarkdownDocumentBuffer) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("visible-find.md");
        std::fs::write(&path, source).unwrap();
        let document = MarkdownDocumentBuffer::open(&path, 1024 * 1024).unwrap();
        (directory, document)
    }

    #[test]
    fn whole_word_uses_original_neighbors_on_both_viewport_edges() {
        let (_directory, document) = document("foobar foo");
        let before = document.selection();
        let options = MarkdownLiteralFindOptions {
            whole_word: true,
            ..MarkdownLiteralFindOptions::default()
        };
        for (query, range, count) in [("bar", 3..6, 0), ("foo", 0..3, 0), ("foo", 7..10, 1)] {
            // 窗口边缘不是词边界：左右真实邻接能拒绝截断单词，完整末词仍应命中。
            let found = visible_literal_matches(
                &document,
                query,
                range,
                options,
                MAX_SPECIAL_CHARACTER_MARKERS,
                || true,
            )
            .unwrap();
            assert_eq!(found.count, count);
            assert!(!found.source_limited);
            assert!(!found.count_limited);
        }
        assert_eq!(document.selection(), before);
        assert_eq!(document.generation(), 0); // 可见反馈不得推进选区、编辑或历史代次。
    }

    #[test]
    fn clipped_source_does_not_split_an_emoji_or_claim_a_complete_visible_total() {
        let source = format!("{}😀目标", "a".repeat(MAX_DECORATION_VIEWPORT_UNITS - 1));
        let (_directory, document) = document(&source);
        let found = visible_literal_matches(
            &document,
            "😀",
            0..document.utf16_len(),
            MarkdownLiteralFindOptions::default(),
            MAX_SPECIAL_CHARACTER_MARKERS,
            || true,
        )
        .unwrap();
        assert_eq!(found.count, 0);
        assert!(found.source_limited);
        assert!(!found.count_limited);
        assert!(found.status().contains("已检查部分"));
        assert_eq!(document.utf16_len(), MAX_DECORATION_VIEWPORT_UNITS + 3);
        assert_eq!(document.generation(), 0); // cap位于emoji中间时只缩小读域，不截断或修改原文。
    }

    #[test]
    fn dense_viewport_stops_at_the_real_feedback_limit_and_reports_at_least() {
        let (_directory, document) = document(&"x".repeat(MAX_SPECIAL_CHARACTER_MARKERS * 2));
        let found = visible_literal_matches(
            &document,
            "x",
            0..document.utf16_len(),
            MarkdownLiteralFindOptions::default(),
            MAX_SPECIAL_CHARACTER_MARKERS,
            || true,
        )
        .unwrap();
        assert_eq!(found.count, MAX_SPECIAL_CHARACTER_MARKERS);
        assert!(found.count_limited);
        assert!(!found.source_limited);
        assert!(found.status().contains("至少")); // 已早停就不能把128写成精确总数或全文匹配数。
        assert_eq!(document.generation(), 0);
    }

    #[test]
    fn cancelled_viewport_does_not_publish_a_partial_zero_result() {
        let (_directory, document) = document("中文😀目标");
        let result = visible_literal_matches(
            &document,
            "不存在",
            0..document.utf16_len(),
            MarkdownLiteralFindOptions::default(),
            MAX_SPECIAL_CHARACTER_MARKERS,
            || false,
        );
        assert!(matches!(
            result,
            Err(MarkdownDocumentBufferError::FindInterrupted)
        ));
        assert_eq!(document.generation(), 0); // 新输入撤票是取消，不能洗成一次完整的“没有匹配”。
    }

    #[test]
    fn same_scan_delivers_original_unicode_ranges_for_visible_glyph_geometry() {
        let (_directory, document) = document("前 x😀 x 后");
        let before = document.selection();
        let latin_ranges = [2..3, 6..7];
        let emoji_range = 3..5;
        for (query, ranges) in [
            ("x", latin_ranges.as_slice()),
            ("😀", std::slice::from_ref(&emoji_range)),
        ] {
            let found = visible_literal_matches(
                &document,
                query,
                0..document.utf16_len(),
                MarkdownLiteralFindOptions::default(),
                MAX_SPECIAL_CHARACTER_MARKERS,
                || true,
            )
            .unwrap();
            assert_eq!(found.ranges.as_slice(), ranges); // 几何必须消费Core交付的原UTF16坐标，不能把emoji当一个unit。
            assert_eq!(found.count, ranges.len());
        }
        assert_eq!(document.selection(), before);
        assert_eq!(document.generation(), 0);
    }
}
