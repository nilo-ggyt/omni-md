//! 输入变化撤旧反馈与像素；原settled刷新同次生成有限计数、源范围与真实字面高亮。

use super::*;
use crate::native_surface::source_decorations::{
    MAX_DECORATION_VIEWPORT_UNITS, MAX_SPECIAL_CHARACTER_MARKERS,
};
use omni_markdown_core::MarkdownDocumentBufferError;
use omni_markdown_core::markdown_find::{
    MarkdownLiteralFindOptions, visit_markdown_literal_matches_cancellable,
};

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
            return; // 原栏已释放，没有可接收这次输入的后端。
        };
        let Some(original_view) = bar.textkit_view.get() else {
            return; // 真正的 Paged 栏沿其原后端；不能为输入反馈临时装配 TextKit。
        };
        let Ok(view) = native_markdown_table_intent_step(&|| self.current(controller), || {
            original_view.load()
        }) else {
            return; // Weak 升级期间若已换笔记或关闭，不能使用随后取得的 Native claim。
        };
        let Some(view) = view else {
            return; // 原 view 已销毁，不从当前窗口寻找替代 view。
        };
        let Some(document) = self.document.upgrade() else {
            return;
        };
        let table_epoch = view.ivars().table_input_intent_epoch.get();
        let current = || {
            self.current(controller)
                && !view.ivars().textkit_graph_teardown.get()
                && view.ivars().folds.matches_document(&document)
                && view.ivars().table_input_intent_epoch.get() == table_epoch
        };
        if !current() {
            return; // 弱 view 活着仍不等于持有原文稿、输入或关闭前权限。
        }
        let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
        view.cancel_find_reveal_before_input(self.intent);
        // 输入已经换代，旧矩形必须同步撤权；这里只删有限几何并排队，不读取候选或布局正文。
        view.invalidate_find_highlights(&current);
        if self.visible && current() {
            let _ = native_markdown_table_intent_step(&current, || {
                view.schedule_visible_table_layout_update()
            });
        } // 隐藏栏只擦除旧像素；可见栏由原 executor 在稳定事务核真实 marked、Source 与 glyph。
    }

    /// 只有原Active图可提供实际viewport；无图、换笔记或旧队列均不产生替代搜索。
    pub(in crate::native_surface) fn run(&self, controller: &NativeMarkdownController) {
        if !self.current(controller) {
            return;
        }
        let Some(bar) = self.bar.upgrade() else {
            return; // 原栏释放后没有反馈接收者，也不能构造一个新栏。
        };
        let view = with_active_native_markdown_surface(|surface| {
            let editor = surface.editor.as_ref().ok_or("原查找文稿已关闭")?;
            if !self.current(controller)
                || !std::ptr::eq(&*surface.controller, controller)
                || !WeakMarkdownDocument::ptr_eq(&self.document, &Rc::downgrade(editor.document()))
                || !editor
                    .find_bar
                    .borrow()
                    .as_ref()
                    .is_some_and(|actual| Rc::ptr_eq(actual, &bar))
            {
                return Err("原查找反馈已撤销".into());
            }
            Ok(editor.backend.text_view.clone())
        });
        let Ok(view) = view else {
            return; // Borrowed或后继Doc不给旧请求重新签发view，清空后的状态保持待输入。
        };
        let _payload = PayloadUseGuard::enter(&view.ivars().payload_use_depth);
        if !self.current(controller) || view.ivars().textkit_graph_teardown.get() {
            return;
        }
        view.cancel_find_reveal_before_input(self.intent); // 仅回收原输入之前的旧目标，不动较新显式动作或模式。
        let table_epoch = view.ivars().table_input_intent_epoch.get();
        let current = || {
            self.current(controller)
                && !view.ivars().textkit_graph_teardown.get()
                && view.ivars().table_input_intent_epoch.get() == table_epoch
        };
        // 协议通知与先前已排队的布局可能交错；先核真实查询组合状态，不读取候选字符串。
        let Ok(editor) = native_markdown_table_intent_step(&current, || bar.query.currentEditor())
        else {
            return;
        };
        let committed = match editor.as_ref() {
            Some(editor) => editor.downcast_ref::<NSTextView>().is_some_and(|editor| {
                matches!(
                    native_markdown_table_intent_step(&current, || !editor.hasMarkedText()),
                    Ok(true)
                )
            }),
            None => true, // 查询已正常结束编辑，控件中的值是已提交查询。
        };
        drop(editor);
        if !current() {
            return; // 原生 claim 析构也可能交接；旧反馈不能借后继客户保留或删除几何。
        }
        if committed {
            view.invalidate_stale_find_highlights(controller, &current);
        } else {
            view.invalidate_find_highlights(&current); // 真实 marked 或未知输入上下文不能复用旧像素。
        }
        if !self.visible || !current() {
            return; // 隐藏栏只清实际旧像素，不扫描或重建高亮。
        }
        if self.refresh {
            // 这里只及时验证正则输入；literal的范围、计数与几何由同一次settled刷新消费。
            let syntax = (|| -> Result<(), String> {
                let regex = native_markdown_table_intent_step(&current, || bar.regex.state() != 0)
                    .map_err(|_| "原查询选项已变化")?;
                if regex && let Some(query) = read_visible_query(&bar, &current)? {
                    native_markdown_table_intent_step(&current, || query.compiled_regex())
                        .map_err(|_| "原正则输入已变化")??;
                    self.consume_status(&bar, &current, "");
                }
                Ok(())
            })();
            if let Err(error) = syntax {
                self.consume_status(&bar, &current, &error);
            }
        }
        if current() {
            // 替换输入也需按同查询恢复几何；pending标记为空，不能擅自发布计数或执行替换。
            let _ = native_markdown_table_intent_step(&current, || {
                view.schedule_visible_table_layout_update()
            });
        }
    }

    /// settled事务只扫描一次真实源范围；该次结果同时用于反馈和后续实际glyph几何。
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
pub(in crate::native_surface) struct NativeMarkdownFindHighlightPreparation {
    pub(in crate::native_surface) ranges: Vec<std::ops::Range<usize>>,
    pub(in crate::native_surface) batch: NativeMarkdownFindHighlightBatch,
}

/// 同一原装饰Vec拥有有限矩形和弱票；不持Doc、TextKit、查询文字或第二份布局。
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
            result.count_limited = result.count == MAX_SPECIAL_CHARACTER_MARKERS;
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
            let found = visible_literal_matches(&document, query, range, options, || true).unwrap();
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
