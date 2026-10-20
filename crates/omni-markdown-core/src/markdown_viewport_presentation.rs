//! Bounded Markdown presentation cache and public viewport/table contracts.

//! A transient UTF-8 viewport becomes nonoverlapping absolute UTF-16 style runs.
//! No text body, HTML/AST, or AppKit object is retained; native adapters map runs
//! to attributes while the piece tree remains the sole authoritative text model.

mod link_destination;
pub(crate) mod parser;
pub use link_destination::MarkdownLinkDestination;
mod prefixes;
// S2原型只在显式feature中提供已验证窗口和导航；默认产品不公开候选构造接口。
#[cfg(feature = "s2-table-window-prototype")]
pub mod table_window;
mod underline;

use parser::*;
#[cfg(feature = "s2-table-window-prototype")]
pub(crate) use prefixes::markdown_physical_line_is_active;

use std::ops::Range;
use std::rc::Rc;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use thiserror::Error;

pub const MARKDOWN_PRESENTATION_OVERSCAN_UTF16_UNITS: usize = 8 * 1_024;
const MAX_MARKDOWN_PRESENTATION_DECORATIONS: usize = 8 * 1_024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// User-facing choice between semantic inline preview and literal source styling.
pub enum MarkdownPresentationMode {
    #[default]
    InlinePreview,
    Source,
}

impl MarkdownPresentationMode {
    /// Stable ordering used by the native popup control. Keeping the mapping in
    /// the domain type prevents AppKit and the controller from inventing
    /// different mode values at the boundary.
    /// Provide the stable native-popup ordering for presentation modes.
    pub const fn popup_index(self) -> usize {
        match self {
            // Inline preview occupies the first popup item.
            Self::InlinePreview => 0,
            // Literal source occupies the second popup item.
            Self::Source => 1,
        }
    }

    /// Converts the bounded popup index back to a domain mode. Unknown values
    /// are rejected instead of silently selecting a future mode.
    /// Reject unknown popup indices rather than silently selecting a future mode.
    pub const fn from_popup_index(index: usize) -> Option<Self> {
        match index {
            // The first item selects inline preview.
            0 => Some(Self::InlinePreview),
            // The second item selects literal source.
            1 => Some(Self::Source),
            // Unknown native values cannot identify a supported presentation mode.
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
/// Fixed-width composable semantic roles consumed by native attribute adapters.
pub struct MarkdownPresentationStyles(u32);

impl MarkdownPresentationStyles {
    /// 内部封存记录按原固定32位样式保存；这不是额外的语义解析入口。
    pub(crate) const fn bits(self) -> u32 {
        self.0
    }

    /// 封存格式只恢复已声明的31个语义位；保留位被污染时拒绝原记录，不静默去掉未知样式。
    pub(crate) const fn from_bits(bits: u32) -> Option<Self> {
        if bits & (1 << 31) != 0 {
            None
        } else {
            Some(Self(bits))
        }
    }

    pub const HEADING_1: Self = Self(1 << 0);
    pub const HEADING_2: Self = Self(1 << 1);
    pub const HEADING_3: Self = Self(1 << 2);
    pub const HEADING_4: Self = Self(1 << 3);
    pub const HEADING_5: Self = Self(1 << 4);
    pub const HEADING_6: Self = Self(1 << 5);
    pub const STRONG: Self = Self(1 << 6);
    pub const EMPHASIS: Self = Self(1 << 7);
    pub const STRIKETHROUGH: Self = Self(1 << 8);
    pub const INLINE_CODE: Self = Self(1 << 9);
    pub const LINK: Self = Self(1 << 10);
    pub const BLOCK_QUOTE: Self = Self(1 << 11);
    pub const UNORDERED_LIST: Self = Self(1 << 12);
    pub const ORDERED_LIST: Self = Self(1 << 13);
    pub const TASK_LIST: Self = Self(1 << 14);
    pub const HORIZONTAL_RULE: Self = Self(1 << 15);
    pub const FENCED_CODE: Self = Self(1 << 16);
    pub const TABLE: Self = Self(1 << 17);
    pub const FORMULA: Self = Self(1 << 18);
    pub const SUPERSCRIPT: Self = Self(1 << 19);
    pub const UNDERLINE: Self = Self(1 << 20);
    pub const MARKER: Self = Self(1 << 21);
    pub const HIDDEN_MARKER: Self = Self(1 << 22);
    pub const TABLE_HEADER: Self = Self(1 << 23);
    pub const TABLE_DELIMITER: Self = Self(1 << 24);
    pub const TABLE_BOUNDARY: Self = Self(1 << 25);
    pub const TABLE_ALIGN_LEFT: Self = Self(1 << 26);
    pub const TABLE_ALIGN_CENTER: Self = Self(1 << 27);
    pub const TABLE_ALIGN_RIGHT: Self = Self(1 << 28);
    /// A native table overlays this preview row; TextKit retains a fixed-height placeholder.
    pub const TABLE_ROW_PLACEHOLDER: Self = Self(1 << 29);
    /// Parser确认的单波浪号下标；与双波浪号删除线独立组合，不改变源UTF16坐标。
    pub const SUBSCRIPT: Self = Self(1 << 30);

    #[must_use]
    /// Require all requested style bits to be present in this style set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    /// Combine independent presentation roles without changing either input set.
    pub(crate) const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    /// Remove requested roles while preserving unrelated presentation bits.
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    #[must_use]
    /// Detect an unstyled source interval.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Iterate the fixed-width active style indices used by the transition sweep.
    fn set_bits(self) -> impl Iterator<Item = usize> {
        (0..u32::BITS as usize).filter(move |bit| {
            // Only active bits participate in the fixed-width style-transition sweep.
            self.0 & (1_u32 << bit) != 0
        })
    }

    /// Collapse nested style occurrence counts into the currently active style set.
    fn from_presence_counts(counts: &[u16; u32::BITS as usize]) -> Self {
        let mut bits = 0_u32;
        for (bit, count) in counts.iter().enumerate() {
            // Inspect the fixed 32-bit style domain; counts reflect nesting rather than source size.
            if *count != 0 {
                // A nonzero occurrence count keeps that style active even if an inner span has ended.
                bits |= 1_u32 << bit;
            }
        }
        Self(bits)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// One disjoint styled interval in absolute document UTF-16 coordinates.
pub struct MarkdownPresentationRun {
    pub utf16_range: Range<usize>,
    pub styles: MarkdownPresentationStyles,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Exact styled or unstyled interval safe to reuse for one native attribute query.
pub struct MarkdownPresentationQuery {
    pub utf16_range: Range<usize>,
    pub styles: MarkdownPresentationStyles,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// 原 Parser 已解析的可见链接；目标经过原 escape/entity 规则，范围仍属于 canonical UTF16。
pub struct MarkdownSourceLink {
    pub label_utf16_range: Range<usize>,
    pub destination: MarkdownLinkDestination,
}

/// Parser确认的无序列表原字符；glyph消费者不能据此改写正文。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MarkdownBulletMarker {
    Dash,
    Plus,
    Star,
}

/// 预览列表标记的源角色；TaskBox保留完整三字符范围，Native只替换首字符。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MarkdownPreviewListMarkerKind {
    Bullet(MarkdownBulletMarker),
    TaskBox {
        checked: bool,
    },
    TaskBullet {
        source: MarkdownBulletMarker,
        checked: bool,
    },
}

/// 有限视口内的真实item标记；只有绝对UTF16范围与值，不持有文本或Parser节点。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownPreviewListMarker {
    pub utf16_range: Range<usize>,
    pub owner_utf16_range: Range<usize>,
    pub kind: MarkdownPreviewListMarkerKind,
    pub active: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Platform-neutral horizontal cell alignment.
pub enum MarkdownTableAlignment {
    Left,
    Center,
    Right,
}

/// Immutable semantic prototype for one cell inside the current bounded viewport.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownTableCellPrototype {
    pub utf16_range: Range<usize>,
    pub column_index: usize,
    pub alignment: MarkdownTableAlignment,
}

/// Row metadata indexes a flat cell array rather than allocating one vector per row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownTableRowPrototype {
    pub utf16_range: Range<usize>,
    pub cell_index_range: Range<usize>,
    pub is_header: bool,
}

/// Native-consumable table prototype without document text or platform ownership.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownTablePrototype {
    pub utf16_range: Range<usize>,
    pub header_utf16_range: Range<usize>,
    pub delimiter_utf16_range: Range<usize>,
    pub rows: Vec<MarkdownTableRowPrototype>,
    pub cells: Vec<MarkdownTableCellPrototype>,
}

#[derive(Debug, Error, Eq, PartialEq)]
/// Bounded projection failures that require literal source presentation instead of partial semantics.
pub enum MarkdownViewportPresentationError {
    #[error("Markdown 视口范围与 UTF-16 文本长度不一致")]
    InvalidUtf16Window,
    #[error("Markdown parser 返回了无效的 UTF-8 边界")]
    InvalidParserBoundary,
    #[error("Markdown 视口语义超过有界 span 上限")]
    DecorationLimitExceeded,
}

/// 当前有界窗口的控制状态与一份完整解析发布；正文只在 rebuild 内临时借用。
/// 编辑、历史和选择同步撤销当前发布，在途 Native 快照仍保持其原不可变值。
pub struct MarkdownViewportPresentation {
    mode: MarkdownPresentationMode,
    generation: Option<u64>,
    active_selection: Range<usize>,
    utf16_window: Option<Range<usize>>,
    // 一次成功解析的三个目录整体发布；Native 借出的旧批次不会被后续属性查询原地改写。
    publication: Option<Rc<MarkdownPresentationBuild>>,
}

#[derive(Clone)]
/// 一次完整有界解析的不可变 metadata。Clone 只共享原目录，不复制表格、样式或正文。
/// 此类型不持文档；消费者仍须绑定并验证原文档、选择及模式，不能仅凭 generation 获得源权限。
pub struct MarkdownViewportPresentationSnapshot {
    generation: u64,
    utf16_window: Range<usize>,
    publication: Rc<MarkdownPresentationBuild>,
}

impl MarkdownViewportPresentationSnapshot {
    /// 原解析代次只用于与消费者保存的源票核对，不代表文档仍然存活。
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// 原发布的精确有界范围；后继缓存不能扩大本快照的查询权限。
    pub fn utf16_window(&self) -> &Range<usize> {
        &self.utf16_window
    }

    /// 直接借用原表目录，不把窗口快照变成另一份整表副本。
    pub fn tables(&self) -> &[MarkdownTablePrototype] {
        &self.publication.tables
    }

    /// 样式与表目录来自同一次完整 build；覆盖外明确无结果。
    pub fn query(&self, utf16_index: usize) -> Option<MarkdownPresentationQuery> {
        query_published_runs(&self.publication.runs, &self.utf16_window, utf16_index)
    }
}

impl Default for MarkdownViewportPresentation {
    /// Create an empty inline-preview cache without source text or native object ownership.
    fn default() -> Self {
        Self {
            mode: MarkdownPresentationMode::InlinePreview,
            generation: None,
            active_selection: 0..0,
            utf16_window: None,
            publication: None,
        }
    }
}

impl MarkdownViewportPresentation {
    /// 检查重置缓存没有保留任何 run/table 容量或旧文档窗口。
    #[must_use]
    pub fn is_unallocated(&self) -> bool {
        self.generation.is_none()
            && self.utf16_window.is_none()
            && self.publication.is_none()
            && self.active_selection == (0..0)
    }

    #[must_use]
    /// Read the current presentation preference without rebuilding semantic metadata.
    pub fn mode(&self) -> MarkdownPresentationMode {
        self.mode
    }

    /// Invalidate bounded attributes only when the presentation preference changes.
    pub fn set_mode(&mut self, mode: MarkdownPresentationMode) -> Option<Range<usize>> {
        if self.mode == mode {
            // Reselecting the active mode preserves the existing cache and needs no native invalidation.
            return None;
        }
        self.mode = mode;
        self.invalidate()
    }

    /// 记录归一化选区；只有预览的标记可见性依赖选区，源码模式保留原有有限覆盖。
    pub fn set_active_selection(&mut self, selection: Range<usize>) -> Option<Range<usize>> {
        let normalized = selection.start.min(selection.end)..selection.start.max(selection.end);
        if self.active_selection == normalized {
            // 方向变化但范围相同不改变语法显隐；canonical 的方向由文档选区另行管理。
            return None;
        }
        self.active_selection = normalized;
        if self.mode == MarkdownPresentationMode::Source {
            // 源码始终显示原字符，不因查找或拖选发送属性重建通知；仍记录最新选区，
            // 下次 set_mode 切回预览会先撤销旧覆盖，再用该选区决定可编辑标记。
            return None;
        }
        // 预览仍须撤销旧选区对应的隐藏标记，不能把源码的优化推广到预览模式。
        self.invalidate()
    }

    #[must_use]
    /// Require a rebuild for a changed document generation or a position outside the cached window.
    pub fn needs_rebuild(&self, generation: u64, utf16_index: usize) -> bool {
        self.generation != Some(generation)
            || self.cached_utf16_window().is_none_or(|window| {
                // A coordinate outside cached half-open coverage requires fresh bounded presentation.
                utf16_index < window.start || utf16_index >= window.end
            })
    }

    /// Validate and atomically replace one bounded semantic cache after successful parsing.
    pub fn rebuild(
        &mut self,
        generation: u64,
        utf16_window: Range<usize>,
        viewport_text: &str,
    ) -> Result<(), MarkdownViewportPresentationError> {
        if utf16_window.start > utf16_window.end
            || viewport_text.encode_utf16().count() != utf16_window.end - utf16_window.start
        {
            // The supplied text must exactly represent the declared UTF-16 window before any cache mutation.
            return Err(MarkdownViewportPresentationError::InvalidUtf16Window);
        }

        let build = if self.mode == MarkdownPresentationMode::Source || viewport_text.is_empty() {
            // Source mode and empty text require no Markdown parser or semantic allocations.
            MarkdownPresentationBuild::default()
        } else {
            // Inline preview parses only the complete bounded source supplied by the document layer.
            build_markdown_presentation(viewport_text, utf16_window.start, &self.active_selection)?
        };
        // Publish only validated complete coverage; replacing vectors releases stale semantic ownership.
        self.generation = Some(generation);
        self.utf16_window = Some(utf16_window);
        self.publication = if self.mode == MarkdownPresentationMode::InlinePreview {
            // 三个目录 move 到同一 Rc；旧 Native 批次仍读原发布，新缓存不深复制旧目录。
            Some(Rc::new(build))
        } else {
            None // 源码窗口只有范围，不为不存在的解析目录分配空 Rc。
        };
        Ok(())
    }

    /// Record the bounded source-mode attribute window used by native layout.
    ///
    /// Source mode needs no parser/body materialization. Retaining its last range
    /// allows a later preview switch to invalidate only that range, not all text.
    /// Record native source-mode coverage without parsing or retaining a document body.
    pub fn record_source_window(
        &mut self,
        generation: u64,
        utf16_window: Range<usize>,
    ) -> Result<(), MarkdownViewportPresentationError> {
        if self.mode != MarkdownPresentationMode::Source || utf16_window.start > utf16_window.end {
            // Only a valid source-mode window can be recorded through this path.
            return Err(MarkdownViewportPresentationError::InvalidUtf16Window);
        }
        // Publish only validated complete coverage; replacing vectors releases stale semantic ownership.
        self.generation = Some(generation);
        self.utf16_window = Some(utf16_window);
        self.publication = None;
        Ok(())
    }

    /// Record bounded source fallback when a complete Markdown block exceeds its budget.
    /// Record explicit inline-preview fallback coverage when a complete block exceeds its parse budget.
    pub fn record_source_fallback_window(
        &mut self,
        generation: u64,
        utf16_window: Range<usize>,
    ) -> Result<(), MarkdownViewportPresentationError> {
        if self.mode != MarkdownPresentationMode::InlinePreview
            || utf16_window.start > utf16_window.end
        {
            // Fallback is meaningful only for a valid inline-preview window.
            return Err(MarkdownViewportPresentationError::InvalidUtf16Window);
        }
        // Publish only validated complete coverage; replacing vectors releases stale semantic ownership.
        self.generation = Some(generation);
        self.utf16_window = Some(utf16_window);
        self.publication = None;
        Ok(())
    }

    /// Drop stale semantic runs after a generation change and return only the last
    /// bounded attribute range requiring invalidation, without materializing text.
    /// Drop stale semantics and return the previous bounded attribute interval for native invalidation.
    pub fn invalidate_document_generation(&mut self) -> Option<Range<usize>> {
        self.invalidate()
    }

    #[must_use]
    /// Find the containing styled run or exact unstyled gap inside a current cached window.
    pub fn query(&self, utf16_index: usize) -> Option<MarkdownPresentationQuery> {
        self.generation?;
        let utf16_window = self.utf16_window.as_ref()?;
        query_published_runs(self.runs(), utf16_window, utf16_index)
    }

    #[must_use]
    /// Expose cached coverage only while its generation remains valid.
    pub fn cached_utf16_window(&self) -> Option<Range<usize>> {
        self.generation.and_then(|_| {
            // Expose only the small owned coverage range after a valid cache generation has been established.
            self.utf16_window.as_ref().cloned()
        })
    }

    #[must_use]
    /// Borrow current immutable table metadata without transferring source or native ownership.
    pub fn tables(&self) -> &[MarkdownTablePrototype] {
        self.publication
            .as_ref()
            .map_or(&[], |publication| &publication.tables)
    }

    /// Borrow current bounded style runs for transient native visible-cell rendering.
    #[must_use]
    /// Borrow current bounded runs for transient native visible-cell rendering.
    pub fn runs(&self) -> &[MarkdownPresentationRun] {
        self.publication
            .as_ref()
            .map_or(&[], |publication| &publication.runs)
    }

    /// 只有真实完整解析才可借出快照；源码和显式 fallback 的空 runs 不授予完整语义权限。
    #[must_use]
    pub fn semantic_snapshot(&self) -> Option<MarkdownViewportPresentationSnapshot> {
        if self.mode != MarkdownPresentationMode::InlinePreview {
            // 源码模式记录的覆盖用于属性通知，不能被表格布局当作预览解析完成。
            return None;
        }
        Some(MarkdownViewportPresentationSnapshot {
            generation: self.generation?,
            utf16_window: self.utf16_window.as_ref()?.clone(),
            publication: Rc::clone(self.publication.as_ref()?),
        })
    }

    /// 内部列表查询与公开 runs/tables 共享原发布，不保留独立的可变列表目录。
    fn list_markers(&self) -> &[MarkdownPreviewListMarker] {
        self.publication
            .as_ref()
            .map_or(&[], |publication| &publication.list_markers)
    }

    /// 在当前有效窗口二分借用单个真实列表token；失效或源码模式不提供glyph权限。
    #[must_use]
    pub fn list_marker_at(&self, utf16_index: usize) -> Option<&MarkdownPreviewListMarker> {
        self.generation?;
        let marker = self
            .list_markers()
            .get(self.list_markers().partition_point(|marker| {
                // 已结束的token不能覆盖后续空白或正文。
                marker.utf16_range.end <= utf16_index
            }))?;
        (marker.utf16_range.start <= utf16_index).then_some(marker)
    }

    /// 返回token或相邻token间隙的精确边界，防止Native属性缓存从普通run跨进glyph角色。
    #[must_use]
    pub fn list_marker_boundary_range_at(&self, utf16_index: usize) -> Option<Range<usize>> {
        let window = self.cached_utf16_window()?;
        if utf16_index < window.start || utf16_index >= window.end {
            // 窗外位置不能借用此批属性或列表元数据。
            return None;
        }
        let index = self
            .list_markers()
            .partition_point(|marker| marker.utf16_range.end <= utf16_index);
        if let Some(marker) = self.list_markers().get(index)
            && marker.utf16_range.start <= utf16_index
        {
            // token内部只允许复用该原范围，TaskBox内部单unit细分由Native继续收窄。
            return Some(marker.utf16_range.clone());
        }
        // 间隙也必须停在下一个token之前，不能沿较大的语义run吞掉它。
        let start = index.checked_sub(1).map_or(window.start, |previous| {
            self.list_markers()[previous].utf16_range.end
        });
        let end = self
            .list_markers()
            .get(index)
            .map_or(window.end, |marker| marker.utf16_range.start);
        Some(start..end)
    }

    /// Release semantic vectors while retaining the last bounded window for repeated native invalidations.
    fn invalidate(&mut self) -> Option<Range<usize>> {
        // Keep the last window through rapid selection/mode invalidations even
        // after generation is cleared; query rejects stale runs using that generation.
        let invalidated = self.utf16_window.as_ref().cloned();
        self.generation = None;
        // 只撤销当前缓存的强边；在途快照的不可变值由其原调用栈自行结束，不强制清空旧值。
        self.publication = None;
        invalidated
    }
}

/// 当前缓存和 Native 已借出的快照复用同一个纯查询，返回范围始终受原发布覆盖限制。
pub(crate) fn query_published_runs(
    runs: &[MarkdownPresentationRun],
    window: &Range<usize>,
    utf16_index: usize,
) -> Option<MarkdownPresentationQuery> {
    if !window.contains(&utf16_index) {
        // 覆盖外没有默认样式权限，不能借邻接 run 或空目录伪造已完成解析。
        return None;
    }
    let candidate = runs.partition_point(|run| run.utf16_range.end <= utf16_index);
    if let Some(run) = runs.get(candidate)
        && run.utf16_range.start <= utf16_index
    {
        // 一个真实 run 只返回原窗口内交集；不向旧发布未覆盖的正文延伸。
        return Some(MarkdownPresentationQuery {
            utf16_range: run.utf16_range.start.max(window.start)
                ..run.utf16_range.end.min(window.end),
            styles: run.styles,
        });
    }
    // 未命中样式的字符仍取得准确 gap；两端由同一个不可变目录决定。
    let start = candidate.checked_sub(1).map_or(window.start, |previous| {
        runs[previous].utf16_range.end.max(window.start)
    });
    let end = runs
        .get(candidate)
        .map_or(window.end, |run| run.utf16_range.start.min(window.end));
    Some(MarkdownPresentationQuery {
        utf16_range: start..end,
        styles: MarkdownPresentationStyles::default(),
    })
}

#[cfg(test)]
mod tests;
